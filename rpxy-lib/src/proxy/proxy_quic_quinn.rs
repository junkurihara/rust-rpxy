use super::{proxy_main::Proxy, socket::bind_udp_socket};
use crate::{error::*, log::*, name_exp::ByteName};
use hyper_util::client::legacy::connect::Connect;
use quinn::{
  Endpoint, TransportConfig,
  crypto::rustls::{HandshakeData, QuicServerConfig},
};
use rpxy_certs::ServerCrypto;
use rustls::ServerConfig;
use std::sync::Arc;

impl<T> Proxy<T>
where
  T: Send + Sync + Connect + Clone + 'static,
{
  pub(super) async fn h3_listener_service(&self) -> RpxyResult<()> {
    let Some(mut server_crypto_rx) = self.globals.cert_reloader_rx.clone() else {
      return Err(RpxyError::NoCertificateReloader);
    };
    info!("Start UDP proxy serving with HTTP/3 request for configured host names [quinn]");
    // first set as null config server
    // AWS LC provider by default
    let provider = rustls::crypto::CryptoProvider::get_default().ok_or(RpxyError::NoDefaultCryptoProvider)?;
    let rustls_server_config = ServerConfig::builder_with_provider(provider.clone())
      .with_protocol_versions(&[&rustls::version::TLS13])
      .map_err(|e| RpxyError::FailedToBuildServerConfig(format!("TLS 1.3 server config failed: {e}")))?
      .with_no_client_auth()
      .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));
    let quinn_server_config_crypto = QuicServerConfig::try_from(Arc::new(rustls_server_config)).unwrap();

    let mut transport_config_quic = TransportConfig::default();
    transport_config_quic
      .max_concurrent_bidi_streams(self.globals.proxy_config.h3_max_concurrent_bidistream.into())
      .max_concurrent_uni_streams(self.globals.proxy_config.h3_max_concurrent_unistream.into())
      .max_idle_timeout(
        self
          .globals
          .proxy_config
          .h3_max_idle_timeout
          .map(|v| quinn::IdleTimeout::try_from(v).unwrap()),
      );

    let mut server_config_h3 = quinn::ServerConfig::with_crypto(Arc::new(quinn_server_config_crypto));
    server_config_h3.transport = Arc::new(transport_config_quic);
    server_config_h3.max_incoming(self.globals.proxy_config.h3_max_concurrent_connections as usize);

    // To reuse address
    let udp_socket = bind_udp_socket(&self.listening_on)?;
    let runtime =
      quinn::default_runtime().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "No async runtime found"))?;
    let endpoint = Endpoint::new(quinn::EndpointConfig::default(), Some(server_config_h3), udp_socket, runtime)?;

    let mut server_crypto: Option<Arc<ServerCrypto>> = None;
    loop {
      tokio::select! {
        new_conn = endpoint.accept() => {
          if server_crypto.is_none() || new_conn.is_none() {
            continue;
          }
          let Ok(mut incoming) = new_conn.unwrap().accept() else {
            continue
          };
          let Ok(hsd) = incoming.handshake_data().await else {
            continue
          };

          let Ok(hsd_downcast) = hsd.downcast::<HandshakeData>() else {
            continue
          };
          let Some(new_server_name) = hsd_downcast.server_name else {
            warn!("HTTP/3 no SNI is given");
            continue;
          };
          debug!(
            "HTTP/3 connection incoming (SNI {:?})",
            new_server_name
          );
          // TODO: server_nameをここで出してどんどん深く投げていくのは効率が悪い。connecting -> connectionsの後でいいのでは？
          // TODO: 通常のTLSと同じenumか何かにまとめたい
          let self_clone = self.clone();
          self.globals.runtime_handle.spawn(async move {
            let client_addr = incoming.remote_address();
            let quic_connection = match incoming.await {
              Ok(new_conn) => {
                trace!("New connection established");
                h3_quinn::Connection::new(new_conn)
              },
              Err(e) => {
                warn!("QUIC accepting connection failed: {:?}", e);
                return Err(RpxyError::QuinnConnectionFailed(e));
              }
            };
            // Timeout is based on underlying quic
            if let Err(e) = self_clone.h3_serve_connection(quic_connection, new_server_name.to_server_name(), client_addr).await {
              warn!("QUIC or HTTP/3 connection failed: {}", e);
            };
            Ok(())
          });
        }
        _ = server_crypto_rx.changed() => {
          if server_crypto_rx.borrow().is_none() {
            error!("Reloader is broken");
            break;
          }
          let cert_keys_map = server_crypto_rx.get().unwrap();

          server_crypto = (&cert_keys_map).try_into().ok();
          let Some(inner) = server_crypto.clone() else {
            error!("Failed to update server crypto for h3");
            break;
          };
          let rustls_server_config = inner.aggregated_config_no_client_auth.clone();
          let Ok(quinn_server_config_crypto) = QuicServerConfig::try_from(rustls_server_config) else {
            error!("Failed to update server crypto for h3");
            break;
          };
          endpoint.set_server_config(Some(quinn::ServerConfig::with_crypto(Arc::new(quinn_server_config_crypto))));
        }
        else => break
      }
    }
    endpoint.wait_idle().await;
    Ok(()) as RpxyResult<()>
  }
}

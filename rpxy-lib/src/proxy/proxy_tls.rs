use super::{
  crypto_service::{CryptoReloader, ServerCrypto, ServerCryptoBase, SniServerCryptoMap},
  proxy_main::{LocalExecutor, Proxy},
};
use crate::{certs::CryptoSource, constants::*, error::*, log::*, utils::BytesName};
use hot_reload::{ReloaderReceiver, ReloaderService};
use hyper::{client::connect::Connect, server::conn::Http};
#[cfg(feature = "http3")]
use quinn::{crypto::rustls::HandshakeData, Endpoint, ServerConfig as QuicServerConfig, TransportConfig};
#[cfg(feature = "http3")]
use rustls::ServerConfig;
use std::sync::Arc;
use tokio::{
  net::TcpListener,
  time::{timeout, Duration},
};

impl<T, U> Proxy<T, U>
where
  T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone + Sync + Send + 'static,
{
  // TCP Listener Service, i.e., http/2 and http/1.1
  async fn listener_service(
    &self,
    server: Http<LocalExecutor>,
    mut server_crypto_rx: ReloaderReceiver<ServerCryptoBase>,
  ) -> Result<()> {
    let tcp_listener = TcpListener::bind(&self.listening_on).await?;
    info!("Start TCP proxy serving with HTTPS request for configured host names");

    let mut server_crypto_map: Option<Arc<SniServerCryptoMap>> = None;
    loop {
      tokio::select! {
        tcp_cnx = tcp_listener.accept() => {
          if tcp_cnx.is_err() || server_crypto_map.is_none() {
            continue;
          }
          let (raw_stream, client_addr) = tcp_cnx.unwrap();
          let sc_map_inner = server_crypto_map.clone();
          let server_clone = server.clone();
          let self_inner = self.clone();

          // spawns async handshake to avoid blocking thread by sequential handshake.
          let handshake_fut = async move {
            let acceptor = tokio_rustls::LazyConfigAcceptor::new(tokio_rustls::rustls::server::Acceptor::default(), raw_stream).await;
            if let Err(e) = acceptor {
              return Err(RpxyError::Proxy(format!("Failed to handshake TLS: {e}")));
            }
            let start = acceptor.unwrap();
            let client_hello = start.client_hello();
            let server_name = client_hello.server_name();
            debug!("HTTP/2 or 1.1: SNI in ClientHello: {:?}", server_name);
            let server_name = server_name.map_or_else(|| None, |v| Some(v.to_server_name_vec()));
            if server_name.is_none(){
              return Err(RpxyError::Proxy("No SNI is given".to_string()));
            }
            let server_crypto = sc_map_inner.as_ref().unwrap().get(server_name.as_ref().unwrap());
            if server_crypto.is_none() {
              return Err(RpxyError::Proxy(format!("No TLS serving app for {:?}", "xx")));
            }
            let stream = match start.into_stream(server_crypto.unwrap().clone()).await {
              Ok(s) => s,
              Err(e) => {
                return Err(RpxyError::Proxy(format!("Failed to handshake TLS: {e}")));
              }
            };
            self_inner.client_serve(stream, server_clone, client_addr, server_name);
            Ok(())
          };

          self.globals.runtime_handle.spawn( async move {
            // timeout is introduced to avoid get stuck here.
            match timeout(
              Duration::from_secs(TLS_HANDSHAKE_TIMEOUT_SEC),
              handshake_fut
            ).await {
              Ok(a) => {
                if let Err(e) = a {
                  error!("{}", e);
                }
              },
              Err(e) => {
                error!("Timeout to handshake TLS: {}", e);
              }
            };
          });
        }
        _ = server_crypto_rx.changed() => {
          if server_crypto_rx.borrow().is_none() {
            error!("Reloader is broken");
            break;
          }
          let cert_keys_map = server_crypto_rx.borrow().clone().unwrap();
          let Some(server_crypto): Option<Arc<ServerCrypto>> = (&cert_keys_map).try_into().ok() else {
            error!("Failed to update server crypto");
            break;
          };
          server_crypto_map = Some(server_crypto.inner_local_map.clone());
        }
        else => break
      }
    }
    Ok(()) as Result<()>
  }

  #[cfg(feature = "http3")]
  async fn listener_service_h3(&self, mut server_crypto_rx: ReloaderReceiver<ServerCryptoBase>) -> Result<()> {
    info!("Start UDP proxy serving with HTTP/3 request for configured host names");
    // first set as null config server
    let rustls_server_config = ServerConfig::builder()
      .with_safe_default_cipher_suites()
      .with_safe_default_kx_groups()
      .with_protocol_versions(&[&rustls::version::TLS13])?
      .with_no_client_auth()
      .with_cert_resolver(Arc::new(rustls::server::ResolvesServerCertUsingSni::new()));

    let mut transport_config_quic = TransportConfig::default();
    transport_config_quic
      .max_concurrent_bidi_streams(self.globals.proxy_config.h3_max_concurrent_bidistream)
      .max_concurrent_uni_streams(self.globals.proxy_config.h3_max_concurrent_unistream)
      .max_idle_timeout(self.globals.proxy_config.h3_max_idle_timeout);

    let mut server_config_h3 = QuicServerConfig::with_crypto(Arc::new(rustls_server_config));
    server_config_h3.transport = Arc::new(transport_config_quic);
    server_config_h3.concurrent_connections(self.globals.proxy_config.h3_max_concurrent_connections);
    let endpoint = Endpoint::server(server_config_h3, self.listening_on)?;

    let mut server_crypto: Option<Arc<ServerCrypto>> = None;
    loop {
      tokio::select! {
        new_conn = endpoint.accept() => {
          if server_crypto.is_none() || new_conn.is_none() {
            continue;
          }
          let mut conn: quinn::Connecting = new_conn.unwrap();
          let Ok(hsd) = conn.handshake_data().await else {
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
          let fut = self.clone().connection_serve_h3(conn, new_server_name.to_server_name_vec());
          self.globals.runtime_handle.spawn(async move {
            // Timeout is based on underlying quic
            if let Err(e) = fut.await {
              warn!("QUIC or HTTP/3 connection failed: {}", e)
            }
          });
        }
        _ = server_crypto_rx.changed() => {
          if server_crypto_rx.borrow().is_none() {
            error!("Reloader is broken");
            break;
          }
          let cert_keys_map = server_crypto_rx.borrow().clone().unwrap();

          server_crypto = (&cert_keys_map).try_into().ok();
          let Some(inner) = server_crypto.clone() else {
            error!("Failed to update server crypto for h3");
            break;
          };
          endpoint.set_server_config(Some(QuicServerConfig::with_crypto(inner.clone().inner_global_no_client_auth.clone())));

        }
        else => break
      }
    }
    endpoint.wait_idle().await;
    Ok(()) as Result<()>
  }

  pub async fn start_with_tls(self, server: Http<LocalExecutor>) -> Result<()> {
    let (cert_reloader_service, cert_reloader_rx) = ReloaderService::<CryptoReloader<U>, ServerCryptoBase>::new(
      &self.globals.clone(),
      CERTS_WATCH_DELAY_SECS,
      !LOAD_CERTS_ONLY_WHEN_UPDATED,
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;

    #[cfg(not(feature = "http3"))]
    {
      tokio::select! {
        _= self.cert_service(tx) => {
          error!("Cert service for TLS exited");
        },
        _ = self.listener_service(server, rx) => {
          error!("TCP proxy service for TLS exited");
        },
        else => {
          error!("Something went wrong");
          return Ok(())
        }
      };
      Ok(())
    }
    #[cfg(feature = "http3")]
    {
      if self.globals.proxy_config.http3 {
        tokio::select! {
          _= cert_reloader_service.start() => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(server, cert_reloader_rx.clone()) => {
            error!("TCP proxy service for TLS exited");
          },
          _= self.listener_service_h3(cert_reloader_rx) => {
            error!("UDP proxy service for QUIC exited");
          },
          else => {
            error!("Something went wrong");
            return Ok(())
          }
        };
        Ok(())
      } else {
        tokio::select! {
          _= cert_reloader_service.start() => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(server, cert_reloader_rx) => {
            error!("TCP proxy service for TLS exited");
          },
          else => {
            error!("Something went wrong");
            return Ok(())
          }
        };
        Ok(())
      }
    }
  }
}

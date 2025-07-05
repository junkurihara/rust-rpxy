use super::proxy_main::Proxy;
use crate::{error::*, log::*, name_exp::ByteName};
use anyhow::anyhow;
use hot_reload::ReloaderReceiver;
use hyper_util::client::legacy::connect::Connect;
use rpxy_certs::{ServerCrypto, ServerCryptoBase};
use s2n_quic::provider;
use std::sync::Arc;

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  /// Start UDP proxy serving with HTTP/3 request for configured host names
  pub(super) async fn h3_listener_service(&self) -> RpxyResult<()> {
    let Some(mut server_crypto_rx) = self.globals.cert_reloader_rx.clone() else {
      return Err(RpxyError::NoCertificateReloader);
    };
    info!("Start UDP proxy serving with HTTP/3 request for configured host names [s2n-quic]");

    // initially wait for receipt
    let mut server_crypto: Option<s2n_quic_rustls::Server> = {
      let _ = server_crypto_rx.changed().await;
      let sc = self.receive_server_crypto(server_crypto_rx.clone())?;
      Some(sc)
    };

    // event loop
    loop {
      tokio::select! {
        v = self.h3_listener_service_inner(&server_crypto) => {
          if let Err(e) = v {
            error!("Quic connection event loop illegally shutdown [s2n-quic] {e}");
            break;
          }
        }
        _ = server_crypto_rx.changed() => {
          server_crypto = match self.receive_server_crypto(server_crypto_rx.clone()) {
            Ok(sc) => Some(sc),
            Err(e) => {
              error!("{e}");
              break;
            }
          };
        }
        else => break
      }
    }

    Ok(())
  }

  /// Receive server crypto from reloader
  fn receive_server_crypto(&self, server_crypto_rx: ReloaderReceiver<ServerCryptoBase>) -> RpxyResult<s2n_quic_rustls::Server> {
    let cert_keys_map = server_crypto_rx.get().ok_or_else(|| {
      error!("Reloader is broken");
      RpxyError::CertificateReloadError(anyhow!("Reloader is broken").into())
    })?;

    let server_crypto: Option<s2n_quic_rustls::Server> = (&cert_keys_map).try_into().ok().and_then(|v: Arc<ServerCrypto>| {
      let rustls_server_config = v.aggregated_config_no_client_auth.clone();
      let resolver = rustls_server_config.cert_resolver.clone();
      let alpn = rustls_server_config.alpn_protocols.clone();
      #[allow(deprecated)]
      let tls = provider::tls::rustls::server::Builder::default()
        .with_cert_resolver(resolver)
        .and_then(|t| t.with_application_protocols(alpn.iter()))
        .and_then(|t| t.build())
        .ok();
      tls
    });
    server_crypto.ok_or_else(|| {
      error!("Failed to update server crypto for h3 [s2n-quic]");
      RpxyError::FailedToUpdateServerCrypto("Failed to update server crypto for h3 [s2n-quic]".to_string())
    })
  }

  /// Event loop for UDP proxy serving with HTTP/3 request for configured host names
  async fn h3_listener_service_inner(&self, server_crypto: &Option<s2n_quic_rustls::Server>) -> RpxyResult<()> {
    // setup UDP socket
    let io = provider::io::tokio::Builder::default()
      .with_receive_address(self.listening_on)?
      .with_reuse_port()?
      .build()?;

    // setup limits
    let mut limits = provider::limits::Limits::default()
      .with_max_open_local_bidirectional_streams(self.globals.proxy_config.h3_max_concurrent_bidistream as u64)?
      .with_max_open_remote_bidirectional_streams(self.globals.proxy_config.h3_max_concurrent_bidistream as u64)?
      .with_max_open_local_unidirectional_streams(self.globals.proxy_config.h3_max_concurrent_unistream as u64)?
      .with_max_open_remote_unidirectional_streams(self.globals.proxy_config.h3_max_concurrent_unistream as u64)?
      .with_max_active_connection_ids(self.globals.proxy_config.h3_max_concurrent_connections as u64)?;
    limits = if let Some(v) = self.globals.proxy_config.h3_max_idle_timeout {
      limits.with_max_idle_timeout(v)?
    } else {
      limits
    };

    // setup tls
    let Some(server_crypto) = server_crypto else {
      warn!("No server crypto is given [s2n-quic]");
      return Err(RpxyError::NoServerCrypto("No server crypto is given [s2n-quic]".to_string()));
    };

    let mut server = s2n_quic::Server::builder()
      .with_tls(server_crypto.to_owned())?
      .with_io(io)?
      .with_limits(limits)?
      .start()?;

    // quic event loop. this immediately cancels when crypto is updated by tokio::select!
    while let Some(new_conn) = server.accept().await {
      trace!("New QUIC connection established");
      let Ok(Some(new_server_name)) = new_conn.server_name() else {
        warn!("HTTP/3 no SNI is given");
        continue;
      };
      debug!("HTTP/3 connection incoming (SNI {:?})", new_server_name);
      let self_clone = self.clone();

      self.globals.runtime_handle.spawn(async move {
        let client_addr = new_conn.remote_addr()?;
        let quic_connection = s2n_quic_h3::Connection::new(new_conn);
        // Timeout is based on underlying quic
        if let Err(e) = self_clone
          .h3_serve_connection(quic_connection, new_server_name.to_server_name(), client_addr)
          .await
        {
          warn!("QUIC or HTTP/3 connection failed: {}", e);
        };
        Ok(()) as RpxyResult<()>
      });
    }

    Ok(())
  }
}

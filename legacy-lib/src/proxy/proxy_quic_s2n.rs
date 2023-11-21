use super::{
  crypto_service::{ServerCrypto, ServerCryptoBase},
  proxy_main::Proxy,
};
use crate::{certs::CryptoSource, error::*, log::*, utils::BytesName};
use hot_reload::ReloaderReceiver;
use hyper_util::client::legacy::connect::Connect;
use s2n_quic::provider;
use std::sync::Arc;

impl<U> Proxy<U>
where
  // T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone + Sync + Send + 'static,
{
  pub(super) async fn listener_service_h3(
    &self,
    mut server_crypto_rx: ReloaderReceiver<ServerCryptoBase>,
  ) -> Result<()> {
    info!("Start UDP proxy serving with HTTP/3 request for configured host names [s2n-quic]");

    // initially wait for receipt
    let mut server_crypto: Option<Arc<ServerCrypto>> = {
      let _ = server_crypto_rx.changed().await;
      let sc = self.receive_server_crypto(server_crypto_rx.clone())?;
      Some(sc)
    };

    // event loop
    loop {
      tokio::select! {
        v = self.listener_service_h3_inner(&server_crypto) => {
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

  fn receive_server_crypto(&self, server_crypto_rx: ReloaderReceiver<ServerCryptoBase>) -> Result<Arc<ServerCrypto>> {
    let cert_keys_map = server_crypto_rx.borrow().clone().ok_or_else(|| {
      error!("Reloader is broken");
      RpxyError::Other(anyhow!("Reloader is broken"))
    })?;

    let server_crypto: Option<Arc<ServerCrypto>> = (&cert_keys_map).try_into().ok();
    server_crypto.ok_or_else(|| {
      error!("Failed to update server crypto for h3 [s2n-quic]");
      RpxyError::Other(anyhow!("Failed to update server crypto for h3 [s2n-quic]"))
    })
  }

  async fn listener_service_h3_inner(&self, server_crypto: &Option<Arc<ServerCrypto>>) -> Result<()> {
    // setup UDP socket
    let io = provider::io::tokio::Builder::default()
      .with_receive_address(self.listening_on)?
      .with_reuse_port()?
      .build()?;

    // setup limits
    let mut limits = provider::limits::Limits::default()
      .with_max_open_local_bidirectional_streams(self.globals.proxy_config.h3_max_concurrent_bidistream as u64)
      .map_err(|e| anyhow!(e))?
      .with_max_open_remote_bidirectional_streams(self.globals.proxy_config.h3_max_concurrent_bidistream as u64)
      .map_err(|e| anyhow!(e))?
      .with_max_open_local_unidirectional_streams(self.globals.proxy_config.h3_max_concurrent_unistream as u64)
      .map_err(|e| anyhow!(e))?
      .with_max_open_remote_unidirectional_streams(self.globals.proxy_config.h3_max_concurrent_unistream as u64)
      .map_err(|e| anyhow!(e))?
      .with_max_active_connection_ids(self.globals.proxy_config.h3_max_concurrent_connections as u64)
      .map_err(|e| anyhow!(e))?;
    limits = if let Some(v) = self.globals.proxy_config.h3_max_idle_timeout {
      limits.with_max_idle_timeout(v).map_err(|e| anyhow!(e))?
    } else {
      limits
    };

    // setup tls
    let Some(server_crypto) = server_crypto else {
      warn!("No server crypto is given [s2n-quic]");
      return Err(RpxyError::Other(anyhow!("No server crypto is given [s2n-quic]")));
    };
    let tls = server_crypto.inner_global_no_client_auth.clone();

    let mut server = s2n_quic::Server::builder()
      .with_tls(tls)
      .map_err(|e| anyhow::anyhow!(e))?
      .with_io(io)
      .map_err(|e| anyhow!(e))?
      .with_limits(limits)
      .map_err(|e| anyhow!(e))?
      .start()
      .map_err(|e| anyhow!(e))?;

    // quic event loop. this immediately cancels when crypto is updated by tokio::select!
    while let Some(new_conn) = server.accept().await {
      debug!("New QUIC connection established");
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
          .connection_serve_h3(quic_connection, new_server_name.to_server_name_vec(), client_addr)
          .await
        {
          warn!("QUIC or HTTP/3 connection failed: {}", e);
        };
        Ok(()) as Result<()>
      });
    }

    Ok(())
  }
}

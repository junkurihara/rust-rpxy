use super::{
  crypto_service::{CryptoReloader, ServerCrypto, ServerCryptoBase, SniServerCryptoMap},
  proxy_main::Proxy,
  socket::bind_tcp_socket,
};
use crate::{certs::CryptoSource, constants::*, error::*, log::*, utils::BytesName};
use hot_reload::{ReloaderReceiver, ReloaderService};
use hyper_util::{client::legacy::connect::Connect, rt::TokioIo, server::conn::auto::Builder as ConnectionBuilder};
use std::sync::Arc;
use tokio::time::{timeout, Duration};

impl<U> Proxy<U>
where
  // T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone + Sync + Send + 'static,
{
  // TCP Listener Service, i.e., http/2 and http/1.1
  async fn listener_service(&self, mut server_crypto_rx: ReloaderReceiver<ServerCryptoBase>) -> Result<()> {
    let tcp_socket = bind_tcp_socket(&self.listening_on)?;
    let tcp_listener = tcp_socket.listen(self.globals.proxy_config.tcp_listen_backlog)?;
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
            let server_name_in_bytes = server_name.map_or_else(|| None, |v| Some(v.to_server_name_vec()));
            if server_name_in_bytes.is_none(){
              return Err(RpxyError::Proxy("No SNI is given".to_string()));
            }
            let server_crypto = sc_map_inner.as_ref().unwrap().get(server_name_in_bytes.as_ref().unwrap());
            if server_crypto.is_none() {
              return Err(RpxyError::Proxy(format!("No TLS serving app for {:?}", server_name.unwrap())));
            }
            let stream = match start.into_stream(server_crypto.unwrap().clone()).await {
              Ok(s) => TokioIo::new(s),
              Err(e) => {
                return Err(RpxyError::Proxy(format!("Failed to handshake TLS: {e}")));
              }
            };
            self_inner.serve_connection(stream, client_addr, server_name_in_bytes);
            Ok(())
          };

          self.globals.runtime_handle.spawn( async move {
            // timeout is introduced to avoid get stuck here.
            let Ok(v) = timeout(
              Duration::from_secs(TLS_HANDSHAKE_TIMEOUT_SEC),
              handshake_fut
            ).await else {
              error!("Timeout to handshake TLS");
              return;
            };
            if let Err(e) = v {
              error!("{}", e);
            }
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

  pub async fn start_with_tls(&self) -> Result<()> {
    let (cert_reloader_service, cert_reloader_rx) = ReloaderService::<CryptoReloader<U>, ServerCryptoBase>::new(
      &self.globals.clone(),
      CERTS_WATCH_DELAY_SECS,
      !LOAD_CERTS_ONLY_WHEN_UPDATED,
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;

    #[cfg(not(any(feature = "http3-quinn", feature = "http3-s2n")))]
    {
      tokio::select! {
        _= cert_reloader_service.start() => {
          error!("Cert service for TLS exited");
        },
        _ = self.listener_service(cert_reloader_rx) => {
          error!("TCP proxy service for TLS exited");
        },
        else => {
          error!("Something went wrong");
          return Ok(())
        }
      };
      Ok(())
    }
    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    {
      if self.globals.proxy_config.http3 {
        tokio::select! {
          _= cert_reloader_service.start() => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(cert_reloader_rx.clone()) => {
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
          _ = self.listener_service(cert_reloader_rx) => {
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

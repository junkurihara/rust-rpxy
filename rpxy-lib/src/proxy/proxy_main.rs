use super::socket::bind_tcp_socket;
use crate::{
  constants::TLS_HANDSHAKE_TIMEOUT_SEC,
  error::*,
  globals::Globals,
  hyper_ext::{
    body::{RequestBody, ResponseBody},
    rt::LocalExecutor,
  },
  log::*,
  message_handler::HttpMessageHandler,
  name_exp::ServerName,
};
use futures::{select, FutureExt};
use http::{Request, Response};
use hyper::{
  body::Incoming,
  rt::{Read, Write},
  service::service_fn,
};
use hyper_util::{client::legacy::connect::Connect, rt::TokioIo, server::conn::auto::Builder as ConnectionBuilder};
use rpxy_certs::ServerCrypto;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::time::timeout;

/// Wrapper function to handle request for HTTP/1.1 and HTTP/2
/// HTTP/3 is handled in proxy_h3.rs which directly calls the message handler
async fn serve_request<T>(
  req: Request<Incoming>,
  handler: Arc<HttpMessageHandler<T>>,
  client_addr: SocketAddr,
  listen_addr: SocketAddr,
  tls_enabled: bool,
  tls_server_name: Option<ServerName>,
) -> RpxyResult<Response<ResponseBody>>
where
  T: Send + Sync + Connect + Clone,
{
  handler
    .handle_request(
      req.map(RequestBody::Incoming),
      client_addr,
      listen_addr,
      tls_enabled,
      tls_server_name,
    )
    .await
}

#[derive(Clone)]
/// Proxy main object responsible to serve requests received from clients at the given socket address.
pub(crate) struct Proxy<T, E = LocalExecutor>
where
  T: Send + Sync + Connect + Clone + 'static,
{
  /// global context shared among async tasks
  pub globals: Arc<Globals>,
  /// listen socket address
  pub listening_on: SocketAddr,
  /// whether TLS is enabled or not
  pub tls_enabled: bool,
  /// hyper connection builder serving http request
  pub connection_builder: Arc<ConnectionBuilder<E>>,
  /// message handler serving incoming http request
  pub message_handler: Arc<HttpMessageHandler<T>>,
}

impl<T> Proxy<T>
where
  T: Send + Sync + Connect + Clone + 'static,
{
  /// Serves requests from clients
  fn serve_connection<I>(&self, stream: I, peer_addr: SocketAddr, tls_server_name: Option<ServerName>)
  where
    I: Read + Write + Send + Unpin + 'static,
  {
    let request_count = self.globals.request_count.clone();
    if request_count.increment() > self.globals.proxy_config.max_clients {
      request_count.decrement();
      return;
    }
    debug!("Request incoming: current # {}", request_count.current());

    let server_clone = self.connection_builder.clone();
    let message_handler_clone = self.message_handler.clone();
    let tls_enabled = self.tls_enabled;
    let listening_on = self.listening_on;
    let handling_timeout = self.globals.proxy_config.connection_handling_timeout;

    self.globals.runtime_handle.clone().spawn(async move {
      let fut = server_clone.serve_connection_with_upgrades(
        stream,
        service_fn(move |req: Request<Incoming>| {
          serve_request(
            req,
            message_handler_clone.clone(),
            peer_addr,
            listening_on,
            tls_enabled,
            tls_server_name.clone(),
          )
        }),
      );

      if let Some(handling_timeout) = handling_timeout {
        timeout(handling_timeout, fut).await.ok();
      } else {
        fut.await.ok();
      }

      request_count.decrement();
      debug!("Request processed: current # {}", request_count.current());
    });
  }

  /// Start without TLS (HTTP cleartext)
  async fn start_without_tls(&self) -> RpxyResult<()> {
    let listener_service = async {
      let tcp_socket = bind_tcp_socket(&self.listening_on)?;
      let tcp_listener = tcp_socket.listen(self.globals.proxy_config.tcp_listen_backlog)?;
      info!("Start TCP proxy serving with HTTP request for configured host names");
      while let Ok((stream, client_addr)) = tcp_listener.accept().await {
        self.serve_connection(TokioIo::new(stream), client_addr, None);
      }
      Ok(()) as RpxyResult<()>
    };
    listener_service.await?;
    Ok(())
  }

  /// Start with TLS (HTTPS)
  pub(super) async fn start_with_tls(&self) -> RpxyResult<()> {
    #[cfg(not(any(feature = "http3-quinn", feature = "http3-s2n")))]
    {
      self.tls_listener_service().await?;
      error!("TCP proxy service for TLS exited");
      Ok(())
    }
    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    {
      if self.globals.proxy_config.http3 {
        select! {
          _ = self.tls_listener_service().fuse() => {
            error!("TCP proxy service for TLS exited");
          },
          _ = self.h3_listener_service().fuse() => {
            error!("UDP proxy service for QUIC exited");
          }
        };
        Ok(())
      } else {
        self.tls_listener_service().await?;
        error!("TCP proxy service for TLS exited");
        Ok(())
      }
    }
  }

  // TCP Listener Service, i.e., http/2 and http/1.1
  async fn tls_listener_service(&self) -> RpxyResult<()> {
    let Some(mut server_crypto_rx) = self.globals.cert_reloader_rx.clone() else {
      return Err(RpxyError::NoCertificateReloader);
    };
    let tcp_socket = bind_tcp_socket(&self.listening_on)?;
    let tcp_listener = tcp_socket.listen(self.globals.proxy_config.tcp_listen_backlog)?;
    info!("Start TCP proxy serving with HTTPS request for configured host names");

    let mut server_crypto_map: Option<Arc<super::SniServerCryptoMap>> = None;
    loop {
      select! {
        tcp_cnx = tcp_listener.accept().fuse() => {
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
              return Err(RpxyError::FailedToTlsHandshake(e.to_string()));
            }
            let start = acceptor.unwrap();
            let client_hello = start.client_hello();
            let sni = client_hello.server_name();
            debug!("HTTP/2 or 1.1: SNI in ClientHello: {:?}", sni.unwrap_or("None"));
            let server_name = sni.map(ServerName::from);
            if server_name.is_none(){
              return Err(RpxyError::NoServerNameInClientHello);
            }
            let server_crypto = sc_map_inner.as_ref().unwrap().get(server_name.as_ref().unwrap());
            if server_crypto.is_none() {
              return Err(RpxyError::NoTlsServingApp(server_name.as_ref().unwrap().try_into().unwrap_or_default()));
            }
            let stream = match start.into_stream(server_crypto.unwrap().clone()).await {
              Ok(s) => TokioIo::new(s),
              Err(e) => {
                return Err(RpxyError::FailedToTlsHandshake(e.to_string()));
              }
            };
            Ok((stream, client_addr, server_name))
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
            match v {
              Ok((stream, client_addr, server_name)) => {
                self_inner.serve_connection(stream, client_addr, server_name);
              }
              Err(e) => {
                error!("{}", e);
              }
            }
          });
        }
        _ = server_crypto_rx.changed().fuse() => {
          if server_crypto_rx.borrow().is_none() {
            error!("Reloader is broken");
            break;
          }
          let server_crypto_base = server_crypto_rx.borrow().clone().unwrap();
          let Some(server_config): Option<Arc<ServerCrypto>> = (&server_crypto_base).try_into().ok() else {
            error!("Failed to update server crypto");
            break;
          };
          let map = server_config.individual_config_map.clone().iter().map(|(k,v)| {
            let server_name = ServerName::from(k.as_slice());
            (server_name, v.clone())
          }).collect::<rustc_hash::FxHashMap<_,_>>();
          server_crypto_map = Some(Arc::new(map));
        }
      }
    }
    Ok(())
  }

  /// Entrypoint for HTTP/1.1, 2 and 3 servers
  pub async fn start(&self) -> RpxyResult<()> {
    let proxy_service = async {
      if self.tls_enabled {
        self.start_with_tls().await
      } else {
        self.start_without_tls().await
      }
    };

    match &self.globals.term_notify {
      Some(term) => {
        select! {
          _ = proxy_service.fuse() => {
            warn!("Proxy service got down");
          }
          _ = term.notified().fuse() => {
            info!("Proxy service listening on {} receives term signal", self.listening_on);
          }
        }
      }
      None => {
        proxy_service.await?;
        warn!("Proxy service got down");
      }
    }

    Ok(())
  }
}

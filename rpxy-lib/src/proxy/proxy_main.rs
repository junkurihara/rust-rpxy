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
use futures::{FutureExt, select};
use http::{Request, Response};
use hyper::{
  body::Incoming,
  rt::{Read, Write},
  service::service_fn,
};
use hyper_util::{client::legacy::connect::Connect, rt::TokioIo, server::conn::auto::Builder as ConnectionBuilder};
use rpxy_certs::ServerCrypto;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpStream, time::timeout};
use tokio_util::sync::CancellationToken;
use ahash::HashMap;

#[cfg(feature = "proxy-protocol")]
use crate::globals::TcpRecvProxyProtocolConfig;

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

/// Result of TLS handshake, including the TLS stream, server name from SNI, and whether it's an ACME TLS ALPN challenge handshake (for conditional shutdown after handshake)
struct TlsHandshakeResult {
  stream: TokioIo<tokio_rustls::server::TlsStream<TcpStream>>,
  server_name: ServerName,
  #[cfg(feature = "acme")]
  is_handshake_acme: bool, // for shutdown just after TLS handshake
}

/// TLS handshake and certificate management for TLS listener service
async fn serve_tls_handshake(raw_stream: TcpStream, server_configs_acme_challenge: Arc<HashMap<String, Arc<rustls::ServerConfig>>>, server_crypto_map: Option<Arc<HashMap<ServerName, Arc<rustls::ServerConfig>>>>) -> RpxyResult<TlsHandshakeResult> {
  let acceptor = tokio_rustls::LazyConfigAcceptor::new(tokio_rustls::rustls::server::Acceptor::default(), raw_stream).await;
  if let Err(e) = acceptor {
    return Err(RpxyError::FailedToTlsHandshake(e.to_string()));
  }
  let start = acceptor.unwrap();
  let client_hello = start.client_hello();
  let sni = client_hello.server_name();
  debug!("HTTP/2 or 1.1: SNI in ClientHello: {:?}", sni.unwrap_or("None"));
  let server_name = sni.map(ServerName::from);
  if server_name.is_none() {
    return Err(RpxyError::NoServerNameInClientHello);
  }
  #[cfg(feature = "acme")]
  let mut is_handshake_acme = false; // for shutdown just after TLS handshake
  /* ------------------ */
  // Check for ACME TLS ALPN challenge
  #[cfg(feature = "acme")]
  let server_crypto = {
    if rpxy_acme::reexports::is_tls_alpn_challenge(&client_hello) {
      info!("ACME TLS ALPN challenge received");
      let Some(server_crypto_acme) = server_configs_acme_challenge.get(&sni.unwrap().to_ascii_lowercase()) else {
        return Err(RpxyError::NoAcmeServerConfig);
      };
      is_handshake_acme = true;
      server_crypto_acme
    } else {
      let server_crypto = server_crypto_map.as_ref().unwrap().get(server_name.as_ref().unwrap());
      let Some(server_crypto) = server_crypto else {
        return Err(RpxyError::NoTlsServingApp(server_name.as_ref().unwrap().try_into().unwrap_or_default()));
      };
      server_crypto
    }
  };
  /* ------------------ */
  #[cfg(not(feature = "acme"))]
  let server_crypto = {
    let server_crypto = server_crypto_map.as_ref().unwrap().get(server_name.as_ref().unwrap());
    let Some(server_crypto) = server_crypto else {
      return Err(RpxyError::NoTlsServingApp(server_name.as_ref().unwrap().try_into().unwrap_or_default()));
    };
    server_crypto
  };
  /* ------------------ */
  let stream = match start.into_stream(server_crypto.clone()).await {
    Ok(s) => TokioIo::new(s),
    Err(e) => {
      return Err(RpxyError::FailedToTlsHandshake(e.to_string()));
    }
  };
  Ok(TlsHandshakeResult {
    stream,
    server_name: server_name.unwrap(),
    #[cfg(feature = "acme")]
    is_handshake_acme,
  })
}

#[cfg(feature = "proxy-protocol")]
/// Extracts and parses the PROXY protocol header from the given TCP stream, returning the real client address.
async fn extract_parse_result_from_proxy_protocol_header(
  mut stream: &mut TcpStream,
  peer_addr: SocketAddr,
  pp_config: TcpRecvProxyProtocolConfig,
) -> Result<SocketAddr, std::io::Error> {
  let parse_result = if pp_config.timeout.is_zero() {
    super::proxy_protocol::parse_inbound_proxy_header(&mut stream, &peer_addr, &pp_config).await
  } else {
    match timeout(
      pp_config.timeout,
      super::proxy_protocol::parse_inbound_proxy_header(&mut stream, &peer_addr, &pp_config),
    )
    .await
    {
      Ok(result) => result,
      Err(_) => Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("PROXY header read timed out after {}ms", pp_config.timeout.as_millis()),
      )),
    }
  };
  parse_result.map(|opt_addr| opt_addr.unwrap_or(peer_addr))
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
    trace!("Request incoming: current # {}", request_count.current());

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
      trace!("Request processed: current # {}", request_count.current());
    });
  }

  /// Start without TLS (HTTP cleartext)
  async fn start_without_tls(&self) -> RpxyResult<()> {
    let listener_service = async {
      let tcp_socket = bind_tcp_socket(&self.listening_on)?;
      let tcp_listener = tcp_socket.listen(self.globals.proxy_config.tcp_listen_backlog)?;
      info!("Start TCP proxy serving with HTTP request for configured host names");
      #[cfg(not(feature = "proxy-protocol"))]
      while let Ok((stream, client_addr)) = tcp_listener.accept().await {
        trace!("Accepted TCP connection from {client_addr}");
        self.serve_connection(TokioIo::new(stream), client_addr, None);
      }
      #[cfg(feature = "proxy-protocol")]
      while let Ok((mut stream, client_addr)) = tcp_listener.accept().await {
        trace!("Accepted TCP connection from {client_addr}");
        // [PROXY-PROTOCOL] Parse PROXY header before serving connection
        if self.globals.proxy_config.tcp_recv_proxy_protocol.is_some() {
          let pp_config = self.globals.proxy_config.tcp_recv_proxy_protocol.clone().unwrap();
          let self_inner = self.clone();
          self.globals.runtime_handle.spawn(async move {
            let parse_result = extract_parse_result_from_proxy_protocol_header(&mut stream, client_addr, pp_config).await;
            let real_addr = match parse_result {
              Ok(addr) => addr,
              Err(e) => {
                warn!("Failed to parse PROXY header: {e}. Closing connection from {client_addr}");
                return;
              }
            };
            self_inner.serve_connection(TokioIo::new(stream), real_addr, None);
          });
          continue;
        }
        // If inbound PROXY protocol is not enabled, serve connection directly with peer address from TCP accept
        self.serve_connection(TokioIo::new(stream), client_addr, None);
      }

      Ok(()) as RpxyResult<()>
    };
    listener_service.await?;
    Ok(())
  }

  /// Start with TLS (HTTPS)
  pub(super) async fn start_with_tls(&self, cancel_token: CancellationToken) -> RpxyResult<()> {
    // By default, TLS listener is spawned
    let join_handle_tls = self.globals.runtime_handle.spawn({
      let self_clone = self.clone();
      let cancel_token = cancel_token.clone();
      async move {
        select! {
          _ = self_clone.tls_listener_service().fuse() => {
            error!("TCP proxy service for TLS exited");
            cancel_token.cancel();
          },
          _ = cancel_token.cancelled().fuse() => {
            debug!("Cancel token is called for TLS listener");
          }
        }
      }
    });

    #[cfg(not(any(feature = "http3-quinn", feature = "http3-s2n")))]
    {
      let _ = join_handle_tls.await;
      Ok(())
    }

    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    {
      // If HTTP/3 is not enabled, wait for TLS listener to finish
      if !self.globals.proxy_config.http3 {
        let _ = join_handle_tls.await;
        return Ok(());
      }

      // If HTTP/3 is enabled, spawn a task to handle HTTP/3 connections
      let join_handle_h3 = self.globals.runtime_handle.spawn({
        let self_clone = self.clone();
        async move {
          select! {
            _ = self_clone.h3_listener_service().fuse() => {
              error!("UDP proxy service for QUIC exited");
              cancel_token.cancel();
            },
            _ = cancel_token.cancelled().fuse() => {
              debug!("Cancel token is called for QUIC listener");
            }
          }
        }
      });
      let _ = futures::future::join(join_handle_tls, join_handle_h3).await;

      Ok(())
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
      #[cfg(feature = "acme")]
      let server_configs_acme_challenge = self.globals.server_configs_acme_challenge.clone();

      select! {
        tcp_cnx = tcp_listener.accept().fuse() => {
          if tcp_cnx.is_err() || server_crypto_map.is_none() {
            continue;
          }

          #[cfg(feature = "proxy-protocol")]
          let (mut raw_stream, client_addr) = tcp_cnx.unwrap();
          #[cfg(not(feature = "proxy-protocol"))]
          let (raw_stream, client_addr) = tcp_cnx.unwrap();
          trace!("Accepted TCP connection from {client_addr} at TLS listener");

          let sc_map_inner = server_crypto_map.clone();
          let self_inner = self.clone();

          #[cfg(feature = "proxy-protocol")]
          let pp_config = self.globals.proxy_config.tcp_recv_proxy_protocol.clone();

          // spawns async handshake to avoid blocking thread by sequential handshake.
          self.globals.runtime_handle.spawn(async move {
            #[cfg(feature = "proxy-protocol")]
            // [PROXY-PROTOCOL] Parse PROXY header before TLS handshake, and obtain the real client address
            // Before TLS handshake, the PROXY protocol header is expected to be present if the inbound PROXY protocol is enabled. If parsing fails, the connection will be closed. After this step, client_addr will be the real client address to be used for TLS handshake and further processing.
            let client_addr = {
              if let Some(ref pp_config) = pp_config {
                let parse_result = extract_parse_result_from_proxy_protocol_header(&mut raw_stream, client_addr, pp_config.to_owned()).await;
                match parse_result {
                  Ok(addr) => addr,
                  Err(e) => {
                    warn!("Failed to parse PROXY header: {e}. Closing connection from {client_addr}");
                    return;
                  }
                }
              } else {
                client_addr
              }
            };

            let tls_handshake_fut = serve_tls_handshake(raw_stream, server_configs_acme_challenge, sc_map_inner);

            // timeout is introduced to avoid get stuck here.
            let Ok(tls_handshake_result) = timeout(
              Duration::from_secs(TLS_HANDSHAKE_TIMEOUT_SEC),
              tls_handshake_fut
            ).await else {
              error!("Timeout to handshake TLS");
              return;
            };
            /* ------------------ */
            #[cfg(feature = "acme")]
            {
              match tls_handshake_result {
                Ok(TlsHandshakeResult { mut stream, server_name, is_handshake_acme }) => {
                  if is_handshake_acme {
                    debug!("Shutdown TLS connection after ACME TLS ALPN challenge");
                    use tokio::io::AsyncWriteExt;
                    stream.inner_mut().shutdown().await.ok();
                  }
                  self_inner.serve_connection(stream, client_addr, Some(server_name));
                }
                Err(e) => {
                  error!("{}", e);
                }
              }
            }
            /* ------------------ */
            #[cfg(not(feature = "acme"))]
            {
              match tls_handshake_result {
                Ok(TlsHandshakeResult { mut stream, server_name, is_handshake_acme }) => {
                  self_inner.serve_connection(stream, client_addr, Some(server_name));
                }
                Err(e) => {
                  error!("{}", e);
                }
              }
            }
            /* ------------------ */
          });
        }
        _ = server_crypto_rx.changed().fuse() => {
          if server_crypto_rx.borrow().is_none() {
            error!("Reloader is broken");
            break;
          }
          let server_crypto_base = server_crypto_rx.get().unwrap();
          let Some(server_config): Option<Arc<ServerCrypto>> = (&server_crypto_base).try_into().ok() else {
            // Don't break the loop - certs might become available later (e.g., ACME provisioning)
            warn!("No valid certificates loaded yet, waiting for next reload cycle");
            continue;
          };
          let map = server_config.individual_config_map.clone().iter().map(|(k,v)| {
            let server_name = ServerName::from(k.as_slice());
            (server_name, v.clone())
          }).collect::<std::collections::HashMap<_,_,ahash::RandomState>>();
          server_crypto_map = Some(Arc::new(map));
          info!("TLS certificates updated successfully");
        }
      }
    }
    Ok(())
  }

  /// Entrypoint for HTTP/1.1, 2 and 3 servers
  pub async fn start(&self, cancel_token: CancellationToken) -> RpxyResult<()> {
    let proxy_service = async {
      if self.tls_enabled {
        self.start_with_tls(cancel_token).await
      } else {
        self.start_without_tls().await
      }
    };

    proxy_service.await
  }
}

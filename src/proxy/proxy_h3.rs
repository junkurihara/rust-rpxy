use super::Proxy;
use crate::{backend::ServerNameLC, error::*, log::*};
use bytes::{Buf, Bytes};
use h3::{quic::BidiStream, server::RequestStream};
use hyper::{client::connect::Connect, Body, HeaderMap, Request, Response};
use std::net::SocketAddr;
use tokio::time::{timeout, Duration};

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn client_serve_h3(&self, conn: quinn::Connecting, tls_server_name: &[u8]) {
    let clients_count = self.globals.clients_count.clone();
    if clients_count.increment() > self.globals.max_clients {
      clients_count.decrement();
      return;
    }
    let fut = self
      .clone()
      .handle_connection_h3(conn, tls_server_name.to_vec());
    self.globals.runtime_handle.spawn(async move {
      // Timeout is based on underlying quic
      if let Err(e) = fut.await {
        warn!("QUIC or HTTP/3 connection failed: {}", e)
      }
      clients_count.decrement();
      debug!("Client #: {}", clients_count.current());
    });
  }

  pub async fn handle_connection_h3(
    self,
    conn: quinn::Connecting,
    tls_server_name: ServerNameLC,
  ) -> Result<()> {
    let client_addr = conn.remote_address();

    match conn.await {
      Ok(new_conn) => {
        let mut h3_conn =
          h3::server::Connection::<_, bytes::Bytes>::new(h3_quinn::Connection::new(new_conn))
            .await?;
        info!(
          "QUIC/HTTP3 connection established from {:?} {:?}",
          client_addr, tls_server_name
        );

        // Does this work enough?
        // while let Some((req, stream)) = h3_conn
        //   .accept()
        //   .await
        //   .map_err(|e| anyhow!("HTTP/3 accept failed: {}", e))?
        while let Some((req, stream)) = match h3_conn.accept().await {
          Ok(opt_req) => opt_req,
          Err(_) => {
            warn!("HTTP/3 failed to accept incoming connection (likely timeout)");
            return Ok(h3_conn.shutdown(0).await?);
          }
        } {
          debug!(
            "HTTP/3 new request from {}: {} {}",
            client_addr,
            req.method(),
            req.uri()
          );

          let self_inner = self.clone();
          let tls_server_name_inner = tls_server_name.clone();
          self.globals.runtime_handle.spawn(async move {
            if let Err(e) = timeout(
              self_inner.globals.proxy_timeout + Duration::from_secs(1), // timeout per stream are considered as same as one in http2
              self_inner.handle_stream_h3(req, stream, client_addr, tls_server_name_inner),
            )
            .await
            {
              error!("HTTP/3 failed to process stream: {}", e);
            }
          });
        }
      }
      Err(err) => {
        warn!("QUIC accepting connection failed: {:?}", err);
        return Err(anyhow!("{}", err));
      }
    }

    Ok(())
  }

  async fn handle_stream_h3<S>(
    self,
    req: Request<()>,
    mut stream: RequestStream<S, Bytes>,
    client_addr: SocketAddr,
    tls_server_name: ServerNameLC,
  ) -> Result<()>
  where
    S: BidiStream<Bytes>,
  {
    let (req_parts, _) = req.into_parts();

    // TODO: h3 -> h2/http1.1などのプロトコル変換がなければ、bodyはBytes単位で直でsend_dataして転送した方がいい。やむなし。
    let mut body_chunk: Vec<u8> = Vec::new();
    while let Some(request_body) = stream.recv_data().await? {
      body_chunk.extend_from_slice(request_body.chunk());
    }
    let body = if body_chunk.is_empty() {
      Body::default()
    } else {
      debug!("HTTP/3 request with non-empty body");
      Body::from(body_chunk)
    };
    // trailers
    let trailers = if let Some(trailers) = stream.recv_trailers().await? {
      debug!("HTTP/3 request with trailers");
      trailers
    } else {
      HeaderMap::new()
    };

    let new_req: Request<Body> = Request::from_parts(req_parts, body);
    let res = self
      .msg_handler
      .clone()
      .handle_request(
        new_req,
        client_addr,
        self.listening_on,
        self.tls_enabled,
        Some(tls_server_name),
      )
      .await?;

    let (new_res_parts, new_body) = res.into_parts();
    let new_res = Response::from_parts(new_res_parts, ());

    match stream.send_response(new_res).await {
      Ok(_) => {
        debug!("HTTP/3 response to connection successful");
        let data = hyper::body::to_bytes(new_body).await?;
        stream.send_data(data).await?;
        stream.send_trailers(trailers).await?;
        return Ok(stream.finish().await?);
      }
      Err(err) => {
        error!("Unable to send response to connection peer: {:?}", err);
      }
    }
    Ok(stream.finish().await?)
  }
}

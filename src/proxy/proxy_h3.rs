use super::Proxy;
use crate::{backend::ServerNameLC, constants::*, error::*, log::*};
use bytes::{Buf, Bytes};
use h3::{quic::BidiStream, server::RequestStream};
use hyper::{client::connect::Connect, Body, Request, Response};
use std::net::SocketAddr;
use tokio::time::{timeout, Duration};

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub(super) fn client_serve_h3(&self, conn: quinn::Connecting, tls_server_name: &[u8]) {
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

  async fn handle_connection_h3(
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
          Err(e) => {
            warn!(
              "HTTP/3 failed to accept incoming connection (likely timeout): {}",
              e
            );
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
    stream: RequestStream<S, Bytes>,
    client_addr: SocketAddr,
    tls_server_name: ServerNameLC,
  ) -> Result<()>
  where
    S: BidiStream<Bytes> + Send + 'static,
    <S as BidiStream<Bytes>>::RecvStream: Send,
  {
    let (req_parts, _) = req.into_parts();
    // split stream and async body handling
    let (mut send_stream, mut recv_stream) = stream.split();

    // generate streamed body with trailers using channel
    let (body_sender, req_body) = Body::channel();

    // Buffering and sending body through channel for protocol conversion like h3 -> h2/http1.1
    // The underling buffering, i.e., buffer given by the API recv_data.await?, is handled by quinn.
    self.globals.runtime_handle.spawn(async move {
      let mut sender = body_sender;
      let mut size = 0usize;
      while let Some(mut body) = recv_stream.recv_data().await? {
        debug!("HTTP/3 incoming request body");
        size += body.remaining();
        if size > H3_REQUEST_MAX_BODY_SIZE {
          error!("Exceeds max request body size for HTTP/3");
          return Err(anyhow!("Exceeds max request body size for HTTP/3"));
        }
        // create stream body to save memory, shallow copy (increment of ref-count) to Bytes using copy_to_bytes
        sender
          .send_data(body.copy_to_bytes(body.remaining()))
          .await?;
      }

      // trailers: use inner for work around. (directly get trailer)
      let trailers = recv_stream.as_mut().recv_trailers().await?;
      if trailers.is_some() {
        debug!("HTTP/3 incoming request trailers");
        sender.send_trailers(trailers.unwrap()).await?;
      }
      Ok(()) as Result<()>
    });

    let new_req: Request<Body> = Request::from_parts(req_parts, req_body);
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

    match send_stream.send_response(new_res).await {
      Ok(_) => {
        debug!("HTTP/3 response to connection successful");
        // aggregate body without copying
        let mut body_data = hyper::body::aggregate(new_body).await?;

        // create stream body to save memory, shallow copy (increment of ref-count) to Bytes using copy_to_bytes
        send_stream
          .send_data(body_data.copy_to_bytes(body_data.remaining()))
          .await?;

        // TODO: needs handling trailer? should be included in body from handler.
      }
      Err(err) => {
        error!("Unable to send response to connection peer: {:?}", err);
      }
    }
    Ok(send_stream.finish().await?)
  }
}

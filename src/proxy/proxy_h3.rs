use super::Proxy;
use crate::{backend::ServerNameLC, error::*, log::*};
use bytes::{Buf, Bytes};
use h3::{quic::BidiStream, server::RequestStream};
use hyper::{client::connect::Connect, Body, Request, Response};
use std::net::SocketAddr;
use tokio::time::{timeout, Duration};

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub(super) async fn connection_serve_h3(self, conn: quinn::Connecting, tls_server_name: ServerNameLC) -> Result<()> {
    let client_addr = conn.remote_address();

    match conn.await {
      Ok(new_conn) => {
        let mut h3_conn = h3::server::Connection::<_, bytes::Bytes>::new(h3_quinn::Connection::new(new_conn)).await?;
        info!(
          "QUIC/HTTP3 connection established from {:?} {:?}",
          client_addr, tls_server_name
        );
        // TODO: Is here enough to fetch server_name from NewConnection?
        // to avoid deep nested call from listener_service_h3
        while let Some((req, stream)) = match h3_conn.accept().await {
          Ok(opt_req) => opt_req,
          Err(e) => {
            warn!("HTTP/3 failed to accept incoming connection: {}", e);
            return Ok(h3_conn.shutdown(0).await?);
          }
        } {
          // We consider the connection count separately from the stream count.
          // Max clients for h1/h2 = max 'stream' for h3.
          let request_count = self.globals.request_count.clone();
          if request_count.increment() > self.globals.max_clients {
            request_count.decrement();
            return Ok(h3_conn.shutdown(0).await?);
          }
          debug!("Request incoming: current # {}", request_count.current());

          let self_inner = self.clone();
          let tls_server_name_inner = tls_server_name.clone();
          self.globals.runtime_handle.spawn(async move {
            if let Err(e) = timeout(
              self_inner.globals.proxy_timeout + Duration::from_secs(1), // timeout per stream are considered as same as one in http2
              self_inner.stream_serve_h3(req, stream, client_addr, tls_server_name_inner),
            )
            .await
            {
              error!("HTTP/3 failed to process stream: {}", e);
            }
            request_count.decrement();
            debug!("Request processed: current # {}", request_count.current());
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

  async fn stream_serve_h3<S>(
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
    let max_body_size = self.globals.h3_request_max_body_size;
    self.globals.runtime_handle.spawn(async move {
      let mut sender = body_sender;
      let mut size = 0usize;
      while let Some(mut body) = recv_stream.recv_data().await? {
        debug!("HTTP/3 incoming request body");
        size += body.remaining();
        if size > max_body_size {
          error!("Exceeds max request body size for HTTP/3");
          return Err(anyhow!("Exceeds max request body size for HTTP/3"));
        }
        // create stream body to save memory, shallow copy (increment of ref-count) to Bytes using copy_to_bytes
        sender.send_data(body.copy_to_bytes(body.remaining())).await?;
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

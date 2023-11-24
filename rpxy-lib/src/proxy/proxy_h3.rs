use super::proxy_main::Proxy;
use crate::{
  error::*,
  hyper_ext::{
    body::{IncomingLike, IncomingOr},
    full, synthetic_response,
  },
  log::*,
  name_exp::ServerName,
};
use bytes::{Buf, Bytes};
use http::{Request, Response};
use http_body_util::BodyExt;
use std::{net::SocketAddr, time::Duration};
use tokio::time::timeout;

#[cfg(feature = "http3-quinn")]
use h3::{quic::BidiStream, quic::Connection as ConnectionQuic, server::RequestStream};
#[cfg(feature = "http3-s2n")]
use s2n_quic_h3::h3::{self, quic::BidiStream, quic::Connection as ConnectionQuic, server::RequestStream};

// use futures::Stream;
// use hyper_util::client::legacy::connect::Connect;

// impl<U> Proxy<U>
// where
//   // T: Connect + Clone + Sync + Send + 'static,
//   U: CryptoSource + Clone + Sync + Send + 'static,
// {

impl Proxy {
  pub(super) async fn h3_serve_connection<C>(
    &self,
    quic_connection: C,
    tls_server_name: ServerName,
    client_addr: SocketAddr,
  ) -> RpxyResult<()>
  where
    C: ConnectionQuic<Bytes>,
    <C as ConnectionQuic<Bytes>>::BidiStream: BidiStream<Bytes> + Send + 'static,
    <<C as ConnectionQuic<Bytes>>::BidiStream as BidiStream<Bytes>>::RecvStream: Send,
    <<C as ConnectionQuic<Bytes>>::BidiStream as BidiStream<Bytes>>::SendStream: Send,
  {
    let mut h3_conn = h3::server::Connection::<_, Bytes>::new(quic_connection).await?;
    info!(
      "QUIC/HTTP3 connection established from {:?} {}",
      client_addr,
      <&ServerName as TryInto<String>>::try_into(&tls_server_name).unwrap_or_default()
    );

    // TODO: Is here enough to fetch server_name from NewConnection?
    // to avoid deep nested call from listener_service_h3
    loop {
      // this routine follows hyperium/h3 examples https://github.com/hyperium/h3/blob/master/examples/server.rs
      match h3_conn.accept().await {
        Ok(None) => {
          break;
        }
        Err(e) => {
          warn!("HTTP/3 error on accept incoming connection: {}", e);
          match e.get_error_level() {
            h3::error::ErrorLevel::ConnectionError => break,
            h3::error::ErrorLevel::StreamError => continue,
          }
        }
        Ok(Some((req, stream))) => {
          // We consider the connection count separately from the stream count.
          // Max clients for h1/h2 = max 'stream' for h3.
          let request_count = self.globals.request_count.clone();
          if request_count.increment() > self.globals.proxy_config.max_clients {
            request_count.decrement();
            h3_conn.shutdown(0).await?;
            break;
          }
          debug!("Request incoming: current # {}", request_count.current());

          let self_inner = self.clone();
          let tls_server_name_inner = tls_server_name.clone();
          self.globals.runtime_handle.spawn(async move {
            if let Err(e) = timeout(
              self_inner.globals.proxy_config.proxy_timeout + Duration::from_secs(1), // timeout per stream are considered as same as one in http2
              self_inner.h3_serve_stream(req, stream, client_addr, tls_server_name_inner),
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
    }

    Ok(())
  }

  /// Serves a request stream from a client
  /// TODO: TODO: TODO: TODO:
  /// TODO: Body in hyper-0.14 was changed to Incoming in hyper-1.0, and it is not accessible from outside.
  /// Thus, we need to implement IncomingLike trait using channel. Also, the backend handler must feed the body in the form of
  /// Either<Incoming, IncomingLike> as body.
  /// Also, the downstream from the backend handler could be Incoming, but will be wrapped as Either<Incoming, ()/Empty> as well due to H3.
  /// Result<Either<_,_>, E> type includes E as HttpError to generate the status code and related Response<BoxBody>.
  /// Thus to handle synthetic error messages in BoxBody, the serve() function outputs Response<Either<Either<Incoming, ()/Empty>, BoxBody>>>.
  async fn h3_serve_stream<S>(
    &self,
    req: Request<()>,
    stream: RequestStream<S, Bytes>,
    client_addr: SocketAddr,
    tls_server_name: ServerName,
  ) -> RpxyResult<()>
  where
    S: BidiStream<Bytes> + Send + 'static,
    <S as BidiStream<Bytes>>::RecvStream: Send,
  {
    let (req_parts, _) = req.into_parts();
    // split stream and async body handling
    let (mut send_stream, mut recv_stream) = stream.split();

    // generate streamed body with trailers using channel
    let (body_sender, req_body) = IncomingLike::channel();

    // Buffering and sending body through channel for protocol conversion like h3 -> h2/http1.1
    // The underling buffering, i.e., buffer given by the API recv_data.await?, is handled by quinn.
    let max_body_size = self.globals.proxy_config.h3_request_max_body_size;
    self.globals.runtime_handle.spawn(async move {
      let mut sender = body_sender;
      let mut size = 0usize;
      while let Some(mut body) = recv_stream.recv_data().await? {
        debug!("HTTP/3 incoming request body: remaining {}", body.remaining());
        size += body.remaining();
        if size > max_body_size {
          error!(
            "Exceeds max request body size for HTTP/3: received {}, maximum_allowd {}",
            size, max_body_size
          );
          return Err(RpxyError::H3TooLargeBody);
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
      Ok(()) as RpxyResult<()>
    });

    let mut new_req: Request<IncomingOr<IncomingLike>> = Request::from_parts(req_parts, IncomingOr::Right(req_body));

    // let res = selfw
    //   .msg_handler
    //   .clone()
    //   .handle_request(
    //     new_req,
    //     client_addr,
    //     self.listening_on,
    //     self.tls_enabled,
    //     Some(tls_server_name),
    //   )
    //   .await?;

    // TODO: TODO: TODO: remove later
    let body = full(Bytes::from("hello h3 echo"));
    // here response is IncomingOr<BoxBody> from message handler
    let res = synthetic_response(Response::builder().body(body).unwrap())?;
    /////////////////

    let (new_res_parts, new_body) = res.into_parts();
    let new_res = Response::from_parts(new_res_parts, ());

    match send_stream.send_response(new_res).await {
      Ok(_) => {
        debug!("HTTP/3 response to connection successful");
        // aggregate body without copying
        let body_data = new_body
          .collect()
          .await
          .map_err(|e| RpxyError::HyperBodyManipulationError(e.to_string()))?;

        // create stream body to save memory, shallow copy (increment of ref-count) to Bytes using copy_to_bytes inside to_bytes()
        send_stream.send_data(body_data.to_bytes()).await?;

        // TODO: needs handling trailer? should be included in body from handler.
      }
      Err(err) => {
        error!("Unable to send response to connection peer: {:?}", err);
      }
    }
    Ok(send_stream.finish().await?)
  }
}

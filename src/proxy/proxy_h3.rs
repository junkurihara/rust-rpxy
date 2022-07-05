use super::Proxy;
use crate::{error::*, log::*};
use bytes::{Buf, Bytes};
use h3::{quic::BidiStream, server::RequestStream};
use hyper::{client::connect::Connect, Body, HeaderMap, Request, Response};
use std::net::SocketAddr;

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn client_serve_h3(self, conn: quinn::Connecting) -> Result<()> {
    // TODO: client数の管理
    let client_addr = conn.remote_address();

    match conn.await {
      Ok(new_conn) => {
        info!("QUIC connection established from {:?} {:?}", client_addr, {
          let hsd = new_conn
            .connection
            .handshake_data()
            .ok_or_else(|| anyhow!(""))?
            .downcast::<quinn::crypto::rustls::HandshakeData>()
            .map_err(|_| anyhow!(""))?;
          (
            hsd.protocol.map_or_else(
              || "<none>".into(),
              |x| String::from_utf8_lossy(&x).into_owned(),
            ),
            hsd.server_name.map_or_else(|| "<none>".into(), |x| x),
          )
        });

        let mut h3_conn =
          h3::server::Connection::<_, bytes::Bytes>::new(h3_quinn::Connection::new(new_conn))
            .await?;
        info!("HTTP/3 connection established");

        while let Some((req, stream)) = h3_conn
          .accept()
          .await
          .map_err(|e| anyhow!("HTTP/3 accept failed: {}", e))?
        {
          info!("HTTP/3 new request received");

          let self_inner = self.clone();
          self.globals.runtime_handle.spawn(async move {
            if let Err(e) = self_inner.handle_request_h3(req, stream, client_addr).await {
              error!("HTTP/3 request failed: {}", e);
            }
          });
        }
      }
      Err(err) => {
        warn!("QUIC accepting connection failed: {:?}", err);
      }
    }

    Ok(())
  }

  async fn handle_request_h3<S>(
    self,
    req: Request<()>,
    mut stream: RequestStream<S, Bytes>,
    client_addr: SocketAddr,
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
    let res = self.handle_request(new_req, client_addr).await?;

    let (new_res_parts, new_body) = res.into_parts();
    let new_res = Response::from_parts(new_res_parts, ());

    match stream.send_response(new_res).await {
      Ok(_) => {
        debug!("HTTP/3 response to connection successful");
        let data = hyper::body::to_bytes(new_body).await?;
        stream.send_data(data).await?;
        stream.send_trailers(trailers).await?;
      }
      Err(err) => {
        error!("Unable to send response to connection peer: {:?}", err);
      }
    }
    Ok(stream.finish().await?)
  }
}

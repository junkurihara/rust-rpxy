use super::Proxy;
use crate::{error::*, log::*};
use bytes::{Buf, Bytes, BytesMut};
use futures::{FutureExt, StreamExt};
use h3::{quic::BidiStream, server::RequestStream};
use hyper::body::HttpBody;
use hyper::http::request;
use hyper::Response;
use hyper::{client::connect::Connect, Body, Request};
use std::{ascii, str};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn client_serve_h3(self, conn: quinn::Connecting) -> Result<()> {
    let client_addr = conn.remote_address();

    match conn.await {
      Ok(new_conn) => {
        debug!(
          "HTTP/3 connection established from {:?} {:?}",
          client_addr,
          {
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
          }
        );

        let mut h3_conn =
          h3::server::Connection::<_, bytes::Bytes>::new(h3_quinn::Connection::new(new_conn))
            .await?;

        // let self_inner = self.clone();
        while let Some((req, stream)) = h3_conn.accept().await? {
          debug!("New request: {:?}", req);

          let self_inner = self.clone();
          self.globals.runtime_handle.spawn(async move {
            let res = self_inner.handle_request_h3(req, stream, client_addr).await;
            // if let Err(e) = handle_request(req, stream).await {
            // error!("HTTP/3 request failed: {}", e);
            // }
            // });
            // tokio::spawn(async {
            //   if let Err(e) = handle_request(req, stream, root).await {
            //     error!("request failed: {}", e);
            //   }
          });
        }
      }
      Err(err) => {
        warn!("HTTP/3 accepting connection failed: {:?}", err);
      }
    }
    // let quinn::NewConnection {
    //   connection,
    //   mut bi_streams,
    //   ..
    // } = conn.await?;
    // async {
    //   debug!(
    //     "HTTP/3 connection established from {:?} (ALPN {:?}, SNI: {:?})",
    //     connection.remote_address(),
    //     connection
    //       .handshake_data()
    //       .unwrap()
    //       .downcast::<quinn::crypto::rustls::HandshakeData>()
    //       .unwrap()
    //       .protocol
    //       .map_or_else(
    //         || "<none>".into(),
    //         |x| String::from_utf8_lossy(&x).into_owned()
    //       ),
    //     connection
    //       .handshake_data()
    //       .unwrap()
    //       .downcast::<quinn::crypto::rustls::HandshakeData>()
    //       .unwrap()
    //       .server_name
    //       .map_or_else(|| "<none>".into(), |x| x)
    //   );

    //   // Each stream initiated by the client constitutes a new request.
    //   while let Some(stream) = bi_streams.next().await {
    //     let stream = match stream {
    //       Err(quinn::ConnectionError::ApplicationClosed { .. }) => {
    //         debug!("HTTP/3 connection closed");
    //         return Ok(());
    //       }
    //       Err(e) => {
    //         return Err(e);
    //       }
    //       Ok(s) => s,
    //     };
    //     let fut = handle_request_h3(stream);
    //     tokio::spawn(async move {
    //       if let Err(e) = fut.await {
    //         error!("failed: {reason}", reason = e.to_string());
    //       }
    //     });
    //   }
    //   Ok(())
    // }
    // .await?;
    // Ok(())
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

    let body = if let Some(request_body) = stream.recv_data().await? {
      let chunk = request_body.chunk();
      Body::from(chunk.to_owned())
    } else {
      Body::default()
    };

    let mut new_req: Request<Body> = Request::from_parts(req_parts, body);
    if let Some(request_trailers) = stream.recv_trailers().await? {
      let headers = new_req.headers_mut();
      for (ok, v) in request_trailers {
        if let Some(k) = ok {
          headers.insert(k, v);
        }
      }
    };

    let res = self.handle_request(new_req, client_addr).await?;
    println!("{:?}", res);

    let (new_res_parts, new_body) = res.into_parts();
    let new_res = Response::from_parts(new_res_parts, ());

    match stream.send_response(new_res).await {
      Ok(_) => {
        debug!("HTTP/3 response to connection successful");
        let data = hyper::body::to_bytes(new_body).await?;
        stream.send_data(data).await?;
      }
      Err(err) => {
        error!("Unable to send response to connection peer: {:?}", err);
      }
    }
    Ok(())
  }
}

// TODO:
// async fn handle_request_h3((mut send, recv): (quinn::SendStream, quinn::RecvStream)) -> Result<()> {
//   let req = recv
//     .read_to_end(64 * 1024)
//     .await
//     .map_err(|e| anyhow!("failed reading request: {}", e))?;

//   // let hyper_req = hyper::Request::try_from(req.clone());

//   let mut escaped = String::new();
//   for &x in &req[..] {
//     let part = ascii::escape_default(x).collect::<Vec<_>>();
//     escaped.push_str(str::from_utf8(&part).unwrap());
//   }
//   info!("content = {:?}", escaped);
//   // Execute the request
//   let resp = process_get(&req).unwrap_or_else(|e| {
//     error!("failed: {}", e);
//     format!("failed to process request: {}\n", e).into_bytes()
//   });
//   // Write the response
//   send
//     .write_all(&resp)
//     .await
//     .map_err(|e| anyhow!("failed to send response: {}", e))?;
//   // Gracefully terminate the stream
//   send
//     .finish()
//     .await
//     .map_err(|e| anyhow!("failed to shutdown stream: {}", e))?;
//   info!("complete");
//   Ok(())
// }

// fn process_get(x: &[u8]) -> Result<Vec<u8>> {
//   if x.len() < 4 || &x[0..4] != b"GET " {
//     bail!("missing GET");
//   }
//   if x[4..].len() < 2 || &x[x.len() - 2..] != b"\r\n" {
//     bail!("missing \\r\\n");
//   }

//   let data = b"hello world!".to_vec();
//   Ok(data)
// }

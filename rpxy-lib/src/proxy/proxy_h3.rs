use super::proxy_main::Proxy;
use crate::{
  error::*,
  hyper_ext::body::{IncomingLike, RequestBody},
  log::*,
  name_exp::ServerName,
};
use bytes::{Buf, Bytes};
use http::{HeaderMap, HeaderName, HeaderValue, Request, Response, header};
use http_body_util::BodyExt;
use hyper_util::client::legacy::connect::Connect;
use std::{
  net::SocketAddr,
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
};

#[cfg(feature = "http3-quinn")]
use h3::{error::Code, quic::BidiStream, quic::Connection as ConnectionQuic, quic::OpenStreams, server::RequestStream};
#[cfg(all(feature = "http3-s2n", not(feature = "http3-quinn")))]
use s2n_quic_h3::h3::{
  self, error::Code, quic::BidiStream, quic::Connection as ConnectionQuic, quic::OpenStreams, server::RequestStream,
};

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub(super) async fn h3_serve_connection<C>(
    &self,
    quic_connection: C,
    tls_server_name: ServerName,
    client_addr: SocketAddr,
  ) -> RpxyResult<()>
  where
    C: ConnectionQuic<Bytes>,
    <C as OpenStreams<Bytes>>::BidiStream: BidiStream<Bytes> + Send + 'static,
    <<C as OpenStreams<Bytes>>::BidiStream as BidiStream<Bytes>>::RecvStream: Send,
    <<C as OpenStreams<Bytes>>::BidiStream as BidiStream<Bytes>>::SendStream: Send,
  {
    let mut h3_conn = h3::server::Connection::<_, Bytes>::new(quic_connection).await?;
    trace!("QUIC/HTTP3 connection established from {:?} {}", client_addr, tls_server_name);

    // TODO: Evaluate whether tls_server_name should live in connection context
    // so listener_service_h3 does not need to thread it through this call.
    loop {
      // this routine follows hyperium/h3 examples https://github.com/hyperium/h3/blob/master/examples/server.rs
      match h3_conn.accept().await {
        Ok(None) => {
          break;
        }
        Err(e) => {
          warn!("HTTP/3 error on accept incoming connection: {}", e);
          break;
        }
        // Ok(Some((req, stream))) => {
        Ok(Some(req_resolver)) => {
          let (req, stream) = match req_resolver.resolve_request().await {
            Ok((req, stream)) => (req, stream),
            Err(e) => {
              warn!("HTTP/3 error on resolve request in stream: {}", e);
              continue;
            }
          };
          trace!("HTTP/3 request stream incoming");

          let self_inner = self.clone();
          let tls_server_name_inner = tls_server_name.clone();
          self.globals.runtime_handle.spawn(async move {
            let fut = self_inner.h3_serve_stream(req, stream, client_addr, tls_server_name_inner);
            if let Some(connection_handling_timeout) = self_inner.globals.proxy_config.connection_handling_timeout {
              if let Err(e) = tokio::time::timeout(connection_handling_timeout, fut).await {
                warn!("HTTP/3 error on serve stream: {}", e);
              };
            } else if let Err(e) = fut.await {
              warn!("HTTP/3 error on serve stream: {}", e);
            }
            trace!("HTTP/3 request stream processed");
          });
        }
      }
    }

    Ok(())
  }

  /// Serves a request stream from a client
  /// Body in hyper-0.14 was changed to Incoming in hyper-1.0, and it is not accessible from outside.
  /// Thus, we needed to implement IncomingLike trait using channel. Also, the backend handler must feed the body in the form of
  /// Either<Incoming, IncomingLike> as body.
  async fn h3_serve_stream<S>(
    &self,
    req: Request<()>,
    mut stream: RequestStream<S, Bytes>,
    client_addr: SocketAddr,
    tls_server_name: ServerName,
  ) -> RpxyResult<()>
  where
    S: BidiStream<Bytes> + Send + 'static,
    <S as BidiStream<Bytes>>::RecvStream: Send,
  {
    let (mut req_parts, _) = req.into_parts();
    // RFC 9114 §4.1.2 / §4.2: h3 does not check these fields itself. A malformed request is a
    // stream error of type H3_MESSAGE_ERROR and is never handed to the message handler.
    let content_length = match validate_request_headers(&mut req_parts.headers) {
      Ok(content_length) => content_length,
      Err(reason) => {
        debug!("HTTP/3 malformed request from {}: {}", client_addr, reason);
        stream.stop_stream(Code::H3_MESSAGE_ERROR);
        stream.stop_sending(Code::H3_MESSAGE_ERROR);
        return Ok(());
      }
    };
    // split stream and async body handling
    let (mut send_stream, mut recv_stream) = stream.split();

    // generate streamed body with trailers using channel
    let (body_sender, req_body) = IncomingLike::channel();

    // Buffering and sending body through channel for protocol conversion like h3 -> h2/http1.1
    // The underlying buffering, i.e., buffer given by the API recv_data.await?, is handled by quinn.
    //
    // The top-level body-size limit applies uniformly to h1, h2, and h3. `None` means
    // unlimited and skips the count entirely. The pre-flight Content-Length check in
    // `handle_request_inner` covers CL-known oversize uploads with a clean 413; this loop
    // covers chunked / streaming overrun.
    let effective_body_limit = self.globals.proxy_config.request_max_body_size;
    // Set by the body task when the received DATA does not match the declared Content-Length,
    // so that the response side resets the stream instead of sending a response.
    let content_length_mismatch = Arc::new(AtomicBool::new(false));
    let content_length_mismatch_tx = content_length_mismatch.clone();
    self.globals.runtime_handle.spawn(async move {
      let mut sender = body_sender;
      // Counted as `u64` (the Content-Length range) so that it cannot wrap on 32-bit targets,
      // where a large upload would otherwise be miscounted as a Content-Length mismatch.
      let mut size = 0u64;
      // Received byte count that disagrees with the declared Content-Length (RFC 9114 §4.1.2).
      let mut mismatched_size = None;
      while let Some(mut body) = recv_stream.recv_data().await? {
        trace!("HTTP/3 incoming request body: remaining {}", body.remaining());
        // Checked before forwarding so that DATA beyond the declared length never reaches the upstream.
        let received = size.saturating_add(body.remaining() as u64);
        if content_length.is_some_and(|declared| received > declared) {
          mismatched_size = Some(received);
          break;
        }
        size = received;
        if let Some(limit) = effective_body_limit
          && size > limit as u64
        {
          error!(
            "Exceeds max request body size for HTTP/3: received {}, maximum_allowed {}",
            size, limit
          );
          // terminate the stream with an error code, and return an error to the caller.
          sender.abort();
          return Err(RpxyError::H3TooLargeBody);
        }
        // create stream body to save memory, shallow copy (increment of ref-count) to Bytes using copy_to_bytes
        sender.send_data(body.copy_to_bytes(body.remaining())).await?;
      }
      // DATA ended before reaching the declared length.
      if mismatched_size.is_none() && content_length.is_some_and(|declared| declared != size) {
        mismatched_size = Some(size);
      }
      if let (Some(declared), Some(received)) = (content_length, mismatched_size) {
        debug!(
          "HTTP/3 request body does not match content-length: declared {}, received {}",
          declared, received
        );
        // Publish the mismatch before aborting, so that the response side sees it no later than
        // the message handler observes the aborted upstream body.
        content_length_mismatch_tx.store(true, Ordering::SeqCst);
        recv_stream.stop_sending(Code::H3_MESSAGE_ERROR);
        sender.abort();
        return Err(RpxyError::H3ContentLengthMismatch);
      }

      // trailers: use inner for work around. (directly get trailer)
      let trailers = futures_util::future::poll_fn(|cx| recv_stream.as_mut().poll_recv_trailers(cx)).await?;
      if let Some(trailers) = trailers {
        trace!("HTTP/3 incoming request trailers");
        sender.send_trailers(trailers).await?;
      }
      Ok(()) as RpxyResult<()>
    });

    let new_req: Request<RequestBody> = Request::from_parts(req_parts, RequestBody::IncomingLike(req_body));
    let res = self
      .message_handler
      .handle_request(
        new_req,
        client_addr,
        self.listener_spec.listening_on,
        self.listener_spec.tls_enabled(),
        Some(tls_server_name),
      )
      .await?;

    // The request body did not match its Content-Length: treat it as a stream error instead of
    // sending the response, which is typically a 502 caused by the aborted upstream body. A
    // mismatch detected after this point only stops the receiving side.
    if content_length_mismatch.load(Ordering::SeqCst) {
      send_stream.stop_stream(Code::H3_MESSAGE_ERROR);
      return Ok(());
    }

    let (new_res_parts, mut new_body) = res.into_parts();
    let new_res = Response::from_parts(new_res_parts, ());

    match send_stream.send_response(new_res).await {
      Ok(_) => {
        trace!("HTTP/3 response to connection successful");
        // on-demand body streaming to downstream without expanding the object onto memory.
        loop {
          let frame = match new_body.frame().await {
            Some(frame) => frame,
            None => {
              trace!("Response body finished");
              break;
            }
          }
          .map_err(|e| RpxyError::HyperBodyManipulationError(e.to_string()))?;

          if frame.is_data() {
            let data = frame.into_data().unwrap_or_default();
            // debug!("Write data to HTTP/3 stream");
            send_stream.send_data(data).await?;
          } else if frame.is_trailers() {
            let trailers = frame.into_trailers().unwrap_or_default();
            // debug!("Write trailer to HTTP/3 stream");
            send_stream.send_trailers(trailers).await?;
          }
        }
      }
      Err(err) => {
        error!("Unable to send response to connection peer: {:?}", err);
      }
    }
    Ok(send_stream.finish().await?)
  }
}

/// Connection-specific header fields that must not appear in an HTTP/3 message (RFC 9114 §4.2).
static CONNECTION_SPECIFIC_HEADERS: [HeaderName; 5] = [
  header::CONNECTION,
  HeaderName::from_static("keep-alive"),
  HeaderName::from_static("proxy-connection"),
  header::TRANSFER_ENCODING,
  header::UPGRADE,
];

/// Validate the header section of an HTTP/3 request (RFC 9114 §4.1.2, §4.2).
///
/// Rejects connection-specific fields, a `te` other than `trailers`, and an invalid or
/// conflicting `content-length`. A valid `content-length` is rewritten to a single canonical
/// value, so later consumers (the pre-flight size check, the upstream request) never see
/// duplicates, list syntax, or surrounding whitespace. Returns the declared length, if any.
fn validate_request_headers(headers: &mut HeaderMap) -> Result<Option<u64>, &'static str> {
  if CONNECTION_SPECIFIC_HEADERS.iter().any(|name| headers.contains_key(name)) {
    return Err("connection-specific header field");
  }
  let te_is_trailers_only = headers.get_all(header::TE).iter().all(|value| {
    value
      .as_bytes()
      .split(|b| *b == b',')
      .map(|element| element.trim_ascii())
      .filter(|element| !element.is_empty())
      .all(|element| element.eq_ignore_ascii_case(b"trailers"))
  });
  if !te_is_trailers_only {
    return Err("te header field other than trailers");
  }

  let mut content_length = None;
  for value in headers.get_all(header::CONTENT_LENGTH) {
    for element in value.as_bytes().split(|b| *b == b',') {
      let n = parse_decimal(element.trim_ascii()).ok_or("invalid content-length")?;
      if content_length.is_some_and(|declared| declared != n) {
        return Err("conflicting content-length");
      }
      content_length = Some(n);
    }
  }
  if let Some(n) = content_length {
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(n));
  }
  Ok(content_length)
}

/// Parse `1*DIGIT` into `u64`; rejects empty input, signs, and overflow.
fn parse_decimal(digits: &[u8]) -> Option<u64> {
  if digits.is_empty() {
    return None;
  }
  digits.iter().try_fold(0u64, |acc, &b| {
    if !b.is_ascii_digit() {
      return None;
    }
    acc.checked_mul(10)?.checked_add(u64::from(b - b'0'))
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn headers_with(fields: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in fields {
      headers.append(*name, HeaderValue::from_static(value));
    }
    headers
  }

  #[test]
  fn content_length_absent_is_none() {
    let mut h = headers_with(&[("content-type", "text/plain")]);
    assert_eq!(validate_request_headers(&mut h), Ok(None));
    assert!(!h.contains_key(header::CONTENT_LENGTH));
  }

  #[test]
  fn content_length_single_value_is_returned() {
    let mut h = headers_with(&[("content-length", "10")]);
    assert_eq!(validate_request_headers(&mut h), Ok(Some(10)));
    assert_eq!(h.get(header::CONTENT_LENGTH).unwrap(), "10");
  }

  /// Leading zeros and surrounding whitespace are rewritten to the canonical value, so the
  /// pre-flight size check (`str::parse::<u64>`) can always parse it.
  #[test]
  fn content_length_is_canonicalised() {
    for value in ["005", " 5", "5 ", "\t5"] {
      let mut h = headers_with(&[("content-length", value)]);
      assert_eq!(validate_request_headers(&mut h), Ok(Some(5)), "value {value:?}");
      assert_eq!(h.get(header::CONTENT_LENGTH).unwrap(), "5", "value {value:?}");
    }
  }

  /// RFC 9110 §8.6 allows identical duplicates; they collapse into a single field.
  #[test]
  fn identical_content_length_duplicates_are_normalised() {
    let mut fields = headers_with(&[("content-length", "5"), ("content-length", "5")]);
    assert_eq!(validate_request_headers(&mut fields), Ok(Some(5)));
    assert_eq!(fields.get_all(header::CONTENT_LENGTH).iter().count(), 1);

    let mut list = headers_with(&[("content-length", "5, 5")]);
    assert_eq!(validate_request_headers(&mut list), Ok(Some(5)));
    assert_eq!(list.get(header::CONTENT_LENGTH).unwrap(), "5");
  }

  #[test]
  fn conflicting_content_length_is_rejected() {
    let mut fields = headers_with(&[("content-length", "5"), ("content-length", "10")]);
    assert_eq!(validate_request_headers(&mut fields), Err("conflicting content-length"));

    let mut list = headers_with(&[("content-length", "5, 6")]);
    assert_eq!(validate_request_headers(&mut list), Err("conflicting content-length"));
  }

  #[test]
  fn invalid_content_length_is_rejected() {
    for value in ["abc", "+5", "-5", "", "5,", "5 0", "0x10", "18446744073709551616"] {
      let mut h = headers_with(&[("content-length", value)]);
      assert_eq!(
        validate_request_headers(&mut h),
        Err("invalid content-length"),
        "value {value:?}"
      );
    }
    let mut obs_text = HeaderMap::new();
    obs_text.insert(header::CONTENT_LENGTH, HeaderValue::from_bytes(b"5\xff").unwrap());
    assert_eq!(validate_request_headers(&mut obs_text), Err("invalid content-length"));
  }

  #[test]
  fn max_u64_content_length_is_accepted() {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(u64::MAX));
    assert_eq!(validate_request_headers(&mut h), Ok(Some(u64::MAX)));
  }

  #[test]
  fn connection_specific_fields_are_rejected() {
    for name in ["connection", "keep-alive", "proxy-connection", "transfer-encoding", "upgrade"] {
      let mut h = headers_with(&[(name, "x")]);
      assert_eq!(
        validate_request_headers(&mut h),
        Err("connection-specific header field"),
        "field {name}"
      );
    }
  }

  #[test]
  fn te_trailers_is_accepted() {
    for value in ["trailers", "Trailers", " trailers "] {
      let mut h = headers_with(&[("te", value)]);
      assert_eq!(validate_request_headers(&mut h), Ok(None), "value {value:?}");
    }
    let mut repeated = headers_with(&[("te", "trailers"), ("te", "trailers")]);
    assert_eq!(validate_request_headers(&mut repeated), Ok(None));
  }

  #[test]
  fn te_other_than_trailers_is_rejected() {
    for value in ["gzip", "trailers, gzip", "trailers;q=1"] {
      let mut h = headers_with(&[("te", value)]);
      assert_eq!(
        validate_request_headers(&mut h),
        Err("te header field other than trailers"),
        "value {value:?}"
      );
    }
  }

  #[test]
  fn unrelated_headers_are_left_untouched() {
    let mut h = headers_with(&[("host", "example.com"), ("content-type", "text/plain"), ("cookie", "a=b")]);
    let before = h.clone();
    assert_eq!(validate_request_headers(&mut h), Ok(None));
    assert_eq!(h, before);
  }
}

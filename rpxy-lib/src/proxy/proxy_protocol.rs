use crate::globals::TcpRecvProxyProtocolConfig;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::{io::AsyncReadExt, net::TcpStream};
use tracing::{debug, trace};

/// v2 signature: 12-byte magic sequence
const V2_SIGNATURE: &[u8; 12] = b"\r\n\r\n\x00\r\nQUIT\n";
/// v2 fixed header size (signature + version/command + family/protocol + addr_len)
const V2_HEADER_FIXED_SIZE: usize = 16;
/// v1 prefix "PROXY "
const V1_PREFIX: &[u8; 6] = b"PROXY ";
/// v1 maximum header length per spec (including \r\n)
const V1_MAX_LENGTH: usize = 107;
/// Interval between peek retries when waiting for enough bytes
const PEEK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

/// Normalize an IPv4-mapped IPv6 address to plain IPv4.
///
/// When a dual-stack listener binds to [::] and accepts an IPv4 connection,
/// the peer_addr may be an IPv4-mapped IPv6 address (e.g., ::ffff:10.0.0.1).
fn normalize_mapped_ipv4(addr: IpAddr) -> IpAddr {
  match addr {
    IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
      Some(v4) => IpAddr::V4(v4),
      None => IpAddr::V6(v6),
    },
    other => other,
  }
}

/// Parse an inbound PROXY protocol header from the stream.
///
/// **I/O contract**: This function consumes exactly the PROXY header bytes
/// from the stream and nothing more. After this function returns, the stream
/// is positioned at the first byte of application data.
///
/// Returns:
/// - `Ok(Some(src_addr))` if a PROXY command header was parsed (replace src_addr)
/// - `Ok(None)` if a LOCAL/UNKNOWN command was parsed (keep original src_addr)
/// - `Err(std::io::Error)` on untrusted source, malformed header, or I/O error
pub(crate) async fn parse_inbound_proxy_header(
  stream: &mut TcpStream,
  peer_addr: &SocketAddr,
  config: &TcpRecvProxyProtocolConfig,
) -> Result<Option<SocketAddr>, std::io::Error> {
  // 1. Validate peer_addr against trusted_proxies
  let normalized_peer_ip = normalize_mapped_ipv4(peer_addr.ip());
  if !config.trusted_proxies.iter().any(|net| net.contains(&normalized_peer_ip)) {
    return Err(std::io::Error::new(
      std::io::ErrorKind::PermissionDenied,
      format!("PROXY header from untrusted source: {peer_addr}"),
    ));
  }

  // 2. Peek first 16 bytes to determine version.
  //    TCP peek may return fewer bytes than available in the receive buffer,
  //    so retry with a short sleep until we have enough bytes for version detection.
  let mut peek_buf = [0u8; V2_HEADER_FIXED_SIZE];
  let peeked = {
    loop {
      let n = stream.peek(&mut peek_buf).await?;
      // EOF: peer closed the connection with no data in buffer
      if n == 0 {
        return Err(std::io::Error::new(
          std::io::ErrorKind::UnexpectedEof,
          "Connection closed before PROXY header could be read",
        ));
      }
      if n >= V2_HEADER_FIXED_SIZE {
        break n;
      }
      // We need at least 6 bytes to distinguish v1 from v2.
      // If we have 6+ bytes and the prefix is clearly v1 ("PROXY "), we can proceed.
      if n >= 6 && peek_buf[..6] == *V1_PREFIX {
        break n;
      }
      // Deadline enforcement is handled by the outer tokio::time::timeout
      // in extract_parse_result_from_proxy_protocol_header().
      tokio::time::sleep(PEEK_RETRY_INTERVAL).await;
    }
  };

  // 3. Determine version and parse
  if peeked >= 12 && peek_buf[..12] == *V2_SIGNATURE {
    parse_v2_inbound(stream, &peek_buf).await
  } else if peek_buf[..6] == *V1_PREFIX {
    parse_v1_inbound(stream).await
  } else {
    Err(std::io::Error::new(
      std::io::ErrorKind::InvalidData,
      "No valid PROXY protocol signature detected",
    ))
  }
}

/// Parse a v2 (binary) PROXY protocol header.
/// The caller guarantees that peek_buf contains at least 16 bytes (V2_HEADER_FIXED_SIZE).
async fn parse_v2_inbound(
  stream: &mut TcpStream,
  peek_buf: &[u8; V2_HEADER_FIXED_SIZE],
) -> Result<Option<SocketAddr>, std::io::Error> {
  // Extract addr_len from bytes 14-15
  let addr_len = u16::from_be_bytes([peek_buf[14], peek_buf[15]]) as usize;
  let total_len = V2_HEADER_FIXED_SIZE + addr_len;

  // Read exactly the full header
  let mut header_buf = vec![0u8; total_len];
  stream.read_exact(&mut header_buf).await?;

  // Parse with ppp crate
  let header = ppp::v2::Header::try_from(header_buf.as_slice()).map_err(|e| {
    std::io::Error::new(
      std::io::ErrorKind::InvalidData,
      format!("Failed to parse PROXY v2 header: {e:?}"),
    )
  })?;

  // Check command type
  if header.command == ppp::v2::Command::Local {
    debug!("PROXY v2 LOCAL command received");
    return Ok(None);
  }

  // PROXY command - extract source address
  match header.addresses {
    ppp::v2::Addresses::IPv4(ipv4) => {
      let src = SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ipv4.source_address)), ipv4.source_port);
      trace!(
        "Parsed PROXY v2 IPv4 header: src={}, dst={}",
        src,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ipv4.destination_address)), ipv4.destination_port)
      );
      Ok(Some(src))
    }
    ppp::v2::Addresses::IPv6(ipv6) => {
      let src = SocketAddr::new(IpAddr::V6(Ipv6Addr::from(ipv6.source_address)), ipv6.source_port);
      trace!(
        "Parsed PROXY v2 IPv6 header: src={}, dst={}",
        src,
        SocketAddr::new(IpAddr::V6(Ipv6Addr::from(ipv6.destination_address)), ipv6.destination_port)
      );
      Ok(Some(src))
    }
    ppp::v2::Addresses::Unix(_) => Err(std::io::Error::new(
      std::io::ErrorKind::Unsupported,
      "Unix socket addresses not supported in PROXY protocol",
    )),
    ppp::v2::Addresses::Unspecified => {
      debug!("PROXY v2 unspecified addresses");
      Ok(None)
    }
  }
}

/// Parse a v1 (text) PROXY protocol header.
/// Reads byte-by-byte until \r\n to avoid over-consuming application data.
async fn parse_v1_inbound(stream: &mut TcpStream) -> Result<Option<SocketAddr>, std::io::Error> {
  let mut header_bytes = Vec::with_capacity(V1_MAX_LENGTH);
  let mut byte = [0u8; 1];
  let mut found_cr = false;

  loop {
    stream.read_exact(&mut byte).await?;
    header_bytes.push(byte[0]);

    if found_cr && byte[0] == b'\n' {
      break;
    }
    found_cr = byte[0] == b'\r';

    if header_bytes.len() >= V1_MAX_LENGTH {
      return Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "PROXY v1 header exceeds maximum length",
      ));
    }
  }

  let header = ppp::v1::Header::try_from(header_bytes.as_slice()).map_err(|e| {
    std::io::Error::new(
      std::io::ErrorKind::InvalidData,
      format!("Failed to parse PROXY v1 header: {e:?}"),
    )
  })?;

  match header.addresses {
    ppp::v1::Addresses::Tcp4(tcp4) => {
      let src = SocketAddr::new(IpAddr::V4(tcp4.source_address), tcp4.source_port);
      trace!(
        "Parsed PROXY v1 TCP4 header: src={}, dst={}",
        src,
        SocketAddr::new(IpAddr::V4(tcp4.destination_address), tcp4.destination_port)
      );
      Ok(Some(src))
    }
    ppp::v1::Addresses::Tcp6(tcp6) => {
      let src = SocketAddr::new(IpAddr::V6(tcp6.source_address), tcp6.source_port);
      trace!(
        "Parsed PROXY v1 TCP6 header: src={}, dst={}",
        src,
        SocketAddr::new(IpAddr::V6(tcp6.destination_address), tcp6.destination_port)
      );
      Ok(Some(src))
    }
    ppp::v1::Addresses::Unknown => {
      debug!("PROXY v1 UNKNOWN command received");
      Ok(None)
    }
  }
}

/* ---------------------------------------------------------- */

#[cfg(test)]
mod tests {
  use super::*;
  use ipnet::IpNet;
  use tokio::io::AsyncWriteExt;
  use tokio::net::TcpListener;

  /// Helper: create a connected TcpStream pair with given data written from the "client" side.
  /// Returns the server-side stream (for parsing) and the client-side stream.
  async fn setup_stream_with_data(data: &[u8]) -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(addr).await.unwrap();
    let (server, _) = listener.accept().await.unwrap();
    client.write_all(data).await.unwrap();
    (server, client)
  }

  fn trusted_config(cidrs: &[&str]) -> TcpRecvProxyProtocolConfig {
    TcpRecvProxyProtocolConfig {
      trusted_proxies: cidrs.iter().map(|c| c.parse::<IpNet>().unwrap()).collect(),
      timeout: std::time::Duration::from_millis(50),
    }
  }

  /// Build a minimal v2 PROXY header for IPv4 src/dst
  fn build_v2_proxy_ipv4(src: SocketAddr, dst: SocketAddr) -> Vec<u8> {
    let (src_ip, dst_ip) = match (src.ip(), dst.ip()) {
      (IpAddr::V4(s), IpAddr::V4(d)) => (s, d),
      _ => panic!("expected IPv4"),
    };
    let addresses: ppp::v2::Addresses = ppp::v2::IPv4::new(src_ip, dst_ip, src.port(), dst.port()).into();
    let version_command = ppp::v2::Version::Two | ppp::v2::Command::Proxy;
    ppp::v2::Builder::with_addresses(version_command, ppp::v2::Protocol::Stream, addresses)
      .build()
      .unwrap()
  }

  /// Build a minimal v2 PROXY header for IPv6 src/dst
  fn build_v2_proxy_ipv6(src: SocketAddr, dst: SocketAddr) -> Vec<u8> {
    let (src_ip, dst_ip) = match (src.ip(), dst.ip()) {
      (IpAddr::V6(s), IpAddr::V6(d)) => (s, d),
      _ => panic!("expected IPv6"),
    };
    let addresses: ppp::v2::Addresses = ppp::v2::IPv6::new(src_ip, dst_ip, src.port(), dst.port()).into();
    let version_command = ppp::v2::Version::Two | ppp::v2::Command::Proxy;
    ppp::v2::Builder::with_addresses(version_command, ppp::v2::Protocol::Stream, addresses)
      .build()
      .unwrap()
  }

  /// Build a v2 LOCAL header (no addresses)
  fn build_v2_local_header() -> Vec<u8> {
    let version_command = ppp::v2::Version::Two | ppp::v2::Command::Local;
    ppp::v2::Builder::with_addresses(version_command, ppp::v2::Protocol::Stream, ppp::v2::Addresses::Unspecified)
      .build()
      .unwrap()
  }

  // --- Trusted proxy validation tests ---

  #[tokio::test]
  async fn test_trusted_proxy_reject_untrusted() {
    let data = b"PROXY TCP4 1.2.3.4 5.6.7.8 1234 80\r\n";
    let (mut server, _client) = setup_stream_with_data(data).await;
    let peer: SocketAddr = "99.99.99.99:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    let result = parse_inbound_proxy_header(&mut server, &peer, &config).await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
  }

  #[tokio::test]
  async fn test_trusted_proxy_accept_cidr() {
    let data = b"PROXY TCP4 1.2.3.4 5.6.7.8 1234 80\r\n";
    let (mut server, _client) = setup_stream_with_data(data).await;
    let peer: SocketAddr = "10.1.2.3:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    let result = parse_inbound_proxy_header(&mut server, &peer, &config).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().unwrap(), "1.2.3.4:1234".parse::<SocketAddr>().unwrap());
  }

  #[tokio::test]
  async fn test_trusted_proxy_ipv4_mapped_ipv6() {
    // Peer is ::ffff:10.0.0.1 (IPv4-mapped), trusted CIDR is 10.0.0.0/8
    let data = b"PROXY TCP4 1.2.3.4 5.6.7.8 1234 80\r\n";
    let (mut server, _client) = setup_stream_with_data(data).await;
    let mapped_ip = Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped();
    let peer = SocketAddr::new(IpAddr::V6(mapped_ip), 9999);
    let config = trusted_config(&["10.0.0.0/8"]);

    let result = parse_inbound_proxy_header(&mut server, &peer, &config).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().unwrap(), "1.2.3.4:1234".parse::<SocketAddr>().unwrap());
  }

  // --- v1 parsing tests ---

  #[tokio::test]
  async fn test_v1_tcp4() {
    let data = b"PROXY TCP4 192.168.1.100 10.0.0.1 45000 443\r\n";
    let (mut server, _client) = setup_stream_with_data(data).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    let src = parse_inbound_proxy_header(&mut server, &peer, &config)
      .await
      .unwrap()
      .unwrap();
    assert_eq!(src, "192.168.1.100:45000".parse::<SocketAddr>().unwrap());
  }

  #[tokio::test]
  async fn test_v1_tcp6() {
    let data = b"PROXY TCP6 2001:db8::1 2001:db8::2 45000 443\r\n";
    let (mut server, _client) = setup_stream_with_data(data).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    let src = parse_inbound_proxy_header(&mut server, &peer, &config)
      .await
      .unwrap()
      .unwrap();
    assert_eq!(src, "[2001:db8::1]:45000".parse::<SocketAddr>().unwrap());
  }

  #[tokio::test]
  async fn test_v1_unknown() {
    let data = b"PROXY UNKNOWN\r\n";
    let (mut server, _client) = setup_stream_with_data(data).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    assert_eq!(parse_inbound_proxy_header(&mut server, &peer, &config).await.unwrap(), None);
  }

  #[tokio::test]
  async fn test_v1_exact_byte_consumption() {
    let app_data = b"GET / HTTP/1.1\r\n";
    let mut full_data = b"PROXY TCP4 1.2.3.4 5.6.7.8 1234 80\r\n".to_vec();
    full_data.extend_from_slice(app_data);

    let (mut server, _client) = setup_stream_with_data(&full_data).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    parse_inbound_proxy_header(&mut server, &peer, &config).await.unwrap();

    // Read remaining application data — must match exactly
    let mut remaining = vec![0u8; app_data.len()];
    server.read_exact(&mut remaining).await.unwrap();
    assert_eq!(&remaining, app_data);
  }

  // --- v2 parsing tests ---

  #[tokio::test]
  async fn test_v2_proxy_ipv4() {
    let header = build_v2_proxy_ipv4("192.168.1.100:45000".parse().unwrap(), "10.0.0.1:443".parse().unwrap());
    let (mut server, _client) = setup_stream_with_data(&header).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    let src = parse_inbound_proxy_header(&mut server, &peer, &config)
      .await
      .unwrap()
      .unwrap();
    assert_eq!(src, "192.168.1.100:45000".parse::<SocketAddr>().unwrap());
  }

  #[tokio::test]
  async fn test_v2_proxy_ipv6() {
    let header = build_v2_proxy_ipv6("[2001:db8::1]:45000".parse().unwrap(), "[2001:db8::2]:443".parse().unwrap());
    let (mut server, _client) = setup_stream_with_data(&header).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    let src = parse_inbound_proxy_header(&mut server, &peer, &config)
      .await
      .unwrap()
      .unwrap();
    assert_eq!(src, "[2001:db8::1]:45000".parse::<SocketAddr>().unwrap());
  }

  #[tokio::test]
  async fn test_v2_local() {
    let header = build_v2_local_header();
    let (mut server, _client) = setup_stream_with_data(&header).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    assert_eq!(parse_inbound_proxy_header(&mut server, &peer, &config).await.unwrap(), None);
  }

  #[tokio::test]
  async fn test_v2_exact_byte_consumption() {
    let app_data = b"\x16\x03\x01\x00\x05hello";
    let mut header = build_v2_proxy_ipv4("192.168.1.100:45000".parse().unwrap(), "10.0.0.1:443".parse().unwrap());
    header.extend_from_slice(app_data);

    let (mut server, _client) = setup_stream_with_data(&header).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    parse_inbound_proxy_header(&mut server, &peer, &config).await.unwrap();

    // Read remaining application data — must match exactly
    let mut remaining = vec![0u8; app_data.len()];
    server.read_exact(&mut remaining).await.unwrap();
    assert_eq!(&remaining, app_data);
  }

  // --- Error cases ---

  #[tokio::test]
  async fn test_malformed_header() {
    let data = b"NOT_A_PROXY_HEADER\r\n";
    let (mut server, _client) = setup_stream_with_data(data).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    let err = parse_inbound_proxy_header(&mut server, &peer, &config).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
  }

  #[tokio::test]
  async fn test_eof_no_data() {
    // Peer closes immediately without sending any data
    let (mut server, mut client) = setup_stream_with_data(b"").await;
    client.shutdown().await.unwrap();
    // Small delay to allow the FIN to propagate
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    let err = parse_inbound_proxy_header(&mut server, &peer, &config).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
  }

  #[tokio::test]
  async fn test_partial_data_then_close() {
    // Peer sends partial data then closes — the outer timeout (in proxy_main.rs)
    // handles this case. Here we verify the parse doesn't complete successfully.
    let data = b"PRO";
    let (mut server, mut client) = setup_stream_with_data(data).await;
    client.shutdown().await.unwrap();
    let peer: SocketAddr = "10.0.0.2:9999".parse().unwrap();
    let config = trusted_config(&["10.0.0.0/8"]);

    // Wrap with a short timeout to avoid waiting for the full PEEK_NO_PROGRESS_TIMEOUT (5s)
    let result = tokio::time::timeout(
      std::time::Duration::from_millis(200),
      parse_inbound_proxy_header(&mut server, &peer, &config),
    )
    .await;

    // Either the outer timeout fires or the inner no-progress detection fires — both are acceptable
    match result {
      Err(_elapsed) => {} // outer timeout fired
      Ok(Err(e)) => assert!(
        e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::UnexpectedEof,
        "Unexpected error kind: {e:?}"
      ),
      Ok(Ok(_)) => panic!("Expected error for partial PROXY header, got success"),
    }
  }
}

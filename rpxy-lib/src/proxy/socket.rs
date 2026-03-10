use crate::{error::*, log::*};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
#[cfg(feature = "http3-quinn")]
use std::net::UdpSocket;
use tokio::net::TcpSocket;

/// Bind TCP socket to the given `SocketAddr`, and returns the TCP socket with `SO_REUSEADDR` and `SO_REUSEPORT` options.
/// This option is required to re-bind the socket address when the proxy instance is reconstructed.
/// Mostly imported from tokio::net::tcp::socket::TcpSocket
pub(super) fn bind_tcp_socket(listening_on: &SocketAddr) -> RpxyResult<TcpSocket> {
  let domain = listening_on.is_ipv6().then(|| Domain::IPV6).unwrap_or(Domain::IPV4);
  let ty = socket2::Type::STREAM;
  #[cfg(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
  ))]
  let ty = ty.nonblocking();
  let socket = Socket::new(domain, ty, Some(Protocol::TCP))?;
  // TODO: for future update with address binding without dual stack
  // if listening_on.is_ipv6() {
  //   socket.set_only_v6(true)?;
  // }
  #[cfg(not(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "fuchsia",
    target_os = "illumos",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
  )))]
  socket.set_nonblocking(true)?;

  let tcp_stream = std::net::TcpStream::from(socket);
  let tcp_socket = TcpSocket::from_std_stream(tcp_stream);
  tcp_socket.set_reuseaddr(true)?;
  #[cfg(not(any(target_os = "solaris", target_os = "illumos", target_os = "cygwin", target_os = "wasi")))]
  tcp_socket.set_reuseport(true)?;

  tcp_socket.bind(*listening_on).map_err(|e| {
    error!("Failed to bind TCP socket: {}", e);
    RpxyError::Io(e)
  })?;

  Ok(tcp_socket)
}

#[cfg(feature = "http3-quinn")]
/// Bind UDP socket to the given `SocketAddr`, and returns the UDP socket with `SO_REUSEADDR` and `SO_REUSEPORT` options.
/// This option is required to re-bind the socket address when the proxy instance is reconstructed.
pub(super) fn bind_udp_socket(listening_on: &SocketAddr) -> RpxyResult<UdpSocket> {
  let domain = listening_on.is_ipv6().then(|| Domain::IPV6).unwrap_or(Domain::IPV4);
  let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
  socket.set_nonblocking(true)?; // This was made true inside quinn. so this line isn't necessary here. but just in case.
  socket.set_reuse_address(true)?; // This isn't necessary?
  #[cfg(not(any(target_os = "solaris", target_os = "illumos", target_os = "cygwin", target_os = "wasi")))]
  socket.set_reuse_port(true)?;

  socket.bind(&(*listening_on).into()).map_err(|e| {
    error!("Failed to bind UDP socket: {}", e);
    RpxyError::Io(e)
  })?;

  Ok(socket.into())
}

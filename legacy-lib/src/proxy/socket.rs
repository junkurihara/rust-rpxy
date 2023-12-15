use crate::{error::*, log::*};
#[cfg(feature = "http3-quinn")]
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
#[cfg(feature = "http3-quinn")]
use std::net::UdpSocket;
use tokio::net::TcpSocket;

/// Bind TCP socket to the given `SocketAddr`, and returns the TCP socket with `SO_REUSEADDR` and `SO_REUSEPORT` options.
/// This option is required to re-bind the socket address when the proxy instance is reconstructed.
pub(super) fn bind_tcp_socket(listening_on: &SocketAddr) -> Result<TcpSocket> {
  let tcp_socket = if listening_on.is_ipv6() {
    TcpSocket::new_v6()
  } else {
    TcpSocket::new_v4()
  }?;
  tcp_socket.set_reuseaddr(true)?;
  tcp_socket.set_reuseport(true)?;
  if let Err(e) = tcp_socket.bind(*listening_on) {
    error!("Failed to bind TCP socket: {}", e);
    return Err(RpxyError::Io(e));
  };
  Ok(tcp_socket)
}

#[cfg(feature = "http3-quinn")]
/// Bind UDP socket to the given `SocketAddr`, and returns the UDP socket with `SO_REUSEADDR` and `SO_REUSEPORT` options.
/// This option is required to re-bind the socket address when the proxy instance is reconstructed.
pub(super) fn bind_udp_socket(listening_on: &SocketAddr) -> Result<UdpSocket> {
  let socket = if listening_on.is_ipv6() {
    Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))
  } else {
    Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
  }?;
  socket.set_reuse_address(true)?; // This isn't necessary?
  socket.set_reuse_port(true)?;
  socket.set_nonblocking(true)?; // This was made true inside quinn. so this line isn't necessary here. but just in case.

  if let Err(e) = socket.bind(&(*listening_on).into()) {
    error!("Failed to bind UDP socket: {}", e);
    return Err(RpxyError::Io(e));
  };
  let udp_socket: UdpSocket = socket.into();

  Ok(udp_socket)
}

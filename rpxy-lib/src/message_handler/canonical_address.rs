use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Trait to convert an IP address to its canonical form
pub trait ToCanonical {
  fn to_canonical(&self) -> Self;
}

impl ToCanonical for SocketAddr {
  fn to_canonical(&self) -> Self {
    match self {
      SocketAddr::V4(_) => *self,
      SocketAddr::V6(v6) => match v6.ip().to_ipv4() {
        Some(mapped) => {
          if mapped == Ipv4Addr::new(0, 0, 0, 1) {
            *self
          } else {
            SocketAddr::new(IpAddr::V4(mapped), self.port())
          }
        }
        None => *self,
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::net::Ipv6Addr;
  #[test]
  fn ipv4_loopback_to_canonical() {
    let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
    assert_eq!(socket.to_canonical(), socket);
  }
  #[test]
  fn ipv6_loopback_to_canonical() {
    let socket = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 8080);
    assert_eq!(socket.to_canonical(), socket);
  }
  #[test]
  fn ipv4_to_canonical() {
    let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 8080);
    assert_eq!(socket.to_canonical(), socket);
  }
  #[test]
  fn ipv6_to_canonical() {
    let socket = SocketAddr::new(
      IpAddr::V6(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0xdead, 0xbeef)),
      8080,
    );
    assert_eq!(socket.to_canonical(), socket);
  }
  #[test]
  fn ipv4_mapped_to_ipv6_to_canonical() {
    let socket = SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc00a, 0x2ff)), 8080);
    assert_eq!(
      socket.to_canonical(),
      SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 10, 2, 255)), 8080)
    );
  }
}

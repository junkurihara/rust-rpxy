use crate::{acceptor::PacketAcceptor, error::*, globals::Globals, log::*};
use futures::future::select_all;
use hyper::Client;
#[cfg(feature = "forward-hyper-trust-dns")]
use hyper_trust_dns::TrustDnsResolver;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Proxy {
  pub globals: Arc<Globals>,
}
impl Proxy {
  pub async fn entrypoint(self) -> Result<()> {
    let addresses = self.globals.listen_addresses.clone();
    let futures = select_all(addresses.into_iter().map(|addr| {
      info!("Listen address: {:?}", addr);

      #[cfg(feature = "forward-hyper-trust-dns")]
      let connector = TrustDnsResolver::default().into_rustls_webpki_https_connector();
      #[cfg(not(feature = "forward-hyper-trust-dns"))]
      let connector = hyper_tls::HttpsConnector::new();
      let forwarder = Arc::new(Client::builder().build::<_, hyper::Body>(connector));

      let acceptor = PacketAcceptor {
        listening_on: addr,
        globals: self.globals.clone(),
        forwarder,
      };
      self.globals.runtime_handle.spawn(acceptor.start())
    }));

    // wait for all future
    if let (Ok(_), _, _) = futures.await {
      error!("Some packet acceptors are down");
    };

    Ok(())
  }
}

use crate::{acceptor::PacketAcceptor, error::*, globals::Globals, log::*};
use futures::future::select_all;
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
      let acceptor = PacketAcceptor {
        listening_on: addr,
        globals: self.globals.clone(),
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

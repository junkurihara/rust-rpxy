mod certs;
mod service;

use crate::{
  backend::BackendAppManager,
  constants::{CERTS_WATCH_DELAY_SECS, LOAD_CERTS_ONLY_WHEN_UPDATED},
  error::RpxyResult,
};
use hot_reload::{ReloaderReceiver, ReloaderService};
use service::CryptoReloader;
use std::sync::Arc;

pub use certs::{CertsAndKeys, CryptoSource};
pub use service::{ServerCrypto, ServerCryptoBase, SniServerCryptoMap};

/// Result type inner of certificate reloader service
type ReloaderServiceResultInner<T> = (
  ReloaderService<CryptoReloader<T>, ServerCryptoBase>,
  ReloaderReceiver<ServerCryptoBase>,
);
/// Build certificate reloader service
pub(crate) async fn build_cert_reloader<T>(
  app_manager: &Arc<BackendAppManager<T>>,
) -> RpxyResult<ReloaderServiceResultInner<T>>
where
  T: CryptoSource + Clone + Send + Sync + 'static,
{
  let (cert_reloader_service, cert_reloader_rx) = ReloaderService::<
    service::CryptoReloader<T>,
    service::ServerCryptoBase,
  >::new(
    app_manager, CERTS_WATCH_DELAY_SECS, !LOAD_CERTS_ONLY_WHEN_UPDATED
  )
  .await?;
  Ok((cert_reloader_service, cert_reloader_rx))
}

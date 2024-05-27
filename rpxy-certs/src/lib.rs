mod certs;
mod crypto_source;
mod error;
mod reloader_service;
mod server_crypto;

#[allow(unused_imports)]
mod log {
  pub(crate) use tracing::{debug, error, info, warn};
}
/* ------------------------------------------------ */
pub use crate::{
  certs::SingleServerCertsKeys,
  crypto_source::{CryptoFileSource, CryptoFileSourceBuilder, CryptoFileSourceBuilderError, CryptoSource},
  server_crypto::{ServerCrypto, ServerNameBytes, ServerNameCryptoMap},
};

use crate::{error::*, reloader_service::CryptoReloader, server_crypto::ServerCryptoBase};
use hot_reload::{ReloaderReceiver, ReloaderService};

/* ------------------------------------------------ */
/// Constants TODO: define from outside
const CERTS_WATCH_DELAY_SECS: u32 = 60;
const LOAD_CERTS_ONLY_WHEN_UPDATED: bool = true;

/* ------------------------------------------------ */
/// Result type inner of certificate reloader service
type ReloaderServiceResultInner = (
  ReloaderService<CryptoReloader, ServerCryptoBase>,
  ReloaderReceiver<ServerCryptoBase>,
);
/// Build certificate reloader service
pub async fn build_cert_reloader() -> Result<ReloaderServiceResultInner, RpxyCertError>
// where
//   T: CryptoSource + Clone + Send + Sync + 'static,
{
  // TODO: fix later
  let source = rustc_hash::FxHashMap::default();

  let (cert_reloader_service, cert_reloader_rx) =
    ReloaderService::<CryptoReloader, ServerCryptoBase>::new(&source, CERTS_WATCH_DELAY_SECS, !LOAD_CERTS_ONLY_WHEN_UPDATED)
      .await?;
  Ok((cert_reloader_service, cert_reloader_rx))
}

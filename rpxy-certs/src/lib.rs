mod certs;
mod crypto_source;
mod error;
mod reloader_service;
mod server_crypto;

#[allow(unused_imports)]
mod log {
  pub(super) use tracing::{debug, error, info, warn};
}

use crate::{error::*, log::*, reloader_service::DynCryptoSource};
use hot_reload::{ReloaderReceiver, ReloaderService};
use rustc_hash::FxHashMap as HashMap;
use rustls::crypto::{aws_lc_rs, CryptoProvider};
use std::sync::Arc;

/* ------------------------------------------------ */
pub use crate::{
  certs::SingleServerCertsKeys,
  crypto_source::{CryptoFileSource, CryptoFileSourceBuilder, CryptoFileSourceBuilderError, CryptoSource},
  reloader_service::CryptoReloader,
  server_crypto::{ServerCrypto, ServerCryptoBase},
};

/* ------------------------------------------------ */
// Constants
/// Default delay in seconds to watch certificates
const DEFAULT_CERTS_WATCH_DELAY_SECS: u32 = 60;
/// Load certificates only when updated
const LOAD_CERTS_ONLY_WHEN_UPDATED: bool = true;

/// Result type inner of certificate reloader service
type ReloaderServiceResultInner = (
  ReloaderService<CryptoReloader, ServerCryptoBase>,
  ReloaderReceiver<ServerCryptoBase>,
);
/// Build certificate reloader service, which accepts a map of server names to `CryptoSource` instances
pub async fn build_cert_reloader<T>(
  crypto_source_map: &HashMap<String, T>,
  certs_watch_period: Option<u32>,
) -> Result<ReloaderServiceResultInner, RpxyCertError>
where
  T: CryptoSource<Error = RpxyCertError> + Send + Sync + Clone + 'static,
{
  info!("Building certificate reloader service");
  // Install aws_lc_rs as default crypto provider for rustls
  let _ = CryptoProvider::install_default(aws_lc_rs::default_provider());

  let source = crypto_source_map
    .iter()
    .map(|(k, v)| {
      let server_name_bytes = k.as_bytes().to_vec().to_ascii_lowercase();
      let dyn_crypto_source = Arc::new(Box::new(v.clone()) as Box<DynCryptoSource>);
      (server_name_bytes, dyn_crypto_source)
    })
    .collect::<HashMap<_, _>>();

  let certs_watch_period = certs_watch_period.unwrap_or(DEFAULT_CERTS_WATCH_DELAY_SECS);

  let (cert_reloader_service, cert_reloader_rx) =
    ReloaderService::<CryptoReloader, ServerCryptoBase>::new(&source, certs_watch_period, !LOAD_CERTS_ONLY_WHEN_UPDATED).await?;
  Ok((cert_reloader_service, cert_reloader_rx))
}

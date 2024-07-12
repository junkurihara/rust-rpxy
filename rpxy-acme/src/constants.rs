/// ACME directory url
pub const ACME_DIR_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// ACME registry path that stores account key and certificate
pub const ACME_REGISTRY_PATH: &str = "./acme_registry";

/// ACME accounts directory, subdirectory of ACME_REGISTRY_PATH
pub(crate) const ACME_ACCOUNT_SUBDIR: &str = "accounts";

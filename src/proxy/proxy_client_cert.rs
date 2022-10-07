use crate::{error::*, log::*};
use rustc_hash::FxHashSet as HashSet;
use rustls::Certificate;
use x509_parser::extensions::ParsedExtension;
use x509_parser::prelude::*;

// TODO: consider move this function to the layer of handle_request (L7) to return 403
pub(super) fn check_client_authentication(
  client_certs: Option<&[Certificate]>,
  client_certs_setting_for_sni: Option<&HashSet<Vec<u8>>>,
) -> Result<()> {
  if let Some(client_ca_keyids_set) = client_certs_setting_for_sni {
    if let Some(client_certs) = client_certs {
      debug!("Incoming TLS client is (temporarily) authenticated via client cert");
      // Check client certificate key ids

      let mut client_certs_parsed_iter = client_certs.iter().filter_map(|d| parse_x509_certificate(&d.0).ok());
      let match_server_crypto_and_client_cert = client_certs_parsed_iter.any(|c| {
        let mut filtered = c.1.iter_extensions().filter_map(|e| {
          if let ParsedExtension::AuthorityKeyIdentifier(key_id) = e.parsed_extension() {
            key_id.key_identifier.as_ref()
          } else {
            None
          }
        });

        filtered.any(|id| client_ca_keyids_set.contains(id.0))
      });
      if !match_server_crypto_and_client_cert {
        // TODO: return 403 here
        error!("Inconsistent client certificate for given server name");
        return Err(RpxyError::Proxy(
          "Inconsistent client certificate for given server name".to_string(),
        ));
      }
    } else {
      // TODO: return 403 here
      error!("Client certificate is needed for given server name");
      return Err(RpxyError::Proxy(
        "Client certificate is needed for given server name".to_string(),
      ));
    }
  }
  Ok(())
}

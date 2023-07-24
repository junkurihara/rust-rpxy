use crate::{error::*, log::*};
use rustc_hash::FxHashSet as HashSet;
use rustls::Certificate;
use x509_parser::extensions::ParsedExtension;
use x509_parser::prelude::*;

#[allow(dead_code)]
// TODO: consider move this function to the layer of handle_request (L7) to return 403
pub(super) fn check_client_authentication(
  client_certs: Option<&[Certificate]>,
  client_ca_keyids_set_for_sni: Option<&HashSet<Vec<u8>>>,
) -> std::result::Result<(), ClientCertsError> {
  let Some(client_ca_keyids_set) = client_ca_keyids_set_for_sni else {
    // No client cert settings for given server name
    return Ok(());
  };

  let Some(client_certs) = client_certs else {
    error!("Client certificate is needed for given server name");
    return Err(ClientCertsError::ClientCertRequired(
      "Client certificate is needed for given server name".to_string(),
    ));
  };
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
    error!("Inconsistent client certificate was provided for SNI");
    return Err(ClientCertsError::InconsistentClientCert(
      "Inconsistent client certificate was provided for SNI".to_string(),
    ));
  }

  Ok(())
}

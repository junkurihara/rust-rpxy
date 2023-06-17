# TODO List

- Improvement of path matcher
- More flexible option for rewriting path
- Refactoring

  Split `backend` module into three parts

  - backend(s): struct containing info, defined for each served domain with multiple paths
  - upstream/upstream group: information on targeted destinations for each set of (a domain + a path)
  - load-balance: load balancing mod for a domain + path

- Unit tests
- Options to serve custom http_error page.
- Prometheus metrics
- Documentation
- Client certificate
  - support intermediate certificate. Currently, only supports client certificates directly signed by root CA.
  - Currently, we took the following approach (caveats)
    - For Http2 and 1.1, prepare `rustls::ServerConfig` for each domain name and hence client CA cert is set for each one.
    - For Http3, use aggregated `rustls::ServerConfig` for multiple domain names except for ones requiring client-auth. So, if a domain name is set with client authentication, http3 doesn't work for the domain.
- Make the session-persistance option for load-balancing sophisticated. (mostly done in v0.3.0)
  - add option for sticky cookie name
  - add option for sticky cookie duration
- etc.

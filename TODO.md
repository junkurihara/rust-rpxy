# TODO List

- [ ] Realtime hot reload of configuration file with `hot_reload` crate v0.3.0 or higher
- [ ] Better documentation (incl. rpxy.io)
- [ ] Add more tests

## Planned (but pending) features

- We need more sophistication on `Forwarder` struct to handle `h2c`.
- Cache using `lru` crate might be inefficient in terms of the speed.
  - Consider more sophisticated architecture for cache
  - Persistent cache (if possible).
  - More secure cache file object naming
  - etc etc
- Improvement of path matcher
- More flexible option for rewriting path
- Refactoring

  - Split `backend` module into three parts

    - backend(s): struct containing info, defined for each served domain with multiple paths
    - upstream/upstream group: information on targeted destinations for each set of (a domain + a path)
    - load-balance: load balancing mod for a domain + path

- Options to serve custom http_error page.
- Traces and metrics using opentelemetry (`tracing-opentelemetry` crate)
- Client certificate
  - support intermediate certificate. Currently, only supports client certificates directly signed by root CA.
  - Currently, we took the following approach (caveats)
    - For Http2 and 1.1, prepare `rustls::ServerConfig` for each domain name and hence client CA cert is set for each one.
    - For Http3, use aggregated `rustls::ServerConfig` for multiple domain names except for ones requiring client-auth. So, if a domain name is set with client authentication, http3 doesn't work for the domain.
- Make the session-persistance option for load-balancing sophisticated. (mostly done in v0.3.0)
  - add option for sticky cookie name
  - add option for sticky cookie duration
- etc.

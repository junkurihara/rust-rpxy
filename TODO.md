# TODO List

## Ongoing

- [ ] Better documentation (incl. rpxy.io)
- [ ] Add more tests

## Planned / pending features

### Cache

- Persistent cache (if practical)
- Reconsider the on-memory store data structure (currently `lru` crate)

### Routing

- Improvement of the path matcher
  - Currently `HashMap<PathName, _>` + `max_by_key`; consider trie / radix tree
- More flexible options for rewriting the request path

### Load balancing (`sticky-cookie` feature)

- Make the sticky cookie name configurable (currently hard-coded)
- Make the sticky cookie duration configurable (currently 300 s constant)

### TLS / client certificates

- Support intermediate certificates (currently only client certificates directly signed by the root CA are supported)
- Lift the HTTP/3 + client-authentication limitation
  - HTTP/2 and HTTP/1.1 use a per-domain `rustls::ServerConfig`, so client-auth can be configured per domain.
  - HTTP/3 currently uses an aggregated `rustls::ServerConfig` for all non-client-auth domains, so a domain configured with client authentication cannot be served over HTTP/3.

### Observability

- Traces and metrics via OpenTelemetry (`tracing-opentelemetry` crate)

### Misc

- Options to serve a custom HTTP error page

# TODO List

- [Done in 0.6.0] But we need more sophistication on `Forwarder` struct. ~~Fix strategy for `h2c` requests on forwarded requests upstream. This needs to update forwarder definition. Also, maybe forwarder would have a cache corresponding to the following task.~~
- [Try in v0.6.0] **Cache option for the response with `Cache-Control: public` header directive ([#55](https://github.com/junkurihara/rust-rpxy/issues/55))**
- Improvement of path matcher
- More flexible option for rewriting path
- Refactoring

  - Split `backend` module into three parts

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

- Done in v0.5.0 ~~Use `gchr.io`~~
- Done in v0.5.0:
  ~~Consideration on migrating from `quinn` and `h3-quinn` to other QUIC implementations ([#57](https://github.com/junkurihara/rust-rpxy/issues/57))~~
- Done in v0.4.0:
  ~~Benchmark with other reverse proxy implementations like Sozu ([#58](https://github.com/junkurihara/rust-rpxy/issues/58)) Currently, Sozu can work only on `amd64` format due to its HTTP message parser limitation... Since the main developer have only `arm64` (Apple M1) laptops, so we should do that on VPS?~~
- Done in v0.4.0:
  ~~Split `rpxy` source codes into `rpxy-lib` and `rpxy-bin` to make the core part (reverse proxy) isolated from the misc part like toml file loader. This is in order to make the configuration-related part more flexible (related to [#33](https://github.com/junkurihara/rust-rpxy/issues/33))~~
- Done in 0.6.0:
  ~~Fix dynamic reloading of configuration file~~

- etc.

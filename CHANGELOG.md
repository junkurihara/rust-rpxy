# CHANGELOG

## 0.12.1 or 0.13.0 (Unreleased)

## 0.12.0 (To be released shortly)

**Security-focused release with the following improvements and bugfixes.**

### Important Changes

- **Breaking: add `trusted_forwarded_proxies` global option.** This supports deployments where rpxy runs behind another load balancer or reverse proxy that adds `X-Forwarded-For`, `Forwarded`, and related forwarding headers, and those headers should be trusted only when the immediate peer is within explicitly trusted proxy ranges. From this version, no proxy is trusted by default, so requests forwarded from rpxy to backend applications are rebuilt from the immediate peer only. When `trusted_forwarded_proxies` is configured with trusted CIDR blocks, rpxy preserves and normalizes forwarding information learned through those trusted proxies, rewrites outgoing `X-Forwarded-For` and related headers from that normalized chain, and falls back safely when the incoming forwarding view is malformed, inconsistent, or cannot be represented safely.
- Add `cloudflare`, `fastly` and `cloudfront` as a built-in `trusted_forwarded_proxies` alias and add the `rpxy-trusted-proxies` snapshot updater command for explicit provider range refreshes.
- **Breaking: harden `default_app` fallback against untrusted `Host` headers.** When a request matches the `default_app` fallback (i.e., its `Host` does not match any configured `server_name`), rpxy now force-overwrites the outgoing `Host` header with the default app's configured `server_name` regardless of the `keep_original_host` / `set_upstream_host` upstream options. In addition, the `default_app` fallback is now strictly limited to plaintext HTTP; TLS requests with an unknown server name are rejected unconditionally (independent of `sni_consistency`).
- **Sticky cookie security attributes.** The `Set-Cookie` issued by the sticky-session load balancer now always carries `HttpOnly` and `SameSite=Lax`, and additionally carries `Secure` when the client-visible request scheme is HTTPS. Operator-visible behavior changes:
  - Applications that previously read rpxy's sticky cookie from JavaScript (`document.cookie`) will no longer see it.
  - When rpxy itself terminates TLS, `Secure` is set automatically.
  - When rpxy runs behind an external TLS terminator (ALB, CloudFront, Nginx, HAProxy, etc.), the terminator's address must be listed in `trusted_forwarded_proxies` for `Secure` to be applied; rpxy honors `X-Forwarded-Proto: https` (or `Forwarded: proto=https`) only from trusted peers.
  - **Operator requirement.** Any proxy listed in `trusted_forwarded_proxies` must overwrite or normalize incoming `X-Forwarded-Proto` rather than appending a client-supplied value (e.g. Nginx `proxy_set_header X-Forwarded-Proto $scheme;`). Otherwise an attacker upstream of the trusted proxy can spoof the forwarded scheme. ALB and CloudFront satisfy this by default. This is the same operator requirement that 0.12.0 introduced for `X-Forwarded-For` chains.
- **Breaking: sticky cookie values are now opaque AEAD blobs.** Deployments using `load_balance = "sticky"` must configure the new global `sticky_cookie_secret` option as an unpadded base64url-encoded 32-byte secret. The default cookie name changed from `rpxy_srv_id` to `rpxy_sticky_token`; the old name is no longer treated as rpxy's sticky cookie. The sealed token contains the backend identifier and an expiration timestamp mirrored with the cookie `expires` / `Max-Age` attributes; expired, malformed, plaintext, or wrong-secret cookies are ignored and reissued automatically. Rotating the secret intentionally resets sticky-session affinity. Replay remains possible only within the sealed expiration window, so sticky cookies must not be used for authentication decisions.
- **Dependency note:** the sticky-cookie AEAD implementation currently pins `aes-gcm = 0.11.0-rc.3` intentionally for the 0.11 AEAD nonce-generation API. This pre-release dependency must be re-evaluated, replaced with a final 0.11.x release, or explicitly re-approved before the release dependency freeze.
- Rebuild `X-Forwarded-Host` as part of the general forwarding-header policy. rpxy no longer forwards a client-supplied `X-Forwarded-Host` value as-is; instead it rebuilds `X-Forwarded-Host` from the original client-visible host, alongside the other authoritative `X-Forwarded-*` headers. As with `Forwarded: host=`, this value is observational only and must not be used for security decisions.
- Harden TLS private key file permissions on Unix-like systems. Newly-created ACME cache files are now created with mode `0600`, newly-created ACME cache directories with mode `0700`, and existing cache artifacts keep their current modes. Manually provisioned TLS private key files are also checked at load time; rpxy emits a `warn!` log when any group or other permission bit is set, while still loading the key for backward compatibility.
- **Redact sensitive headers in DEBUG request logs.** The `debug!` line that logs the request to be forwarded now masks the values of `Authorization`, `Cookie`, and `Proxy-Authorization` with a `<redacted>` placeholder (header names stay visible). For troubleshooting, redaction can be disabled by setting the environment variable `RPXY_UNSAFE_DEBUG_HEADERS` to `1`, `true`, or `yes`; the variable is read once at startup and emits a `warn!` when enabled. Do not leave it enabled in production. The unredacted values still only appear when `RUST_LOG=debug`.
- **Fix: preserve the case of the sticky cookie `path` attribute.** The sticky-session `Set-Cookie` previously lowercased its `path`, which could mis-scope the cookie and silently break stickiness on case-sensitive route paths. The `path` is now emitted verbatim (the cookie `domain` is still lowercased). Because the path is bound into the sealed token, sticky cookies issued for a mixed-case path before the upgrade are ignored once and reissued; all-lowercase paths are unaffected.
- **Validate `server_name` as a hostname.** Each app's `server_name` is now validated at startup and must be a syntactically valid hostname: dot-separated labels of 1-63 characters, each starting and ending with an alphanumeric and otherwise containing only alphanumerics and `-`, with a total length up to 253 ASCII characters. This is defense-in-depth, in particular for the ACME on-disk paths derived from `server_name`. Valid hostnames are unaffected, but a `server_name` that is not a valid hostname (containing path separators, `..`, wildcards `*`, underscores `_`, IPv6 literals, or non-ASCII characters) is now rejected at startup where it was previously accepted (IPv4 literals are still accepted).
- **Add optional per-IP connection limit.** A new global `max_clients_per_ip` option caps the number of concurrent connections from a single source IP, in addition to the existing global `max_clients`, so one source cannot exhaust the connection pool. It defaults to `0` (disabled), preserving existing behavior. The source IP is the immediate TCP/QUIC peer, or the real client address recovered from an inbound PROXY protocol header; it is not derived from `X-Forwarded-For` / `Forwarded`, so the limit is only meaningful when rpxy is the edge or inbound PROXY protocol is enabled (behind a bare L7 load balancer every connection collapses to the balancer's IP). For HTTP/1.1 and HTTP/2 the slot is reserved before the TLS handshake so handshake floods are bounded too; for HTTP/3 it caps QUIC connections per source IP, and a single IP's concurrent HTTP/3 request streams are then bounded by `max_clients_per_ip` times `[experimental.h3] max_concurrent_bidistream`.

### Improvement

- Document that `connection_handling_timeout = 0` (the default) means no forced timeout, and recommend a non-zero value in production unless long-lived connections (e.g. WebSocket) are required.
- deps and refactor

## 0.11.3

### Improvement

- Feat: Support `tcp` and `http` active health checks. This is to support the use case where rpxy needs to monitor the health of backend applications and avoid sending requests to unhealthy ones. To enable this feature, the `health-check` feature has to be enabled and the `health_check` option in the config file has to be specified for each reverse proxy backend group.

- Deps and refactor

## 0.11.2

### Improvement

- Feat: Support implementation of multiple address-binding: This is to support the use case where rpxy is used in a host with multiple network interfaces and needs to bind to multiple ones. Both `listen_address_v4` and `listen_address_v6` options in the config file accepts either a single address or a list of addresses.

- Deps and refactor

## 0.11.1

### Improvement

- Feat: Support specific listener address binding for both IPv4 and IPv6. This is to support the use case where rpxy is used in a host with multiple network interfaces and needs to bind to a specific one. To enable this feature, the `listen_address_v4` and `listen_address_v6` options in the config file have to be specified. If `listen_address_v6` is not specified and `listen_ipv6` is true, it binds to `::`. If `listen_address_v6` is not specified and `listen_ipv6` is false or undefined, IPv6 is disabled. (#239)

- Deps and refactor

## 0.11.0

### Improvement

- Feat: Support PROXY protocol for incoming TCP connections, i.e., HTTP/1.1 and HTTP/2. This is to support the use case where rpxy is used behind another load balancer or reverse proxy that supports PROXY protocol, e.g., rpxy-l4, AWS ELB, HAProxy, Nginx, etc. To enable this feature, the `proxy-protocol` feature has to be enabled and the `experimental.tcp_recv_proxy_protocol` option in the config file has to be specified. Note that this feature is only for incoming connections and does not affect outgoing connections towards backend applications. Also note that HTTP/3 (QUIC) is not supported for PROXY protocol since its underlying UDP is connectionless and does not fit the layer-4 connection-oriented nature of PROXY protocol.

- Deps and refactor

### Bugfix

- Fix: TLS listener hot-reload fix: Changed break to continue when certificate reload fails, allowing the listener to wait for ACME to provision certificates instead of stopping entirely (#454)
- Fix: Write permission preflight check: Added startup verification for ACME certificate directories to fail fast with clear error messages, preventing silent failures that waste ACME rate limits (#454)

## 0.10.4

### Improvement

- Deps and refactor

### Bugfix

- Fix: RFC compliance issue for the URL path string (#425)

## 0.10.3

### Improvement

- Feat: Update the reloading strategy for config toml from polling to realtime.
- Deps

### Bugfix

- Fix: Fix the bug that when only https_port is specified, rpxy does not start properly.

## 0.10.2

### Bugfix

- Fix: Fix the bug that the `forwarded_header` option does not work properly (`proto` param)

## 0.10.1

### Improvement

- Feat: Support `Forwarded` header in addition to `X-Forwarded-For` header. This is to support the standard forwarding header for reverse proxy applications (RFC 7239). Use the `forwarded_header` upstream option to enable this feature.
  By default, it is not appended to the outgoing header. However, if the incoming request has the forwarded header, it would be preserved and updated simultaneously with `x-forwarded-for` header. if both forwarded and x-forwarded-for headers exists (and they are inconsistent), x-forwarded-for is prioritized. This means that x-forwarded-for is first updated and it is then copied (overridden) to `for` param of forwarded header.
- Refactor: lots of minor improvements
- Deps

## 0.10.0

### Important Changes

- [Breaking] We removed non-`watch` execute option and enabled the dynamic reloading of the config file by default.
- We newly added `log-dir` execute option to specify the directory for `access.log`,`error.log` and `rpxy.log`. This is optional, and if not specified, the logs are written to the standard output by default.

### Improvement

- Refactor: lots of minor improvements
- Deps

## 0.9.7

### Improvement

- Feat: add version tag for docker images via github actions
- Feat: support gRPC: This makes rpxy to serve gRPC requests on the same port as HTTP and HTTPS, i.e., listen_port and listen_port_tls. This means that by using the different subdomain for HTTP(S) and gRPC, we can multiplex them on same ports without opening another port dedicated to gRPC. To this end, this update made the forwarder to force HTTP/2 for gRPC requests towards backend (gRPC) app.
- Deps and refactor

### Bugfix

- Fixed bug for the upstream option "force_http2_upstream"

### Other

- Tentative downgrade of github actions `runs-on` from ubuntu-latest to ubuntu-22.04.

## 0.9.6

### Improvement

- Feat: Change the default hashing algorithm for internal hashmaps and hashsets from FxHash to aHash. This change is to improve the security against HashDos attacks for colliding domain names and paths, and to improve the speed of hash operations for string keys (c.f., [the performance comparison](https://github.com/tkaitchuck/aHash/blob/master/compare/readme.md)).
- Deps and refactor

## 0.9.5

### Bugfix

- Fix docker image build options with `post-quantum` feature.

## 0.9.4

### Improvement

- Feat: Enable the hybrid post-quantum key exchange for TLS and QUIC with `X25519MLKEM768` by default.
- Deps and refactor

## 0.9.3

### Improvement

- Feat: Support post-quantum `X25519Kyber768Draft00` for incoming and outgoing TLS initiation. This is non-default feature [feature: `post-quantum`].
- Feat: emit WARN messages if there exist unused and unsupported options specified in configuration file.
- Docs: `rpxy.io` is now available for the official website of `rpxy`.
- Refactor: lots of minor improvements
- Deps

## 0.9.2

### Improvement

- Feat: Add Jenkins build pipeline (#182)
- Refactor: lots of minor improvements
- BugFix: Fix the bug related to the installation of `CryptoProvider` (#194)
- BugFix: h3 header to use https_redirection_port (#192)
- Deps

## 0.9.1

### Important Changes

- Feat: Support `https_redirection_port` option to redirect http requests to https with custom port.

### Improvement

- Refactor: lots of minor improvements
- Deps

## 0.9.0

### Important Changes

- Breaking: Experimental ACME support is added. Check the new configuration options and README.md for ACME support. Note that it is still under development and may have some issues.

### Improvement

- Refactor: lots of minor improvements
- Deps

### Bugfix

- Fix the bug that the dynamic config reload does not work properly.

## 0.8.1

### Improvement

- Refactor: lots of minor improvements
- Deps

## 0.8.0

### Important Changes

- Breaking: Support for `rustls`-0.23.x for http/1.1, 2 and 3. No configuration update is needed at this point.
- Breaking: Along with `rustls`, the cert manager was split from `rpxy-lib` and moved to a new inner crate `rpxy-cert`. This change is to make the cert manager reusable for other projects and to support not only static file based certificates but also other types, e.g., dynamic fetching and management via ACME, in the future.

### Improvement

- Refactor: lots of minor improvements
- Change the certificate verifier from `rustls-native-certs` to `rustls-platform-verifier` to use the system's default root cert store for better client (forwarder) performance in `hyper-rustls`.

## 0.7.1

- deps and patches

## 0.7.0

### Important Changes

- Breaking: `hyper`-1.0 for both server and client modules.
- Breaking: Remove `override_host` option in upstream options. Add a reverse option, i.e., `keep_original_host`, and the similar option `set_upstream_host`. While `keep_original_host` can be explicitly specified, `rpxy` keeps the original `host` given by the incoming request by default. Then, the original `host` header is maintained or added from the value of url request line. If `host` header needs to be overridden with the upstream host name (backend uri's host name), `set_upstream_host` has to be set. If both of `set_upstream_host` and `keep_original_host` are set, `keep_original_host` is prioritized since it is explicitly specified.
- Breaking: Introduced `native-tls-backend` feature to use the native TLS engine to access backend applications.
- Breaking: Changed the policy of the default cert store from `webpki` to the system-native store. Thus we terminated the feature `native-roots` and introduced `webpki-roots` feature to use `webpki` root cert store.

### Improvement

- Redesigned: Cache structure is totally redesigned with more memory-efficient way to read from cache file, and more secure way to strongly bind memory-objects with files with hash values.
- Redesigned: HTTP body handling flow is also redesigned with more memory-and-time efficient techniques without putting the whole objects on memory by using `futures::stream::Stream` and `futures::channel::mpsc`
- Feat: Allow to disable/enable forced-connection-timeout regardless of connection status (idle or not). [default: disabled]
- Refactor: lots of minor improvements

## 0.6.2

### Improvement

- Feat: Add a build feature of `native-roots` to use the system's default root cert store.
- Feat: Add binary release in addition to container release
- Refactor: lots of minor improvements

## 0.6.1

### Bugfix

- Fix: fix a "watch" bug for docker. Due to a docker limitation, we need to mount a dir, e.g, `/rpxy/config`, instead of a file, `rpxy.toml`, to track changes of the configuration file. We thus updated a start up script in docker container for the case "WATCH=true".

## 0.6.0

### Improvement

- Feat: Enabled `h2c` (HTTP/2 cleartext) requests to upstream app servers (in the previous versions, only HTTP/1.1 is allowed for cleartext requests)
- Feat: Initial implementation of caching feature using file + on memory cache. (Caveats: No persistance of the cache. Once config is updated, the cache is totally eliminated.)
- Refactor: lots of minor improvements

### Bugfix

- Fix: fix `server` in the response header (`rpxy_lib` -> `rpxy`)
- Fix: fix bug for hot-reloading configuration file (Add termination notification receiver in proxy services)

## 0.5.0

### Improvement

- Feat: `s2n-quic` with `s2n-quic-h3` is supported as QUIC and HTTP/3 library in addition to `quinn` with `h3-quinn`, related to #57.
- Feat: Publish dockerfile for `rpxy` with `s2n-quic` on both `amd64` and `arm64`.
- Feat: Start to publish docker images on `ghcr.io`
- Refactor: logs of minor improvements

## 0.4.0

### Improvement

- Feat: Continuous watching on a specified config file and hot-reloading the file when updated
- Feat: Enabled to specify TCP listen backlog in the config file
- Feat: Add a GitHub action to build `arm64` docker image.
- Bench: Add benchmark result on `amd64` architecture.
- Refactor: Split `rpxy` into `rpxy-lib` and `rpxy-bin`
- Refactor: lots of minor improvements

### Bugfix

- Fix bug to apply default backend application

## 0.3.0

### Improvement

- HTTP/3 Deps: Update `h3` with `quinn-0.10` or higher. But changed their crates from `crates.io` to `git submodule` as a part of work around. I think this will be back to `crates.io` in a near-future update.
- Load Balancing: Implement the session persistance function for load balancing using sticky cookie (initial implementation). Enabled in `default-features`.
- Docker UID:GID: Update `Dockerfile`s to allow arbitrary UID and GID (non-root users) for rpxy. Now they can be set as you like by specifying through env vars.
- Refactor: Various minor improvements

## 0.2.0

### Improvement

- Update docker of `nightly` built from `develop` branch along with `amd64-slim` and `amd64` images with `latest` and `latest:slim` tags built from `main` branch. `nightly` image is based on `amd64`.
- Update `h3` with `quinn-0.10` or higher.
- Implement path replacing option for each reverse proxy backend group.

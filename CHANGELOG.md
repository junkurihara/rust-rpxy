# CHANGELOG

## 0.10.3 or 0.11.0 (Unreleased)

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

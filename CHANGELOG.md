# CHANGELOG

## 0.7.0  (unreleased)

- Breaking: `hyper`-1.0 for both server and client modules.
- Breaking: Remove `override_host` option in upstream options. Add a reverse option, i.e., `disable_override_host`. That is, `rpxy` always override the host header by the upstream hostname by default.
- Breaking: Introduced `hyper-tls-backend` feature to use the native TLS engine to access backend applications.
- Redesigned: Cache structure is totally redesigned with more memory-efficient way to read from cache file, and more secure way to strongly bind memory-objects with files with hash values.
- Redesigned: HTTP body handling flow is also redesigned with more memory-and-time efficient techniques without putting the whole objects on memory by using `futures::stream::Stream` and `futures::channel::mpsc`
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

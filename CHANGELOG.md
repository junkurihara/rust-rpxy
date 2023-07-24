# CHANGELOG

## 0.4.0 (unreleased)

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

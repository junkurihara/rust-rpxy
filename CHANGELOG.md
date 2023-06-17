# CHANGELOG

## 0.3.0 (unreleased)

### Improvement

- Update `h3` with `quinn-0.10` or higher.
- Implement the session persistance function for load balancing using sticky cookie (initial implementation). Enabled in `default-features`.
- Update `Dockerfile`s to change UID and GID to non-root users. Now they can be set as you like by specifying through env vars.

## 0.2.0

### Improvement

- Update docker of `nightly` built from `develop` branch along with `amd64-slim` and `amd64` images with `latest` and `latest:slim` tags built from `main` branch. `nightly` image is based on `amd64`.
- Update `h3` with `quinn-0.10` or higher.
- Implement path replacing option for each reverse proxy backend group.

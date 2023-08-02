# Docker Images of `rpxy`

The `rpxy` docker images are hosted both on [Docker Hub](https://hub.docker.com/r/jqtype/rpxy) and [GitHub Container Registry](https://github.com/junkurihara/rust-rpxy/pkgs/container/rust-rpxy). Differences among tags are summarized as follows.

## Latest Builds

- `latest`: Built from the `main` branch with default features, running on Ubuntu.
- `latest-slim`, `slim`: Built by `musl` from the `main` branch with default features, running on Alpine.
- `latest-s2n`, `s2n`: Built from the `main` branch with the `http3-s2n` feature, running on Ubuntu.

## Nightly Builds

- `nightly`: Built from the `develop` branch with default features, running on Ubuntu.
- `nightly-slim`: Built by `musl` from the `develop` branch with default features, running on Alpine.
- `nightly-s2n`: Built from the `develop` branch with the `http3-s2n` feature, running on Ubuntu.

## Caveats

Due to some compile errors of `s2n-quic` subpackages with `musl`, `nightly-s2n-slim` or `latest-s2n-slim` are not yet provided.

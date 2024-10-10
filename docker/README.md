# Docker Images of `rpxy`

The `rpxy` docker images are hosted both on [Docker Hub](https://hub.docker.com/r/jqtype/rpxy) and [GitHub Container Registry](https://github.com/junkurihara/rust-rpxy/pkgs/container/rust-rpxy).

## Usage

There are several docker-specific environment variables.

- `HOST_USER` (default: `user`): User name executing `rpxy` inside the container.
- `HOST_UID` (default: `900`): `UID` of `HOST_USER`.
- `HOST_GID` (default: `900`): `GID` of `HOST_USER`
- `LOG_LEVEL=debug|info|warn|error`: Log level
- `LOG_TO_FILE=true|false`: Enable logging to the log file `/rpxy/log/rpxy.log` using `logrotate`. You should mount `/rpxy/log` via docker volume option if enabled. The log dir and file will be owned by the `HOST_USER` with `HOST_UID:HOST_GID` on the host machine. Hence, `HOST_USER`, `HOST_UID` and `HOST_GID` should be the same as ones of the user who executes the `rpxy` docker container on the host.
- `WATCH=true|false` (default: `false`): Activate continuous watching of the config file if true.

Then, all you need is to mount your `config.toml` as `/etc/rpxy.toml` and certificates/private keys as you like through the docker volume option. **If `WATCH=true`, You need to mount a directory, e.g., `./rpxy-config/`, including `rpxy.toml` on `/rpxy/config` instead of a file to correctly track file changes**. This is a docker limitation. Even if `WATCH=false`, you can mount the dir onto `/rpxy/config` rather than `/etc/rpxy.toml`. A file mounted on `/etc/rpxy` is prioritized over a dir mounted on `/rpxy/config`.

See [`docker-compose.yml`](./docker-compose.yml) for the detailed configuration. Note that the file path of keys and certificates must be ones in your docker container.

## Custom CAs for upstream TLS connections

To add a custom certificate, you must use a non-`webpki` image. Then mount `/usr/local/share/ca-certificates` in the container with your desired CAs each in a file like `myca.crt`. The certificates are accepted in PEM format but file extension must be `crt`.

e.g. `-v rpxy/ca-certificates:/usr/local/share/ca-certificates`

## Differences among image tags of Docker Hub and GitHub Container Registry

Differences among tags are summarized as follows.

### Latest Builds

- `latest`: Built from the `main` branch with default features, running on Ubuntu.
- `latest-slim`, `slim`: Built by `musl` from the `main` branch with default features, running on Alpine.
- `latest-s2n`, `s2n`: Built from the `main` branch with the `http3-s2n` feature, running on Ubuntu.

### Nightly Builds

- `nightly`: Built from the `develop` branch with default features, running on Ubuntu.
- `nightly-slim`: Built by `musl` from the `develop` branch with default features, running on Alpine.
- `nightly-s2n`: Built from the `develop` branch with the `http3-s2n` feature, running on Ubuntu.

## Caveats

Due to some compile errors of `s2n-quic` subpackages with `musl`, `nightly-s2n-slim` or `latest-s2n-slim` are not yet provided.

See [`./docker/README.md`](./docker/README.md) for the differences on image tags.

FROM ubuntu:24.04 AS base
LABEL maintainer="Jun Kurihara"

SHELL ["/bin/sh", "-x", "-c"]
ENV SERIAL 2

########################################
FROM --platform=$BUILDPLATFORM base AS builder

ENV CFLAGS=-Ofast
ENV BUILD_DEPS curl make ca-certificates build-essential
ENV TARGET_SUFFIX=unknown-linux-gnu

WORKDIR /tmp

COPY . /tmp/

ARG TARGETARCH
ARG CARGO_FEATURES
ENV CARGO_FEATURES ${CARGO_FEATURES}
ARG ADDITIONAL_DEPS
ENV ADDITIONAL_DEPS ${ADDITIONAL_DEPS}

RUN if [ $TARGETARCH = "amd64" ]; then \
  echo "x86_64" > /arch; \
  elif [ $TARGETARCH = "arm64" ]; then \
  echo "aarch64" > /arch; \
  else \
  echo "Unsupported platform: $TARGETARCH"; \
  exit 1; \
  fi

ENV RUSTFLAGS "-C link-arg=-s"

RUN update-ca-certificates 2> /dev/null || true

RUN apt-get update && apt-get install -qy --no-install-recommends $BUILD_DEPS ${ADDITIONAL_DEPS} && \
  curl -sSf https://sh.rustup.rs | bash -s -- -y --default-toolchain stable && \
  export PATH="$HOME/.cargo/bin:$PATH" && \
  echo "Install toolchain" && \
  rustup target add $(cat /arch)-${TARGET_SUFFIX} && \
  echo "Building rpxy from source" && \
  cargo update &&\
  cargo build --release --target=$(cat /arch)-${TARGET_SUFFIX} ${CARGO_FEATURES} && \
  strip --strip-all /tmp/target/$(cat /arch)-${TARGET_SUFFIX}/release/rpxy &&\
  cp /tmp/target/$(cat /arch)-${TARGET_SUFFIX}/release/rpxy /tmp/target/release/rpxy

########################################
FROM --platform=$TARGETPLATFORM base AS runner

ENV RUNTIME_DEPS logrotate ca-certificates gosu

RUN apt-get update && \
  apt-get install -qy --no-install-recommends $RUNTIME_DEPS && \
  apt-get -qy clean && \
  apt-get -qy autoremove && \
  rm -fr /tmp/* /var/tmp/* /var/cache/apt/* /var/lib/apt/lists/* /var/log/apt/* /var/log/*.log && \
  find / -type d -path /proc -prune -o -type f -perm /u+s -ignore_readdir_race -exec chmod u-s {} \; && \
  find / -type d -path /proc -prune -o -type f -perm /g+s -ignore_readdir_race -exec chmod g-s {} \; && \
  mkdir -p /rpxy/bin &&\
  mkdir -p /rpxy/log &&\
  mkdir -p /rpxy/cache &&\
  mkdir -p /rpxy/config

COPY --from=builder /tmp/target/release/rpxy /rpxy/bin/rpxy
COPY ./docker/run.sh /rpxy
COPY ./docker/entrypoint.sh /rpxy

RUN chmod +x /rpxy/run.sh && \
  chmod +x /rpxy/entrypoint.sh

EXPOSE 80 443

CMD ["/usr/bin/bash" "/rpxy/entrypoint.sh"]

ENTRYPOINT ["/usr/bin/bash", "/rpxy/entrypoint.sh"]

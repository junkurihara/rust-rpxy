FROM ubuntu:22.04 AS base

SHELL ["/bin/sh", "-x", "-c"]
ENV SERIAL 2

########################################
FROM base as builder

ENV CFLAGS=-Ofast
ENV BUILD_DEPS curl make ca-certificates build-essential

WORKDIR /tmp

COPY . /tmp/

ENV RUSTFLAGS "-C link-arg=-s"

RUN update-ca-certificates 2> /dev/null || true

RUN apt-get update && apt-get install -qy --no-install-recommends $BUILD_DEPS && \
  curl -sSf https://sh.rustup.rs | bash -s -- -y --default-toolchain stable && \
  export PATH="$HOME/.cargo/bin:$PATH" && \
  echo "Building rpxy from source" && \
  cargo build --release && \
  strip --strip-all /tmp/target/release/rpxy

########################################
FROM base AS runner
LABEL maintainer="Jun Kurihara"

ENV RUNTIME_DEPS bash logrotate ca-certificates

RUN apt-get update && \
  apt-get install -qy --no-install-recommends $RUNTIME_DEPS && \
  apt-get -qy clean && \
  rm -fr /tmp/* /var/tmp/* /var/cache/apt/* /var/lib/apt/lists/* /var/log/apt/* /var/log/*.log &&\
  mkdir -p /opt/rpxy/sbin &&\
  mkdir -p /var/log/rpxy && \
  touch /var/log/rpxy/rpxy.log

COPY --from=builder /tmp/target/release/rpxy /opt/rpxy/sbin/rpxy
COPY docker-bin/run.sh /
COPY docker-bin/entrypoint.sh /

RUN chmod 755 /run.sh && \
  chmod 755 /entrypoint.sh

EXPOSE 80 443

CMD ["/entrypoint.sh"]

ENTRYPOINT ["/entrypoint.sh"]

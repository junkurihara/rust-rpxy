########################################
FROM messense/rust-musl-cross:x86_64-musl as builder

ENV TARGET_DIR=x86_64-unknown-linux-musl
ENV CFLAGS=-Ofast

WORKDIR /tmp

COPY . /tmp/

ENV RUSTFLAGS "-C link-arg=-s"

# RUN update-ca-certificates 2> /dev/null || true

RUN echo "Building rpxy from source" && \
  cargo build --release && \
  musl-strip --strip-all /tmp/target/${TARGET_DIR}/release/rpxy

########################################
FROM alpine:latest as runner

ENV TARGET_DIR=x86_64-unknown-linux-musl
ENV RUNTIME_DEPS logrotate ca-certificates

RUN apk add --no-cache ${RUNTIME_DEPS} && \
  update-ca-certificates && \
  mkdir -p /opt/rpxy/sbin &&\
  mkdir -p /var/log/rpxy && \
  touch /var/log/rpxy/rpxy.log

COPY --from=builder /tmp/target/${TARGET_DIR}/release/rpxy /opt/rpxy/sbin/rpxy
COPY docker-bin/run.sh /
COPY docker-bin/entrypoint.sh /

RUN chmod 755 /run.sh && \
  chmod 755 /entrypoint.sh

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
ENV SSL_CERT_DIR=/etc/ssl/certs

EXPOSE 80 443

CMD ["/entrypoint.sh"]

ENTRYPOINT ["/entrypoint.sh"]

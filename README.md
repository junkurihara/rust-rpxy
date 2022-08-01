# rpxy: A simple and ultrafast reverse-proxy for multiple host names with TLS termination, written in pure Rust

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Unit Test](https://github.com/junkurihara/rust-rpxy/actions/workflows/ci.yml/badge.svg)
![Build and Publish Docker](https://github.com/junkurihara/rust-rpxy/actions/workflows/docker_build_push.yml/badge.svg)
![ShiftLeft Scan](https://github.com/junkurihara/rust-rpxy/actions/workflows/shift_left.yml/badge.svg)

**WIP Project**

## Introduction

`rpxy` [ahr-pik-see] is an implementation of simple and lightweight reverse-proxy with some additional features. The implementation is based on [`hyper`](https://github.com/hyperium/hyper), [`rustls`](https://github.com/rustls/rustls) and [`tokio`](https://github.com/tokio-rs/tokio), i.e., written in pure Rust. Our `rpxy` allows to route multiple host names to appropriate backend application servers while serving TLS connections.

 As default, `rpxy` provides the *TLS connection sanitization* by correctly binding a certificate used to establish secure channel with backend application. Specifically, it always keeps the consistency between the given SNI (server name indication) in `ClientHello` of the underlying TLS and the domain name given by the overlaid HTTP HOST header (or URL in Request line) [^1]. Additionally, as a somewhat unstable feature, our `rpxy` can handle the brand-new HTTP/3 connection thanks to [`quinn`](https://github.com/quinn-rs/quinn) and [`hyperium/h3`](https://github.com/hyperium/h3).

 This project is still *work-in-progress*. But it is already working in some production environments and serves numbers of domain names. Furthermore it *significantly outperforms* NGINX and Caddy, e.g., *1.5x faster than NGINX*, in the setting of very simple HTTP reverse-proxy scenario (See [`bench`](./bench/) directory).

 [^1]: We should note that NGINX doesn't guarantee such a consistency by default. To this end, you have to add `if` statement in the configuration file in NGINX.

## Making an executable binary

```:bash
% cargo build --release
```

Then you have a binary at `./target/release/rpxy`.

You can also use [`docker` image](https://hub.docker.com/r/jqtype/rpxy) instead of building from the source.

## Usage

todo!

## Configuration

todo!

## Using `docker` image

todo!

# rpxy: A simple and ultrafast reverse-proxy for multiple host names with TLS termination, written in pure Rust

![Unit Test](https://github.com/junkurihara/rust-rpxy/actions/workflows/ci.yml/badge.svg)
![Build and Publish Docker](https://github.com/junkurihara/rust-rpxy/actions/workflows/docker_build_push.yml/badge.svg)
![ShiftLeft Scan](https://github.com/junkurihara/rust-rpxy/actions/workflows/shift_left.yml/badge.svg)


**WIP Project**

## Introduction

`rpxy` [ahr-pik-see] is an (currently experimental) implementation of simple and lightweight reverse-proxy, which is based on [`hyper`](https://github.com/hyperium/hyper), [`rustls`](https://github.com/rustls/rustls) and [`tokio`](https://github.com/tokio-rs/tokio), i.e., written in pure Rust. Our `rpxy` allows to route multiple host names to appropriate backend application servers while serving TLS connections.

This project is still *work-in-progress*. But it is already working in some production environments and serves numbers of domain names. Furthermore it dramatically outperforms NGINX and Caddy in the setting of very simple HTTP reverse-proxy scenario (See [`bench`](./bench/) directory).

 `rpxy` provides the sanitization of TLS's SNI (server name indication) in default by correctly binding a certificate used to establish an underlying TLS connection with backend application specified in the overlaid HTTP HOST header (or URL in Request line). Additionally, as a somewhat unstable feature, our `rpxy` can handle the brand-new HTTP/3 connection thanks to [`quinn`](https://github.com/quinn-rs/quinn) and [`hyperium/h3`](https://github.com/hyperium/h3).

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

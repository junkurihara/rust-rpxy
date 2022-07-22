# rpxy: A simple and fast reverse-proxy for multiple host names, written in pure Rust

**WIP Project**

## Introduction

`rpxy` [ahr-pik-see] is an (currently experimental) implementation of simple and lightweight reverse-proxy, which is based on `hyper`, `rustls` and `tokio`, i.e., written in pure Rust. Our `rpxy` allows to route multiple host names to appropriate backend application servers while serving TLS connections.

This project is still *work-in-progress*. But it is already working in some production environments and serves numbers of domain names. Furthermore it dramatically outperforms NGINX and Caddy in the setting of very simple HTTP reverse-proxy scenario (See `./bench` directory).

 `rpxy` provides the sanitization of TLS's SNI (server name indication) in default by correctly binding a certificate used to establish an underlying TLS connection with backend application specified in the overlaid HTTP HOST header (or URL in Request line). Additionally, as a somewhat unstable feature, our `rpxy` can handle the brand-new HTTP/3 connection thanks to `quinn` and `hyperium/h3`.

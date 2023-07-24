# rpxy: A simple and ultrafast reverse-proxy serving multiple domain names with TLS termination, written in pure Rust

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Unit Test](https://github.com/junkurihara/rust-rpxy/actions/workflows/ci.yml/badge.svg)
![Build and Publish Docker](https://github.com/junkurihara/rust-rpxy/actions/workflows/docker_build_push.yml/badge.svg)
![ShiftLeft Scan](https://github.com/junkurihara/rust-rpxy/actions/workflows/shift_left.yml/badge.svg)
[![Docker Image Size (latest by date)](https://img.shields.io/docker/image-size/jqtype/rpxy)](https://hub.docker.com/r/jqtype/rpxy)

> **WIP Project**

## Introduction

`rpxy` [ahr-pik-see] is an implementation of simple and lightweight reverse-proxy with some additional features. The implementation is based on [`hyper`](https://github.com/hyperium/hyper), [`rustls`](https://github.com/rustls/rustls) and [`tokio`](https://github.com/tokio-rs/tokio), i.e., written in pure Rust. Our `rpxy` routes multiple host names to appropriate backend application servers while serving TLS connections.

 As default, `rpxy` provides the *TLS connection sanitization* by correctly binding a certificate used to establish a secure channel with the backend application. Specifically, it always keeps the consistency between the given SNI (server name indication) in `ClientHello` of the underlying TLS and the domain name given by the overlaid HTTP HOST header (or URL in Request line) [^1]. Additionally, as a somewhat unstable feature, our `rpxy` can handle the brand-new HTTP/3 connection thanks to [`quinn`](https://github.com/quinn-rs/quinn) and [`hyperium/h3`](https://github.com/hyperium/h3).

 This project is still *work-in-progress*. But it is already working in some production environments and serves a number of domain names. Furthermore it *significantly outperforms* NGINX and Caddy, e.g., *1.5x faster than NGINX*, in the setting of a very simple HTTP reverse-proxy scenario (See [`bench`](./bench/) directory).

 [^1]: We should note that NGINX doesn't guarantee such a consistency by default. To this end, you have to add `if` statement in the configuration file in NGINX.

## Installing/Building an Executable Binary of `rpxy`

You can build an executable binary yourself by checking out this Git repository.

```bash
# Cloning the git repository
% git clone https://github.com/junkurihara/rust-rpxy
% cd rust-rpxy

# Update submodule hyperium/h3
% git submodule update --init

# Build
% cargo build --release
```

Then you have an executive binary `rust-rpxy/target/release/rpxy`.

Note that we do not have an option of installation via [`crates.io`](https://crates.io/), i.e., `cargo install`, at this point since some dependencies are not published yet. Alternatively, you can use docker image (see below) as the easiest way for `amd64` environment.

## Usage

`rpxy` always refers to a configuration file in TOML format, e.g., `config.toml`. You can find an example of the configuration file, `config-example.toml`, in this repository.

You can run `rpxy` with a configuration file like

```bash
% ./target/release/rpxy --config config.toml
```

If you specify `-w` option along with the config file path, `rpxy` tracks the change of `config.toml` in the real-time manner and apply the change immediately without restarting the process.

The full help messages are given follows.

```bash:
usage: rpxy [OPTIONS] --config <FILE>

Options:
  -c, --config <FILE>  Configuration file path like ./config.toml
  -w, --watch          Activate dynamic reloading of the config file via continuous monitoring
  -h, --help           Print help
  -V, --version        Print version
```

That's all!

## Basic Configuration

### First Step: Cleartext HTTP Reverse Proxy

The most basic configuration of `config.toml` is given like the following.

```toml
listen_port = 80

[apps.app1]
server_name = 'app1.example.com'
reverse_proxy = [{ upstream = [{ location = 'app1.local:8080' }] }]
```

In the above setting, `rpxy` listens on port 80 (TCP) and serves incoming cleartext HTTP request including a `app1.example.com` in its HOST header or URL in its Request line.
For example, request messages like the followings.

```http
GET http://app1.example.com/path/to HTTP/1.1\r\n
```

or

```http
GET /path/to HTTP/1.1\r\n
HOST: app1.example.com\r\n
```

Otherwise, say, a request to `other.example.com` is simply rejected with the status code `40x`.

If you want to host multiple and distinct domain names in a single IP address/port, simply create multiple `app."<app_name>"` entries in config file like

```toml
default_application = "app1"

[app.app1]
server_name = "app1.example.com"
#...

[app.app2]
server_name = "app2.example.org"
#...
```

Here we note that by specifying `default_application` entry, *HTTP* requests will be served by the specified application if HOST header or URL in Request line doesn't match any `server_name`s in `reverse_proxy` entries. For HTTPS requests, it will be rejected since the secure connection cannot be established for the unknown server name.

#### HTTPS to Backend Application

The request message will be routed to the backend application specified with the domain name `app1.localdomain:8080` or IP address over cleartext HTTP. If the backend channel needs to serve TLS like forwarding to `https://app1.localdomain:8080`, you need to enable a `tls` option for the location.

```toml
revese_proxy = [
  { location = 'app1.localdomain:8080', tls = true }
]
```

#### Load Balancing

You can specify multiple backend locations in the `reverse_proxy` array for *load-balancing* with an appropriate `load_balance` option. Currently it works in the manner of round-robin, in the random fashion, or round-robin with *session-persistance* using cookie. if `load_balance` is not specified, the first backend location is always chosen.

```toml
[apps."app_name"]
server_name = 'app1.example.com'
reverse_proxy = [
  { location = 'app1.local:8080' },
  { location = 'app2.local:8000' }
]
load_balance = 'round_robin' # or 'random' or 'sticky'
```

### Second Step: Terminating TLS

First of all, you need to specify a port `listen_port_tls` listening the HTTPS traffic, separately from HTTP port (`listen_port`). Then, serving an HTTPS endpoint can be easily done for your desired application just by specifying TLS certificates and private keys in PEM files.

```toml
listen_port = 80
listen_port_tls = 443

[apps."app_name"]
server_name = 'app1.example.com'
tls = { tls_cert_path = 'server.crt',  tls_cert_key_path = 'server.key' }
reverse_proxy = [{ upstream = [{ location = 'app1.local:8080' }] }]
```

In the above setting, both cleartext HTTP requests to port 80 and ciphertext HTTPS requests to port 443 are routed to the backend `app1.local:8080` in the same fashion. If you don't need to serve cleartext requests, just remove `listen_port = 80` and specify only `listen_port_tls = 443`.

We should note that the private key specified by `tls_cert_key_path` must be *in PKCS8 format*. (See TIPS to convert PKCS1 formatted private key to PKCS8 one.)

#### Redirecting Cleartext HTTP Requests to HTTPS

In the current Web, we believe it is common to serve everything through HTTPS rather than HTTP, and hence *https redirection* is often used for HTTP requests. When you specify both `listen_port` and `listen_port_tls`, you can enable an option of such  redirection by making `https_redirection` true.

```toml
tls = { https_redirection = true, tls_cert_path = 'server.crt', tls_cert_key_path = 'server.key' }
```

If it is true, `rpxy` returns the status code `301` to the cleartext request with new location `https://<requested_host>/<requested_query_and_path>` served over TLS.

### Third Step: More Flexible Routing Based on URL Path

`rpxy` can serves, of course, routes requests to multiple backend destination according to the path information. The routing information can be specified for each application (`server_name`) as follows.

```toml
listen_port_tls = 443

[apps.app1]
server_name = 'app1.example.com'
tls = { https_redirection = true, tls_cert_path = 'server.crt', tls_cert_key_path = 'server.key' }

[[apps.app1.reverse_proxy]]
upstream = [
  { location = 'default.backend.local' }
]

[[apps.app1.reverse_proxy]]
path = '/path'
upstream = [
  { location = 'path.backend.local' }
]

[[apps.app1.reverse_proxy]]
path = '/path/another'
replace_path = '/path'
upstream = [
  { location = 'another.backend.local' }
]
```

In the above example, a request to `https://app1.example.com/path/to?query=ok` matches the second `reverse_proxy` entry in the longest-prefix-matching manner, and will be routed to `path.backend.local` with preserving path and query information, i.e., served as `http://path.backend.local/path/to?query=ok`.

On the other hand, a request to `https://app1.example.com/path/another/xx?query=ng` matching the third entry is routed with *being rewritten its path information* specified by `replace_path` option. Namely, the matched `/path/another` part is rewritten with `/path`, and it is served as `http://another.backend.local/path/xx?query=ng`.

Requests that doesn't match any paths will be routed by the first entry that doesn't have the `path` option, which means the *default destination*. In other words, unless every `reverse_proxy` entry has an explicit `path` option, `rpxy` rejects requests that don't match any paths.

#### Simple Path-based Routing

This path-based routing option would be enough in many cases. For example, you can serve multiple applications with one domain by specifying unique path to each application. More specifically, see an example below.

```toml
[apps.app]
server_name = 'app.example.com'
#...

[[apps.app.reverse_proxy]]
path = '/subapp1'
replace_path = '/'
upstream = [ { location = 'subapp1.local' } ]

[[apps.app.reverse_proxy]]
path = '/subapp2'
replace_path = '/'
upstream = [ { location = 'subapp2.local' } ]

[[apps.app.reverse_proxy]]
path = '/subapp3'
replace_path = '/'
upstream = [ { location = 'subapp3.local' } ]
```

This example configuration explains a very frequent situation of path-based routing. When a request to `app.example.com/subappN` routes to `sbappN.local` by replacing a path part `/subappN` to `/`.

## More Options

Since it is currently a work-in-progress project, we are frequently adding new options. We first add new option entries in the `config-example.toml` as examples. So please refer to it for up-to-date options. We will prepare a comprehensive documentation for all options.

## Using Docker Image

You can also use [docker image](https://hub.docker.com/r/jqtype/rpxy) instead of directly executing the binary. There are only several docker-specific environment variables.

- `HOST_USER` (default: `user`): User name executing `rpxy` inside the container.
- `HOST_UID` (default: `900`): `UID` of `HOST_USER`.
- `HOST_GID` (default: `900`): `GID` of `HOST_USER`
- `LOG_LEVEL=debug|info|warn|error`: Log level
- `LOG_TO_FILE=true|false`: Enable logging to the log file `/rpxy/log/rpxy.log` using `logrotate`. You should mount `/rpxy/log` via docker volume option if enabled. The log dir and file will be owned by the `HOST_USER` with `HOST_UID:HOST_GID` on the host machine. Hence, `HOST_USER`, `HOST_UID` and `HOST_GID` should be the same as ones of the user who executes the `rpxy` docker container on the host.

Other than them, all you need is to mount your `config.toml` as `/etc/rpxy.toml` and certificates/private keys as you like through the docker volume option. See [`docker/docker-compose.yml`](./docker/docker-compose.yml) for the detailed configuration. Note that the file path of keys and certificates must be ones in your docker container.

## Example

[`./bench`](./bench/) directory could be a very simple example of configuration of `rpxy`. This can also be an example of an example of docker use case.

## Experimental Features and Caveats

### HTTP/3

`rpxy` can serves HTTP/3 requests thanks to `quinn` and `hyperium/h3`. To enable this experimental feature, add an entry `experimental.h3` in your `config.toml` like follows. Any values in the entry like `alt_svc_max_age` are optional.

```toml
[experimental.h3]
alt_svc_max_age = 3600
request_max_body_size = 65536
max_concurrent_connections = 10000
max_concurrent_bidistream = 100
max_concurrent_unistream = 100
max_idle_timeout = 10
```

### Client Authentication via Client Certificates

Client authentication is enabled when `apps."app_name".tls.client_ca_cert_path` is set for the domain specified by `"app_name"` like

```toml
[apps.localhost]
server_name = 'localhost' # Domain name
tls = { https_redirection = true, tls_cert_path = './server.crt', tls_cert_key_path = './server.key', client_ca_cert_path = './client_cert.ca.crt' }
```

 However, currently we have a limitation on HTTP/3 support for applications that enables client authentication. If an application is set with client authentication, HTTP/3 doesn't work for the application.

## TIPS

### Using Private Key Issued by Let's Encrypt

If you obtain certificates and private keys from [Let's Encrypt](https://letsencrypt.org/), you have PKCS1-formatted private keys. So you need to convert such retrieved private keys into PKCS8 format to use in `rpxy`.

The easiest way is to use `openssl` by

```bash
% openssl pkcs8 -topk8 -nocrypt \
    -in yoru_domain_from_le.key \
    -inform PEM \
    -out your_domain_pkcs8.key.pem \
    -outform PEM
```

### Client Authentication using Client Certificate Signed by Your Own Root CA

First, you need to prepare a CA certificate used to verify client certificate. If you do not have one, you can generate CA key and certificate by OpenSSL commands as follows. *Note that `rustls` accepts X509v3 certificates and reject SHA-1, and that `rpxy` relys on Version 3 extension fields of `KeyID`s of `Subject Key Identifier` and `Authority Key Identifier`.*

1. Generate CA key of `secp256v1`, CSR, and then generate CA certificate that will be set for `tls.client_ca_cert_path` for each server app in `config.toml`.

  ```bash
  % openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:prime256v1 -out client.ca.key

  % openssl req -new -key client.ca.key -out client.ca.csr
  ...
  -----
  Country Name (2 letter code) []: ...
  State or Province Name (full name) []: ...
  Locality Name (eg, city) []: ...
  Organization Name (eg, company) []: ...
  Organizational Unit Name (eg, section) []: ...
  Common Name (eg, fully qualified host name) []: <Should not input CN>
  Email Address []: ...

  % openssl x509 -req -days 3650 -sha256 -in client.ca.csr -signkey client.ca.key -out client.ca.crt -extfile client.ca.ext
  ```

2. Generate a client key of `secp256v1` and certificate signed by CA key.

  ```bash
  % openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:prime256v1 -out client.key

  % openssl req -new -key client.key -out client.csr
  ...
  -----
  Country Name (2 letter code) []:
  State or Province Name (full name) []:
  Locality Name (eg, city) []:
  Organization Name (eg, company) []:
  Organizational Unit Name (eg, section) []:
  Common Name (eg, fully qualified host name) []: <Should not input CN>
  Email Address []:

  % openssl x509 -req -days 365 -sha256 -in client.csr -CA client.ca.crt -CAkey client.ca.key -CAcreateserial -out client.crt -extfile client.ext
  ```

  Now you have a client key `client.key` and certificate `client.crt` (version 3). `pfx` (`p12`) file can be retrieved as

  ```bash
  % openssl pkcs12 -export -inkey client.key -in client.crt -certfile client.ca.crt -out client.pfx
  ```

  Note that on MacOS, a `pfx` generated by `OpenSSL 3.0.6` cannot be imported to MacOS KeyChain Access. We generated the sample `pfx` using `LibreSSL 2.8.3` instead `OpenSSL`.

  All of sample certificate files are found in `./example-certs/` directory.

### (Work Around) Deployment on Ubuntu 22.04LTS using docker behind `ufw`

Basically, docker automatically manage your iptables if you use the port-mapping option, i.e., `--publish` for `docker run` or `ports` in `docker-compose.yml`. This means you do not need to manually expose your port, e.g., 443 TCP/UDP for HTTPS, using `ufw`-like management command.

However, we found that if you want to use the brand-new UDP-based protocol, HTTP/3, on `rpxy`, you need to explicitly expose your HTTPS port by using `ufw`-like command.

```bash
% sudo ufw allow 443
% sudo ufw enable
```

Your docker container can receive only TCP-based connection, i.e., HTTP/2 or before, unless you manually manage the port. We see that this is weird and expect that it is a kind of bug (of docker? ubuntu? or something else?). But at least for Ubuntu 22.04LTS, you need to handle it as above.

### Other TIPS

todo!

## License

`rpxy` is free, open-source software licensed under MIT License.

You can open issues for bugs you've found or features you think are missing. You can also submit pull requests to this repository.

Contributors are more than welcome!

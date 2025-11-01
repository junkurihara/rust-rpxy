# rpxy: A simple and ultrafast reverse-proxy serving multiple domain names with TLS termination, written in Rust

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Unit Test](https://github.com/junkurihara/rust-rpxy/actions/workflows/ci.yml/badge.svg)
![Docker](https://github.com/junkurihara/rust-rpxy/actions/workflows/release_docker.yml/badge.svg)
![ShiftLeft Scan](https://github.com/junkurihara/rust-rpxy/actions/workflows/shift_left.yml/badge.svg)
[![Docker Image Size (latest by date)](https://img.shields.io/docker/image-size/jqtype/rpxy)](https://hub.docker.com/r/jqtype/rpxy)

> **WIP Project**

> [!NOTE]
> This project is an HTTP, i.e., Layer 7, reverse-proxy. If you are looking for a TCP/UDP, i.e., Layer 4, reverse-proxy, please check my another project, [`rpxy-l4`](https://github.com/junkurihara/rust-rpxy-l4).

## Introduction

`rpxy` [ahr-pik-see] is a simple and lightweight reverse-proxy implementation with additional features. The implementation is based on [`hyper`](https://github.com/hyperium/hyper), [`rustls`](https://github.com/rustls/rustls) and [`tokio`](https://github.com/tokio-rs/tokio), i.e., written in Rust [^pure_rust]. `rpxy` routes multiple hostnames to appropriate backend application servers while serving TLS connections.

[^pure_rust]: It is questionable whether this can be claimed to be written in pure Rust since the current `rpxy` is based on `aws-lc-rs` for cryptographic operations.

The supported features are summarized as follows:

- Supported HTTP(S) protocols: HTTP/1.1, HTTP/2, and the brand-new HTTP/3 [^h3lib]
- gRPC is also supported
- Serving multiple domain names with TLS termination
- Mutual TLS authentication with client certificates
- Automated certificate issuance and renewal via TLS-ALPN-01 ACME protocol [^acme]
- Post-quantum key exchange for TLS/QUIC [^kyber]
- TLS connection sanitization to avoid domain fronting [^sanitization]
- Load balancing with round-robin, random, and sticky sessions
- and more...

[^h3lib]: HTTP/3 is enabled thanks to [`quinn`](https://github.com/quinn-rs/quinn), [`s2n-quic`](https://github.com/aws/s2n-quic) and [`hyperium/h3`](https://github.com/hyperium/h3). HTTP/3 libraries are mutually exclusive. You need to explicitly specify `s2n-quic` with `--no-default-features` flag. Also note that if you build `rpxy` with `s2n-quic`, then it requires `openssl` just for building the package.

[^acme]: `rpxy` supports the automatic issuance and renewal of certificates via [TLS-ALPN-01 (RFC8737)](https://www.rfc-editor.org/rfc/rfc8737) of [ACME protocol (RFC8555)](https://www.rfc-editor.org/rfc/rfc8555) thanks to [`rustls-acme`](https://github.com/FlorianUekermann/rustls-acme).

[^kyber]: `rpxy` supports the hybridized post-quantum key exchange [`X25519MLKEM768`](https://www.ietf.org/archive/id/draft-kwiatkowski-tls-ecdhe-mlkem-02.html)[^kyber] for TLS/QUIC incoming and outgoing initiation thanks to [`rustls-post-quantum`](https://docs.rs/rustls-post-quantum/latest/rustls_post_quantum/). This is already a default feature.  Also note that `X25519MLKEM768` is still a draft version yet this is widely used on the Internet.

[^sanitization]: By default, `rpxy` provides *TLS connection sanitization* by correctly binding a certificate used to establish a secure channel with the backend application. Specifically, it always maintains consistency between the given SNI (server name indication) in `ClientHello` of the underlying TLS and the domain name given by the overlaid HTTP HOST header (or URL in Request line). We should note that NGINX doesn't guarantee such consistency by default. To achieve this, you have to add an `if` statement in the NGINX configuration file.

This project is still *work-in-progress*. However, it is already working in some production environments and serves a number of domain names. Furthermore, it *significantly outperforms* NGINX and Caddy, e.g., *30% ~ 60% or more faster than NGINX*, in very simple HTTP reverse-proxy scenarios (See [`bench`](./bench/) directory).

## Installing/Building an Executable Binary of `rpxy`

### Building from Source

You can build an executable binary yourself by checking out this Git repository.

```bash
# Cloning the git repository
% git clone https://github.com/junkurihara/rust-rpxy
% cd rust-rpxy

# Update submodules
% git submodule update --init

# Build (default: QUIC and HTTP/3 is enabled using `quinn`)
% cargo build --release

# If you want to use `s2n-quic`, build as follows. You may need several additional dependencies.
% cargo build --no-default-features --features http3-s2n --release
```

Then you have an executable binary `rust-rpxy/target/release/rpxy`.

### Package Installation for Linux (RPM/DEB)

You can find the Jenkins CI/CD build scripts for `rpxy` in the [./.build](./.build) directory.

Prebuilt packages for Linux RPM and DEB are available at [https://rpxy.gamerboy59.dev](https://rpxy.gamerboy59.dev), provided by [@Gamerboy59](https://github.com/Gamerboy59).

Note that we do not have an installation option via [`crates.io`](https://crates.io/), i.e., `cargo install`, at this point since some dependencies are not yet published. Alternatively, you can use the docker image (see below) as the easiest way for `amd64` environments.

## Usage

`rpxy` always refers to a configuration file in TOML format, e.g., `config.toml`. You can find an example of the configuration file, `config-example.toml`, in this repository.

You can run `rpxy` with a configuration file like

```bash
% ./target/release/rpxy --config config.toml
```

`rpxy` tracks changes to `config.toml` in real-time and applies changes immediately without restarting the process. [^hot_reload]

[^hot_reload]: Note that if `config.toml` is removed by `rm` command when `rpxy` is running, `rpxy` stops itself (inode is gone). On the other hand, if `config.toml` is renamed or moved (including moving to trash), `rpxy` continues running with the last valid configuration until a new file named `config.toml` is created in the same directory. So be careful when you remove or rename the configuration file.

The full help message is as follows.

```bash:
usage: rpxy [OPTIONS] --config <FILE>

Options:
  -c, --config <FILE>      Configuration file path like ./config.toml
  -l, --log-dir <LOG_DIR>  Directory for log files. If not specified, logs are printed to stdout.
  -h, --help               Print help
  -V, --version            Print version
```

If you set `--log-dir=<log_dir>`, the log files are created in the specified directory. Otherwise, the log is printed to stdout.

- `${log_dir}/access.log` for access log
<!-- - `${log_dir}/error.log` for error log -->
- `${log_dir}/rpxy.log` for system and error log

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

In the above setting, `rpxy` listens on port 80 (TCP) and serves incoming cleartext HTTP requests that include `app1.example.com` in their HOST header or URL in their Request line.
For example, request messages like the following.

```http
GET http://app1.example.com/path/to HTTP/1.1\r\n
```

or

```http
GET /path/to HTTP/1.1\r\n
HOST: app1.example.com\r\n
```

Otherwise, a request to `other.example.com` is simply rejected with status code `40x`.

If you want to host multiple distinct domain names on a single IP address/port, simply create multiple `app."<app_name>"` entries in the config file like

```toml
default_app = "app1"

[apps.app1]
server_name = "app1.example.com"
#...

[apps.app2]
server_name = "app2.example.org"
#...
```

Note that by specifying a `default_app` entry, *HTTP* requests will be served by the specified application if the HOST header or URL in the Request line doesn't match any `server_name`s in `reverse_proxy` entries. For HTTPS requests, it will be rejected since a secure connection cannot be established for an unknown server name.

#### HTTPS to Backend Application

The request message will be routed to the backend application specified with the domain name `app1.localdomain:8080` or IP address over cleartext HTTP. If the backend channel needs to serve TLS, like forwarding to `https://app1.localdomain:8080`, you need to enable the `tls` option for the location.

```toml
reverse_proxy = [
  { location = 'app1.localdomain:8080', tls = true }
]
```

#### Load Balancing

You can specify multiple backend locations in the `reverse_proxy` array for *load-balancing* with an appropriate `load_balance` option. Currently it works in a round-robin manner, randomly, or round-robin with *session-persistence* using cookies. If `load_balance` is not specified, the first backend location is always chosen.

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

First of all, you need to specify a port `listen_port_tls` listening for HTTPS traffic, separately from the HTTP port (`listen_port`). Then, serving an HTTPS endpoint can be easily done for your desired application by simply specifying TLS certificates and private keys in PEM files.

```toml
listen_port = 80
listen_port_tls = 443

[apps."app_name"]
server_name = 'app1.example.com'
tls = { tls_cert_path = 'server.crt',  tls_cert_key_path = 'server.key' }
reverse_proxy = [{ upstream = [{ location = 'app1.local:8080' }] }]
```

In the above setting, both cleartext HTTP requests to port 80 and encrypted HTTPS requests to port 443 are routed to the backend `app1.local:8080` in the same manner. If you don't need to serve cleartext requests, just remove `listen_port = 80` and specify only `listen_port_tls = 443`.

Note that the private key specified by `tls_cert_key_path` must be *in PKCS8 format*. (See TIPS to convert PKCS1 formatted private keys to PKCS8 format.)

#### Redirecting Cleartext HTTP Requests to HTTPS

In the current Web, it is common to serve everything through HTTPS rather than HTTP, and hence *HTTPS redirection* is often used for HTTP requests. When you specify both `listen_port` and `listen_port_tls`, you can enable such redirection by setting `https_redirection` to true.

```toml
tls = { https_redirection = true, tls_cert_path = 'server.crt', tls_cert_key_path = 'server.key' }
```

If it is true, `rpxy` returns status code `301` to the cleartext request with the new location `https://<requested_host>/<requested_query_and_path>` served over TLS.

### Third Step: More Flexible Routing Based on URL Path

`rpxy` can, of course, route requests to multiple backend destinations according to path information. The routing information can be specified for each application (`server_name`) as follows.

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

In the above example, a request to `https://app1.example.com/path/to?query=ok` matches the second `reverse_proxy` entry in a longest-prefix-matching manner, and will be routed to `path.backend.local` while preserving path and query information, i.e., served as `http://path.backend.local/path/to?query=ok`.

On the other hand, a request to `https://app1.example.com/path/another/xx?query=ng` matching the third entry is routed with *its path information being rewritten* as specified by the `replace_path` option. Namely, the matched `/path/another` part is rewritten to `/path`, and it is served as `http://another.backend.local/path/xx?query=ng`.

Requests that don't match any paths will be routed by the first entry that doesn't have the `path` option, which serves as the *default destination*. In other words, unless every `reverse_proxy` entry has an explicit `path` option, `rpxy` rejects requests that don't match any paths.

#### Simple Path-based Routing

This path-based routing option would be sufficient in many cases. For example, you can serve multiple applications with one domain by specifying a unique path for each application. More specifically, see the example below.

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

This example configuration demonstrates a very common path-based routing situation. When a request to `app.example.com/subappN` is routed to `subappN.local` by replacing the path part `/subappN` with `/`.

## More Options

Since this is currently a work-in-progress project, we are frequently adding new options. We first add new option entries in `config-example.toml` as examples. Please refer to it for up-to-date options. We will prepare comprehensive documentation for all options.

## Using Docker Image

You can also use the `docker` image hosted on [Docker Hub](https://hub.docker.com/r/jqtype/rpxy) and [GitHub Container Registry](https://github.com/junkurihara/rust-rpxy/pkgs/container/rust-rpxy) instead of directly executing the binary. See the [`./docker`](./docker/README.md) directory for more details.

## Example

The [`./bench`](./bench/) directory contains a very simple example of `rpxy` configuration. This can also serve as an example of a docker use case.

## Experimental Features and Caveats

### HTTP/3

`rpxy` can serve HTTP/3 requests thanks to `quinn`, `s2n-quic` and `hyperium/h3`. To enable this experimental feature, add an entry `experimental.h3` in your `config.toml` as follows. Any values in the entry like `alt_svc_max_age` are optional.

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

However, currently we have a limitation on HTTP/3 support for applications that enable client authentication. If an application is configured with client authentication, HTTP/3 doesn't work for that application.

### Hybrid Caching Feature with Temporary File and On-Memory Cache

If `[experimental.cache]` is specified in `config.toml`, you can leverage the local caching feature using temporary files and on-memory objects. An example configuration is as follows.

```toml
# If this specified, file cache feature is enabled
[experimental.cache]
cache_dir = './cache'                # optional. default is "./cache" relative to the current working directory
max_cache_entry = 1000               # optional. default is 1k
max_cache_each_size = 65535          # optional. default is 64k
max_cache_each_size_on_memory = 4096 # optional. default is 4k if 0, it is always file cache.
```

A *storable* (in the context of an HTTP message) response is stored if its size is less than or equal to `max_cache_each_size` in bytes. If it is also less than or equal to `max_cache_each_size_on_memory`, it is stored as an in-memory object. Otherwise, it is stored as a temporary file. Note that `max_cache_each_size` must be greater than or equal to `max_cache_each_size_on_memory`. Also note that once `rpxy` restarts or the config is updated, the cache is completely eliminated not only from the in-memory table but also from the file system.

### Automated Certificate Issuance and Renewal via TLS-ALPN-01 ACME Protocol

This is a brand-new feature and may still be unstable. Thanks to [`rustls-acme`](https://github.com/FlorianUekermann/rustls-acme), automatic issuance and renewal of certificates are finally available in `rpxy`. To enable this feature, you need to specify the following entries in `config.toml`.

```toml
# ACME enabled domain name.
# ACME will be used to get a certificate for the server_name with ACME tls-alpn-01 protocol.
# Note that acme option must be specified in the experimental section.
[apps.localhost_with_acme]
server_name = 'example.org'
reverse_proxy = [{ upstream = [{ location = 'example.com', tls = true }] }]
tls = { https_redirection = true, acme = true } # do not specify tls_cert_path and/or tls_cert_key_path
```

For the ACME enabled domain, the following settings are referred to acquire a certificate.

```toml
# Global ACME settings. Unless specified, ACME is disabled.
[experimental.acme]
dir_url = "https://localhost:14000/dir" # optional. default is "https://acme-v02.api.letsencrypt.org/directory"
email = "test@example.com"
registry_path = "./acme_registry"       # optional. default is "./acme_registry" relative to the current working directory
```

The above configuration is common to all ACME-enabled domains. Note that the HTTPS port must be open to the public to verify domain ownership.

## TIPS

### Set Custom Port for HTTPS Redirection

Consider a case where `rpxy` is running in a container. When the container manager maps port A (e.g., 80/443) of the host to port B (e.g., 8080/8443) of the container for HTTP and HTTPS, `rpxy` must be configured with port B for `listen_port` and `listen_port_tls`. However, when you want to set `https_redirection=true` for some backend apps, `rpxy` issues redirection response 301 with port B by default, which is not accessible from outside the container. To avoid this, you can set a custom port for the redirection response by specifying `https_redirection_port` in `config.toml`. In this case, port A should be set for `https_redirection_port`, then redirection response 301 will be issued with port A.

```toml
listen_port = 8080
listen_port_tls = 8443
https_redirection_port = 443
```

### Using Private Keys Issued by Let's Encrypt

If you obtain certificates and private keys from [Let's Encrypt](https://letsencrypt.org/), you have PKCS1-formatted private keys. You need to convert such retrieved private keys into PKCS8 format to use them in `rpxy`.

The easiest way is to use `openssl`:

```bash
% openssl pkcs8 -topk8 -nocrypt \
    -in your_domain_from_le.key \
    -inform PEM \
    -out your_domain_pkcs8.key.pem \
    -outform PEM
```

### Client Authentication Using Client Certificates Signed by Your Own Root CA

First, you need to prepare a CA certificate used to verify client certificates. If you do not have one, you can generate a CA key and certificate using OpenSSL commands as follows. *Note that `rustls` accepts X509v3 certificates and rejects SHA-1, and that `rpxy` relies on Version 3 extension fields of `KeyID`s of `Subject Key Identifier` and `Authority Key Identifier`.*

1. Generate a CA key of `secp256v1`, CSR, and then generate a CA certificate that will be set for `tls.client_ca_cert_path` for each server app in `config.toml`.

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

2. Generate a client key of `secp256v1` and certificate signed by the CA key.

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

  Now you have a client key `client.key` and certificate `client.crt` (version 3). A `pfx` (`p12`) file can be generated as follows:

  ```bash
  % openssl pkcs12 -export -inkey client.key -in client.crt -certfile client.ca.crt -out client.pfx
  ```

  Note that on macOS, a `pfx` generated by `OpenSSL 3.0.6` cannot be imported to macOS Keychain Access. We generated the sample `pfx` using `LibreSSL 2.8.3` instead of `OpenSSL`.

  All sample certificate files can be found in the `./example-certs/` directory.

### (Work Around) Deployment on Ubuntu 22.04 LTS Using Docker Behind `ufw`

Basically, Docker automatically manages your iptables if you use the port-mapping option, i.e., `--publish` for `docker run` or `ports` in `docker-compose.yml`. This means you do not need to manually expose your port, e.g., 443 TCP/UDP for HTTPS, using `ufw`-like management commands.

However, we found that if you want to use the brand-new UDP-based protocol, HTTP/3, on `rpxy`, you need to explicitly expose your HTTPS port using `ufw`-like commands.

```bash
% sudo ufw allow 443
% sudo ufw enable
```

Your Docker container can receive only TCP-based connections, i.e., HTTP/2 or earlier, unless you manually manage the port. We see that this is strange and expect that it is some kind of bug (of Docker? Ubuntu? or something else?). But at least for Ubuntu 22.04 LTS, you need to handle it as described above.

### Managing `rpxy` via Web Interface

Check the third-party project [`Gamerboy59/rpxy-webui`](https://github.com/Gamerboy59/rpxy-webui) to manage `rpxy` via a web interface.

### Other TIPS

todo!

## Credits

`rpxy` cannot be built without the following projects and inspirations:

- [`hyper`](https://github.com/hyperium/hyper) and [`hyperium/h3`](https://github.com/hyperium/h3)
- [`rustls`](https://github.com/rustls/rustls)
- [`tokio`](https://github.com/tokio-rs/tokio)
- [`quinn`](https://github.com/quinn-rs/quinn)
- [`s2n-quic`](https://github.com/aws/s2n-quic)
- [`rustls-acme`](https://github.com/FlorianUekermann/rustls-acme)

## License

`rpxy` is free, open-source software licensed under MIT License.

## Security

If you discover a security vulnerability, **do not open a public Issue**.
Please use [GitHub's Private vulnerability reporting](../../security/advisories/new) to notify the maintainers.

## Contributing

Contributions are welcome (issues, feature requests, bug reports, pull requests).

Please note that this project is maintained primarily based on the code owner’s personal interests, and not backed by any commercial agreement.
Contributions are handled on a best-effort basis. Sponsorship is also welcome to help sustain the project.

For more details on contribution guidelines and project scope, please see [CONTRIBUTING.md](./CONTRIBUTING.md).

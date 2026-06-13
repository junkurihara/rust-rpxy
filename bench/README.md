# Benchmarking rpxy

> [!WARNING]
> **The benchmark numbers previously published here have been removed because they are not reliable.**
> They were produced in a heavily environment-limited setup and reflect the test environment far
> more than the proxies themselves, so they must **not** be read as a performance ranking:
>
> - The runs were executed under **Docker Desktop on macOS** (a **Mac mini, M4 Pro** for
>   `arm64`, and an Intel iMac for `amd64`) — i.e. inside a **Linux VM on macOS, not bare metal**.
>   Container traffic passes through Docker Desktop's port-forwarding and the VM network layer,
>   which is high-overhead and high-variance and tends to dominate over the actual efficiency of
>   the proxy under test.
> - The load generator (**`rewrk`**) can itself become **client- or network-bound** and then
>   report misleading numbers that reflect the client/VM network rather than the server.
> - In those runs the **nginx side showed connection errors / degraded behavior** (e.g.
>   `512 Errors: connection closed`), so the comparison was not apples-to-apples.
>
> In short, those figures measured the environment, not the proxies. We may publish updated,
> controlled measurements in the future once the methodology is solid.

## What's in this directory

A simple reverse-proxy benchmarking **harness** (configuration and scripts — no results) for
comparing `rpxy` against other reverse proxies (`nginx`, `caddy`, `sozu`) over HTTP/1.1:

- `docker-compose.yml` / `docker-compose.amd64.yml` — a backend plus the proxies under test
- `nginx.conf`, `Caddyfile`, `rpxy.toml`, `sozu-config.toml` — the proxy configurations
- `bench.sh` / `bench.amd64.sh` — driver scripts using [`rewrk`](https://github.com/lnx-search/rewrk)
  (and [`wrk`](https://github.com/wg/wrk) for `sozu`)

## How to run

Bring up the stack and run the driver script, for example:

```sh
docker compose -f docker-compose.yml up -d
./bench.sh
```

Each proxy is driven with a command such as:

```sh
rewrk -c 512 -t 4 -d 15s -h http://localhost:8080 --pct
```

## Measuring meaningfully

Reverse-proxy performance is **highly dependent on hardware, configuration, and environment**,
and a single published figure rarely transfers to another setup. To get numbers you can trust
for your own deployment, measure on your **own target hardware**, and:

- Use a **server-bound** load generator (e.g. [`oha`](https://github.com/hatoo/oha) or
  [`wrk`](https://github.com/wg/wrk)) and confirm the load generator is not itself the
  bottleneck — a quick sanity check: if two different proxies report near-identical throughput,
  the client (not the server) is probably the ceiling.
- **Pin** the proxy and the load generator to **disjoint physical cores**, and prefer **bare
  metal over a VM**, so you measure the proxy rather than scheduling/VM noise.
- Compare like-for-like configurations (TLS vs plaintext, HTTP/1.1 vs HTTP/2, keep-alive,
  response size) and run multiple repeats to gauge the noise floor.

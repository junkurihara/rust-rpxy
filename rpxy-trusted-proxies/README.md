# rpxy-trusted-proxies

Small helper crate for resolving `trusted_forwarded_proxies` entries used by
`rpxy`.

Its current role is:

- accept user-facing entries such as CIDRs and built-in alias names
- expand built-in aliases like `cloudflare`, `fastly`, and `cloudfront` into concrete CIDR sets
- return a resolved `Vec<IpNet>` to the main config parser

This crate intentionally keeps alias resolution separate from the main proxy
runtime so that provider-specific IP-range metadata does not leak into request
handling code.

## Scope

At the moment this crate uses built-in static snapshots for supported providers.
It does **not** fetch provider IP lists at runtime when
`resolve_trusted_proxy_entries()` is called.

That is intentional:

- configuration parsing should stay deterministic
- startup should not depend on external network reachability
- providers may be unreachable from restricted environments
- runtime fetch would introduce timeout, caching, and failure-policy questions

If automatic updates are added later, this crate is the intended place for that
logic, but it should be done as an explicit update workflow or cacheable
metadata refresh mechanism, not as an implicit network fetch during normal
configuration parsing.

## Snapshot Maintenance

The built-in provider snapshots can be refreshed explicitly with:

```sh
cargo run -p rpxy-trusted-proxies --features update-snapshots --bin update-snapshots --
```

The `update-snapshots` feature is required because the updater pulls in heavier
networking / serialization dependencies (`reqwest`, `chrono`, `serde`,
`serde_json`) that the runtime resolver itself does not need. Regular rpxy
builds consume only the lightweight resolver and are unaffected.

Useful flags:

- `--provider cloudflare` to refresh a single provider snapshot
- `--provider all` to refresh all built-in providers
- `--check` to verify whether `src/snapshots.rs` is up to date without writing
- `--timeout-seconds 10` to change the fetch timeout

This command fetches provider IP lists from their official endpoints and
rewrites `src/snapshots.rs` only when the rendered snapshot actually changes.
Normal config parsing and proxy startup still use the checked-in static
snapshot data.

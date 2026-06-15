# CHANGELOG

## 0.13.1 or 0.14.0 (Unreleased)

### Bugfix

- **Fix: detect the `TE: trailers` token per RFC 9110 (case-insensitive, OWS includes HTAB).** Detecting whether the incoming request signalled willingness to accept trailers compared the token as raw bytes against `b"trailers"` and split list items on comma or space only, so any non-lowercase spelling - `Trailers`, `TRAILERS`, or `Trailers` inside a list (`gzip, Trailers`) - was silently missed, as was a tab-separated list (`gzip,\tTrailers`), and the upstream-bound request was sent without the `te: trailers` header it should have had. RFC 9110 defines TE values as `#t-codings`, specifies transfer-coding tokens as case-insensitive (§5.6.6 / §10.1.4), and defines OWS around the comma as `*(SP / HTAB)` (§5.6.3); the local convention in `extract_upgrade`/`hop.rs` already uses `eq_ignore_ascii_case`. The match now uses `<[u8]>::eq_ignore_ascii_case` and the splitter additionally accepts `\t`, with the detection extracted into a small private free function for direct unit testing; the surrounding `.map().unwrap_or(false)` collapses to `is_some_and(...)`. Pinned by new tests covering lowercase / titlecase / uppercase, position-in-list with both SP and HTAB separators, unrelated tokens, the empty header, and a `trailers-extra` substring safety net. No unrelated code path is changed; lowercase TE-trailers inputs keep the same upstream request bytes. Non-lowercase or tab-separated TE-trailers inputs now correctly emit `te: trailers` upstream, which - per RFC 9110 - may let the backend send response trailers that are forwarded downstream.

### Improvement

- **Simplify `remove_connection_header` by taking the `Connection` header out of the map upfront.** Removing the per-hop headers listed by a request's / response's `Connection` value cloned the whole `HeaderValue` so its `to_str()` borrow detached from the `HeaderMap`, allowing the subsequent `headers.remove(name)` calls. The function now removes the `Connection` header itself first (yielding an owned `HeaderValue` whose borrow is independent of the map), then iterates the comma-separated name list and removes each listed header - no `HeaderValue` clone, no scratch allocation. The contract is widened to "this function also removes `Connection` itself"; both call sites (`handler_manipulate_messages.rs:30, 90`) pair this function with `remove_hop_header` (which already lists `header::CONNECTION`), so the end-to-end behaviour is unchanged. `HOP_HEADERS` keeps `CONNECTION` as defensive redundancy. Pinned by new `hop::tests` cases covering listed-name removal, case-insensitivity, whitespace/empty-segment handling, an absent header, unparseable names being silently skipped, and the new `Connection`-self removal. Readability cleanup only.
- **Simplify `drop_port` (the Host-header / URI host port-stripping helper) and drop its dead error arms.** `Request::inspect_parse_host` stripped the `:port` suffix via three `.split(..).next().ok_or_else(..)` chains whose `Err` branches were unreachable (`.split()` on a slice always yields at least one element), and detected bare IPv6 by subtracting the sum of segment lengths from the total slice length (a colon-count computed the long way round). The helper is now a small private free function returning `Vec<u8>` directly: bracketed IPv6 uses `strip_prefix(b"[")` + `position(b']')` (the previous lenient unterminated-bracket case is preserved), bare IPv6 uses `iter().filter(|&&b| b == b':').take(2).count() == 2` (short-circuits after the second colon instead of scanning the whole value), and the v4/hostname branch cuts at the first `:` and ASCII-lowercases. The outer match in `inspect_parse_host` collapses its now-unreachable `Some(Ok)`/`Some(Err)` distinction into `Some(_)` / `(None, None) => Err(..)`, and the `InspectParseHost` trait's public signature is unchanged. Same byte output for every input the previous code reached (v4 / hostname / bracketed v6 / bare v6, with and without port; empty Host; the lenient `[v6` no-`]` case), pinned by a `mod tests` covering those eleven cases. Readability cleanup only; no observable behavior change.
- **Render `ServerName` directly via `Display` instead of a `TryInto<String>` roundtrip everywhere it was logged or formatted.** Five sites (`handler_main` redirect debug log, the H/3 connection-established debug log, the two `NoTlsServingApp` error arms in the TLS acceptor, and the HTTPS redirect URL builder) rendered the server name with `<&ServerName as TryInto<String>>::try_into(..).unwrap_or_default()` and silently substituted `""` on UTF-8 failure. `ServerName` now implements `Display` (zero-allocation on the UTF-8 path the request flow normally produces; falls back to U+FFFD via `from_utf8_lossy` instead of swallowing the host when the byte constructors are fed non-UTF-8 input); the two debug logs read the value through `{}` directly, and the two error / one URL sites use `.to_string()` (same byte output, same allocation count, no turbofish). The two debug-log sites also drop one `String` allocation per emitted line (the disabled-callsite path was already a no-op); the error and URL sites are unchanged allocation-wise. The `TryInto<String> for &ServerName` impl is kept so external/downstream callers are unaffected. Cosmetic cleanup only.
- **Parse the sticky cookie token by splitting on the first `=` instead of collecting every `=`-separated slice (`sticky-cookie` feature).** `StickyCookieValue::try_from` split the cookie token on every `=` into a `Vec<&str>` and required exactly two slices, allocating a two-element vector per sticky-LB request and rejecting any value that itself contained `=`. It now uses `split_once('=')`: no allocation, and the value keeps any further `=` (a cookie `name=value` pair is defined by the first `=`; the value may contain more). The wire value is a base64 NO_PAD AEAD blob that never contains `=` today, so this is observably unchanged - a malformed multi-`=` token is still ignored, now rejected one step later by the sticky-blob length / NO_PAD decode / AEAD verification in `open_server_id` rather than by the structural check - while removing the brittleness against a future padded-base64 value. The exact-name and empty-value rejections are unchanged, pinned by the existing sticky-cookie suite plus new `try_from` cases (empty value, missing `=`, and a value containing `=`). Allocation/readability cleanup on the sticky-LB path only.
- **Enable stateless TLS session resumption (session tickets) for non-mTLS apps and HTTP/3, and drop server-side TLS session caching for the app-serving TLS configurations.** rpxy previously relied on rustls's built-in fallback session cache (256 entries per server config, mutex-guarded, discarded on every certificate hot-reload), so returning clients effectively never resumed and every TLS handshake paid the full certificate/key-exchange cost. Non-mTLS server configurations — including the HTTP/3 (`http3-quinn`) one — now share a single process-wide RFC 5077 session-ticket issuer (AES-256/HMAC-SHA256, keys rotated every 6 hours, tickets valid up to 12 hours): resumption works regardless of traffic volume, keeps working across certificate hot-reloads, and the in-memory session cache (and its per-handshake lock) is gone — TLS 1.2 clients resume via tickets as well, while legacy TLS 1.2 clients without session-ticket support lose only the previously ineffective cache. mTLS apps now never resume a TLS session: they issue no tickets and no longer use the fallback session cache either. Previously a returning mTLS client could occasionally resume from that cache, silently skipping client-certificate re-verification — and thereby escaping the handshake-failure audit, which can only log verification attempts; now client-certificate verification runs on every mTLS connection, matching the industry practice of disabling resumption for mutual TLS. Restarting rpxy invalidates outstanding tickets; clients then silently perform one full handshake. The `http3-s2n` backend is unaffected: it builds its TLS configuration through s2n-quic's own rustls provider and reuses only the certificate resolver and ALPN list from rpxy's configuration. The ACME TLS-ALPN challenge listener is likewise a separate configuration and is unchanged.
- **Skip building the access-log record when no access log can be emitted, and write the file access log through a dedicated minimal formatter.** Two related cleanups driven by an instruction-level profile that attributed ~31% of the plaintext keep-alive data-path instructions to access logging. First, every request used to construct the access-log record (`HttpMessageLog`: method/version capture, four header clones, the upstream URI clone) unconditionally, with the level check happening only at the final emit — so configurations whose logger can never emit an access line (stdio logging with `RUST_LOG=warn|error`) paid the full capture cost for nothing. Whether the installed logger can emit access lines is now determined once at startup (file logging: always; stdio: only when the resolved `RUST_LOG` level admits INFO) and threaded into the request handler, which skips the record entirely when no line can result; the predicate shares its level resolution with the logger setup and is pinned to the actual installed filters by tests, so it cannot silently disagree with them. One visible trade-off in those quiet-stdio configurations only: ERROR system-log lines for failed requests now carry the error and client address instead of the full request summary (which no longer exists). Second, the file-mode access log — always on, the normal production configuration — used to render each line through the generic compact formatter, which re-processes the already-formatted message character by character through an ANSI-sanitizing writer; since every byte that sanitizer could transform is unreachable in access lines (header values and URI components are restricted to visible ASCII by HTTP parsing), the access-log file layer now writes the timestamp and message in a single pass. The emitted bytes are identical (pinned by golden tests against the generic formatter, including header values containing quotes and backslashes); the stdio log format, system logs, and the mTLS handshake audit logs are unchanged. CPU/allocation cleanup; end-to-end throughput was not re-measured.
- **Stop re-parsing hop-by-hop header names on every request.** Removing the hop-by-hop headers (`Connection`, `TE`, `Trailer`, `Keep-Alive`, `Proxy-Connection`, `Proxy-Authenticate`, `Proxy-Authorization`, `Transfer-Encoding`, `Upgrade`) from the proxied request and response passed the names as `&str` keys, so the `http` crate parsed each string into a `HeaderName` and hashed it on every removal — 9 names twice per request (request towards the upstream and response towards the client), i.e. 18 string parses plus hashes per proxied request. An instruction-level profile (callgrind) of the plaintext HTTP/1.1 keep-alive path with access logging disabled attributed roughly 10% of the data-path instructions to this name parsing and hashing. The names are now pre-built `HeaderName` constants (constructed at compile time), so removal skips the string parse entirely and standard names take the `http` crate's pre-hashed fast path. The set of removed headers, case-insensitive matching, and all proxied bytes are unchanged. CPU cleanup only; end-to-end throughput was not re-measured.
- **Assemble the outgoing forwarding headers in single buffers instead of intermediate string lists.** Generating the outgoing `X-Forwarded-For` built one `String` per hop plus a list and a joined copy before the header value was created, and generating the RFC 7239 `Forwarded` header built up to four `String`s per hop plus two levels of join; the trusted-proxy path additionally cloned the immediate-peer chain entry on every request only to feed a defensive branch that is unreachable (the trust-boundary reduction always retains the appended peer hop — the branch is kept, but now rebuilds the entry instead of pre-cloning it). Each emitted header is now written left to right into one buffer: `X-Forwarded-For` hands that buffer to the header value without re-copying, `X-Real-IP` is sliced out of its first element (same bytes as before, one copy — previously the copy direction was reversed), and the `Forwarded` writer reproduces the exact RFC 7230/7239 quoting rules (tchar fast path, quoted-pair escaping, IPv6 bracketing), pinned by the existing byte-exact tests plus new cases covering the rare `for=` node forms (`unknown`/obfuscated, with and without ports) and quote/backslash escaping. Header parsing, the trust-boundary normalization logic, and every emitted byte are unchanged; this is a CPU/allocation cleanup of the output assembly only (common single-hop path: 4 allocations → 2; each `Forwarded` hop: ~4–6 → a shared buffer). End-to-end throughput was not re-measured.
- **Compute the authoritative request host once per request.** The "authoritative host" of the incoming request (URI host preferred, port included, falling back to the `Host` header) was recomputed — with a fresh `String` allocation each time — at up to four places per request: the peer `Forwarded` entry, the `X-Forwarded-Host` rewrite, the optional RFC 7239 `Forwarded` generation (`forwarded_header` upstream option), and the access log. It is now computed once at the top of the request-forwarding path, from the original URI and `Host` header captured before any header mutation, and handed by reference to every consumer; this is equivalent by construction because every `Host` rewrite (`set_upstream_host`, the `default_app` hardening, the missing-`Host` insertion) runs only after the forwarding headers are built. The helper itself now also borrows instead of allocating in the common cases (URI host without an explicit port, `Host`-header fallback), which additionally removes one allocation per emitted access-log line. The emitted `X-Forwarded-Host`, `Forwarded` (`host=`), upstream `Host` handling, trust-boundary normalization, routing, and access-log bytes are all unchanged, guarded by the existing forwarding/trusted-proxy test suite plus new pinning tests for every authoritative-host source. CPU/allocation cleanup only (roughly one to two heap allocations per request plus the duplicate computations); end-to-end throughput was not re-measured.
- **Render IPv4 addresses in the forwarding headers without the generic formatter.** Writing the client/peer IPv4 into `X-Forwarded-For` (and the `X-Real-IP` it is sliced from) and into the RFC 7239 `Forwarded` `for=` node went through `std`'s `Ipv4Addr`/`IpAddr` `Display`, which routes each octet through the general-purpose `core::fmt` integer path (`pad_integral`). An instruction-level profile of the behind-proxy path attributed a noticeable slice (~3.4% of per-request instructions) to this IP/integer formatting, and a callgrind microbench on the build toolchain measured std `Ipv4Addr` Display at ~1075 instructions per address versus ~262 with `itoa` (about 4x fewer). The IPv4 octets are now written with the `itoa` crate (already present in the dependency tree, now a direct dependency of `rpxy-lib`); IPv6 deliberately stays on the std formatter, which already emits the RFC 5952 canonical (`::`-compressed) form that `itoa` (decimal-only) cannot produce, and the port already used `HeaderValue::from(u16)`. The emitted `X-Forwarded-For`, `X-Real-IP`, and `Forwarded` bytes are unchanged, pinned by a new boundary test asserting byte-equality with `IpAddr::to_string()` across the IPv4 octet ranges plus the existing forwarding/`Forwarded` test suite. CPU cleanup on the forwarding path only; end-to-end throughput was not re-measured.
- **Skip rebuilding the `Cookie` header when there is nothing to merge.** Collapsing a request's `Cookie` lines into one line (HTTP/2 clients may split a cookie across several `Cookie` headers) scanned the entire header map on every request, collected the matching values into a `Vec`, `join`-ed them into a fresh `String`, and then removed and re-inserted the header — even when zero or one `Cookie` line was present and there was nothing to merge. It now reads the values directly via `get_all`, returns immediately when fewer than two lines exist (a single line is already single-line), and builds the joined buffer — sized exactly up front — only when two or more lines are present. A callgrind microbench of this step measured the common single-`Cookie` request (typical browser traffic) at roughly 1480 instructions before versus 200 after, with two fewer heap allocations (the `Vec` and the joined `String`) and the remove/insert skipped; the cookieless path is instruction-neutral. The merged bytes, the line ordering, and the lossy `to_str` fallback for non-ASCII values are unchanged, pinned by new tests for the zero-, one-, and multi-line cases. CPU/allocation cleanup only; end-to-end throughput was not re-measured.
- **Precompute the upstream `Host` header value instead of rebuilding it per request.** The `set_upstream_host` upstream option overwrites the request `Host` with the chosen upstream's host. This re-derived the value from the upstream URI on every such request — `host().to_string()`, then (when a port is present) a second `format!("{host}:{port}")` that discarded the first allocation, then `HeaderValue::from_str` re-validating the result — even though the upstream URI, and hence this `Host` value, is fixed at configuration-build time. The value (`host` or `host:port`) is now rendered once when the `Upstream` is built and stored on it; the per-request override clone-inserts the ready `HeaderValue` (a `Bytes` refcount bump — no formatting, no validation, no allocation). A callgrind microbench measured the value production at roughly 2170 instructions before versus 55 after. The emitted `Host` is byte-identical and the `KeepOriginalHost` precedence is unchanged; an upstream URI without a host still yields the same "No hostname is given" error (now raised at the override when the precomputed value is absent), pinned by the existing `set_upstream_host`/`keep_original_host` tests plus a new no-host case. CPU/allocation cleanup only on the `set_upstream_host` path; end-to-end throughput was not re-measured.
- **Read the forwarding headers by borrowing instead of join-then-resplit.** Parsing `X-Forwarded-For` and `Forwarded` (and the sticky-cookie proto readers) first called a helper that collected every field-line value into a `Vec<String>` and `join`-ed them into one `String`, which the caller then immediately tore apart again with `split(',')` — so the common single-field-line case spent three heap allocations (a per-value `String`, the `Vec`, and the joined `String`) to produce a value that is only borrowed and split. The helper now returns a `Cow<str>`: a single field-line is borrowed directly from the header value (no allocation), while the rare multi-field-line case still validates every value and joins them with ", " exactly as before — so an unreadable line after the first still errors, preserving the fail-closed behavior the proto / `Secure` decision depends on. The first-value helper likewise returns a borrowed `&str` (only the one owned copy that is actually stored is kept). A callgrind microbench of the single-line read measured roughly 1140 instructions before versus 835 after (the three eliminated allocations). The parsed chain, the multi-line and empty-segment handling, and every error path are unchanged, pinned by the existing forwarding/trusted-proxy suite plus new helper-level tests (single-line borrows, multi-line joins, and a non-first unreadable line still errors). CPU/allocation cleanup on the behind-proxy parse path only; end-to-end throughput was not re-measured.
- **Dispatch the upstream header options by set membership instead of iterating the option set.** Applying the per-upstream options (`set_upstream_host`, `upgrade_insecure_requests`, `forwarded_header`) iterated the options `HashSet` and matched each variant, with the `KeepOriginalHost`-overrides-`SetUpstreamHost` precedence expressed as a `contains` check nested inside the `SetUpstreamHost` arm. Since the set already answers membership in O(1) and the three actions are independent (each writes a different header and reads none that the others write), the function now tests membership directly: the `Host` override (with its `KeepOriginalHost` precedence as a top-level boolean), the `Upgrade-Insecure-Requests` insertion, and the `Forwarded` generation each run from a plain `if`. The emitted headers on the success path are byte-identical and the option precedence is unchanged, now pinned by the existing host/forwarded tests plus new `Upgrade-Insecure-Requests` cases (inserted when the option is set, absent when unset, and a pre-existing value preserved). This is a readability/maintainability cleanup, not a performance change.
- **Precompute the sticky-cookie name prefix and stop owning every sticky token (`sticky-cookie` feature).** Two cleanups in the same code path. (1) The `"{name}="` cookie prefix used to locate the sticky token was rebuilt with `format!` on every sticky-LB request, in both the takeout and the set paths — yet `name` is a config-fixed value. `StickyCookieConfig` already precomputes its AEAD AAD in `try_new`; the name prefix is now precomputed alongside it (same all-private/`try_new`-only/immutable invariant, so the prefix can never disagree with the name), and both call sites read it via `sticky_config.name_prefix()` instead of formatting. (2) The takeout helper used to `to_string` every sticky token into a `Vec<String>` to release the immutable borrow before mutating `headers`, even though it immediately rejects the request unless exactly one sticky token exists — so at most one token was ever used. It now owns only the first token plus a `multiple_sticky` boolean; the multi-sticky rejection still fires (and still runs after the upstream-`Cookie` rewrite, unchanged). A callgrind microbench of the takeout parse-and-partition step (single-sticky request) measured roughly 2580 instructions before versus 1850 after (~28%), with the per-request `format!` and the surplus `Vec<String>` gone. The matched bytes, the multi-sticky rejection, the AEAD open/verify, and every reject/ignore path are unchanged, pinned by the existing sticky-cookie suite plus a `name_prefix()` precompute test that mirrors the AAD precompute test. CPU/allocation cleanup on the sticky-LB path only; end-to-end throughput was not re-measured.

## 0.13.0

### Important Changes

- **Breaking: rename `https_redirection_port` to `public_https_port`.** The option now represents the client-visible HTTPS/H3 port used by both HTTP->HTTPS redirects and HTTP/3 `Alt-Svc` advertisement. Existing configs using `https_redirection_port` must be updated. If clients already reach the same port as `listen_port_tls`, this option is still unnecessary.

### Bugfix

- **Fix: the file-cache bookkeeping no longer stalls cache publication - or leaks files - under sustained store churn (`cache` feature).** The count of committed file-cache objects was kept behind a read-write lock, and file I/O was performed while holding it: evicting a displaced cache file held the lock exclusively across the file unlink, every newly stored object had to take the same exclusive lock to be counted before its metadata was published, and serving a file-backed hit held a shared guard across the cache-file open. Under sustained store-and-evict pressure (many concurrent cacheable misses for distinct URIs), one slow unlink made every in-flight store queue behind it: publication stopped within seconds, lock waits grew into tens of seconds, and committed-but-never-published cache files accumulated on disk without bound (a synthetic stress test left >135k orphaned files after one minute against an LRU capacity of 10). The count is now a lock-free atomic and unlinks/opens run without holding any lock, so publication can never queue behind another task's file I/O and eviction degrades gracefully at filesystem speed instead of collapsing. Counting semantics (best-effort, saturating, count-before-publish ordering), eviction tolerance for already-missing files, and the integrity-check behavior are unchanged.
- **Fix: HTTP/1.1 responses to slow clients no longer buffer the entire response body in memory.** rpxy enabled hyper's experimental `pipeline_flush` option on HTTP/1.1 server connections (since the initial implementation), which - besides aggregating flushes for pipelined requests - bypasses hyper's per-connection write-buffer cap (~400 KB) and forces the flattened (copying) write strategy. A client reading more slowly than the upstream or the cache produced the body therefore caused the **whole response body, however large, to be copied into that connection's write buffer**: a handful of deliberately slow readers of a large response could grow resident memory by hundreds of megabytes, on any response path (proxied or cached, cleartext or TLS). The option is now left at hyper's default (disabled): the write buffer is capped again, backpressure propagates from the client socket to the upstream read or cache file read, and the write strategy returns to hyper's default (zero-copy queueing with vectored writes where the transport supports them). Pipelined HTTP/1.1 clients still receive correct responses and only lose the flush batching (HTTP/1.1 pipelining is effectively unused by real clients); no throughput change was measured for normal keep-alive traffic.

### Improvement

- **Fix HTTP/3 `Alt-Svc` advertisement for HTTPS-only deployments.** rpxy now advertises HTTP/3 on secure non-mTLS responses when HTTP/3 is enabled, independent of per-app HTTP redirect settings. Plain HTTP responses and mTLS apps do not advertise HTTP/3.
- **Reduce per-request allocations in the forwarding-header path.** Building the outgoing `X-Forwarded-*` / `Proxy` headers no longer re-validates or re-allocates values that are already known: the constant headers (`X-Forwarded-Proto`, `X-Forwarded-Ssl`, `Proxy`) are written via `HeaderValue::from_static`, `X-Forwarded-Port` via `HeaderValue::from(u16)`, and `X-Real-IP` reuses the IP string already computed for `X-Forwarded-For` and is handed to `HeaderValue` without an extra copy. The immediate-peer forwarding entry is also no longer built twice per request, and request host parsing no longer constructs an error value on the success path. The forwarding/trust-boundary logic and every emitted header value are byte-for-byte unchanged. This trims roughly ten heap allocations per request on the common path; it is a CPU/allocation cleanup, not a measured throughput change (no throughput difference was observed on a loopback benchmark).
- **Reduce per-request allocations in path routing and request-URI rebuilding.** Longest-prefix route matching now compares the request path bytes directly instead of allocating a `PathName` per request, and rebuilding the outgoing request URI now reuses the original path-and-query via a shallow clone (instead of copying it into a `Vec` and re-validating it) when no `replace_path` is configured. Routing decisions and the rewritten URI are byte-for-byte unchanged. Like the forwarding-header change above, this is a CPU/allocation cleanup rather than a measured throughput change.
- **Avoid cloning the whole request header map on the sticky-cookie path (`sticky-cookie` feature).** When a request reaches a sticky-session (`StickyRoundRobin`) upstream group, extracting the sticky cookie no longer clones the entire `HeaderMap`; it reads the `Cookie` header(s) directly and only re-materializes the cookie tokens. Which cookie is consumed versus forwarded upstream, the recovered backend id, and all reject/ignore paths are unchanged. CPU/allocation cleanup only.
- **Read file-cache hits in larger chunks (`cache` feature).** Serving a cache hit stored on disk previously read the file into a zero-capacity `BytesMut`, which `read_buf` grows only ~64 bytes at a time — so an 8 KB object was read in ~128 tiny iterations, each allocating a buffer, a copied `Bytes`, and a body frame. The read now fills a 64 KiB buffer and hands each chunk downstream without an extra copy, collapsing a hit from hundreds of allocations to a handful. Large objects still stream a chunk at a time, and the integrity hash check plus eviction on mismatch are unchanged. CPU/allocation cleanup only.
- **Skip the per-hit re-hash of on-memory cache objects (`cache` feature).** Serving a cache hit held in memory previously recomputed a full SHA-256 over the whole object on every hit and compared it to the stored hash. Unlike a file-backed object — which lives on disk as an external, mutable resource and is therefore still hash-verified on every read — an on-memory object is an immutable `Bytes` held inside the same cache entry as its hash and is never mutated after insertion, so re-hashing it on each hit only guarded against in-process RAM corruption (which the stored hash itself equally suffers) at the cost of a full hash per hit. On-memory hits now return the stored object directly. The **file-cache** integrity check is unchanged. CPU/allocation cleanup only.
- **Stream the file-cache store path to disk and bound its memory (`cache` feature).** Storing a cacheable response previously buffered the entire body in memory, hashed it, and only then wrote a file-backed object to disk — so a file-cache object up to `cache_max_each_size` was held in full in RAM before spilling to disk. The store path now hashes incrementally while streaming: a body that crosses the on-memory threshold spills to a temp file and subsequent bytes are written straight to disk, capping the store-path buffer regardless of how large `cache_max_each_size` is configured. The temp file is created with `create_new` and atomically renamed to a generation-unique final path, and the cache metadata is published only after the file is fully written — closing a window where a concurrent reader could see metadata pointing at a not-yet-written file, and letting concurrent stores of the same URI no longer clobber each other's file. Any cache-side failure (too-large body, upstream error, or file I/O error) still forwards the full response to the client and simply skips caching; the file/on-memory selection threshold and the file-cache integrity check are unchanged. This is a memory-bound and correctness cleanup — material mainly when `cache_max_each_size` is configured large — not a measured throughput change at default settings.
- **Raise the default on-memory cache threshold from 4 KiB to 64 KiB (`cache` feature).** `max_cache_each_size_on_memory` now defaults to the same value as `max_cache_each_size` (65,535 bytes), so by default every cacheable object is served from memory; the file-backed tier engages only when `max_cache_each_size` is raised beyond it. Rationale: serving a hit from memory is several times faster than the file-backed path, which opens and reads the cache file on every hit (measured on loopback: ~150k req/s on-memory vs ~36k req/s file-backed for an 8 KiB object), and typical HTML/API responses fall in the 4-64 KiB range that the old default sent to disk. The trade-off is a larger worst-case cache memory footprint: `max_cache_entry` (default 1,000) x this threshold ≈ 64 MB at defaults, versus ~4 MB before — deployments that prefer the old behavior can set `max_cache_each_size_on_memory = 4096` explicitly. Explicitly configured values are unaffected.
- **Drop the second copy when resolving the request host name.** Resolving a request's server name parsed the Host header / request-URI host into an owned, port-stripped byte buffer and then lowercased it into a second freshly allocated buffer. The conversion now lowercases the already-owned buffer in place, removing one allocation and copy per request on the always-on path. The resulting server name bytes are identical for every input, so routing and the SNI consistency check are unchanged. CPU/allocation cleanup only.
- **Precompute the sticky-cookie AEAD AAD at config build time (`sticky-cookie` feature).** The additional authenticated data binding a sticky cookie to its app (name/domain/path) was re-validated and re-assembled on every request that opens or seals a sticky cookie, even though its inputs are fixed when the backend is built. It is now validated and computed once per load-balancer configuration and reused per request; as a side effect, an invalid component (e.g. a NUL byte in a configured path) is rejected at startup/config reload with a proper error instead of failing every request at runtime. The AAD bytes are unchanged, so cookies sealed before this change still open after it, including across a hot config reload. CPU/allocation cleanup only; no behavior change for valid configurations.
- **Bound the cache streaming channels so a slow client no longer queues unbounded response data in memory (`cache` feature).** Serving a file-cache hit and storing a cacheable miss previously relayed body frames to the client over an unbounded in-memory channel: the producer (the disk read, or the upstream response) ran at full speed regardless of how fast the client consumed, so a slow-reading client could queue an entire large cached object — or an entire upstream response — in memory per request. Both paths now relay over a small bounded channel and the producer waits when it is full, capping per-stream queued memory at a few frames (on the order of a few hundred KiB worst case for file-backed hits) and propagating flow control to the file read and to the upstream connection, as the non-cache forwarding path already does. Cache hit/miss decisions, the stored bytes, the integrity check, and every failure-handling path are unchanged; a cache-side failure still never cuts the response to the client. With a fast consumer the channel never fills and behavior is unchanged apart from two extra small allocations per request of channel bookkeeping — this is a memory-robustness improvement for slow-consumer scenarios, not a throughput change. Note: a related, pre-existing slow-client buffering point below this layer (the HTTP/1.1 connection write buffer growing without bound when request pipelining support is enabled, unrelated to the cache) was identified during verification and is fixed in this release (see the Bugfix above).

### Internal

- **Add an off-by-default `dhat-heap` feature for developer heap profiling.** Building `rpxy` with `--features dhat-heap` swaps the global allocator for the [dhat](https://crates.io/crates/dhat) heap profiler and writes a `dhat-heap.json` (viewable with `dhat/dh_view.html`) on a Ctrl-C graceful shutdown, so per-request allocation call-sites can be measured before micro-optimizing the request hot path. The feature is off by default and not built into release binaries: normal builds keep mimalloc (and the system allocator on illumos) and are unchanged in both behavior and dependencies. This is a development aid only; it is not a runtime or configuration change.

## 0.12.1

### Bugfix

- **Fix: the cache no longer truncates responses larger than `cache_max_each_size`.** With the `cache` feature enabled, a cacheable response whose body exceeded `cache_max_each_size` (default 65535 bytes) was truncated when delivered to the client, because the caching worker stopped forwarding the response body to the client as soon as the size limit was crossed. Depending on framing this surfaced either as a silently short body (chunked / unknown-length responses) or as a body/protocol error (when `Content-Length` was present). Such over-limit responses are now forwarded to the client in full and simply not cached; within-limit responses are cached as before. Relatedly, a response whose upstream body errors mid-stream now propagates that error to the client (failing as it did upstream) instead of the cache layer masking it as a clean, truncated end-of-stream.
- **Fix: ACME no longer panics with `static str is not valid path` (`acme = true`).** With ACME enabled, rpxy aborted with that message as soon as it contacted the ACME server. The cause was a transitive dependency used for ACME requests (`async-web-client`, pulled in via `rustls-acme`) that unconditionally constructed `PathAndQuery::from_static("")` on every outgoing request; this panics under `http` 1.4.1, which started rejecting paths that do not begin with `/` (the empty string included). The `http` 1.3 -> 1.4 bump shipped in 0.12.0, so ACME was broken there. rpxy now builds `rustls-acme` against a patched `async-web-client`, restoring ACME certificate provisioning. Configurations that do not use ACME were unaffected. (GitHub Discussion #581.)

### Improvement

- **Enable `TCP_NODELAY` on downstream and upstream connections.** rpxy now disables Nagle's algorithm on accepted client connections (both cleartext and TLS, set on the raw socket right after accept) and on the forwarder's upstream HTTP connector, matching common reverse-proxy practice. This avoids Nagle / delayed-ACK latency on the many small writes a proxy relays; the effect is most visible over connections with non-trivial round-trip time. Health-check probe connections and HTTP/3 (QUIC, UDP) are intentionally left unaffected.
- **Build the access-log record lazily to cut per-request allocations.** The access-log record is now captured as cheap, reference-counted handles (request URI, method, headers) and formatted only when a log line is actually emitted, instead of eagerly building roughly eight owned strings on every request. This removes that per-request work when access logging is filtered out (for example, the stdout logger at `RUST_LOG=warn` or higher; a configured file logger always emits the access log). The emitted log line is byte-for-byte unchanged, and the query-redaction guarantee of `redact_query_in_access_log` is preserved: when redaction is enabled, query values are still masked at capture time so raw query strings are never retained in the record.

## 0.12.0

**Security-focused release with the following improvements and bugfixes.**

### Important Changes

- **Breaking: add `trusted_forwarded_proxies` global option.** This supports deployments where rpxy runs behind another load balancer or reverse proxy that adds `X-Forwarded-For`, `Forwarded`, and related forwarding headers, and those headers should be trusted only when the immediate peer is within explicitly trusted proxy ranges. From this version, no proxy is trusted by default, so requests forwarded from rpxy to backend applications are rebuilt from the immediate peer only. When `trusted_forwarded_proxies` is configured with trusted CIDR blocks, rpxy preserves and normalizes forwarding information learned through those trusted proxies, rewrites outgoing `X-Forwarded-For` and related headers from that normalized chain, and falls back safely when the incoming forwarding view is malformed, inconsistent, or cannot be represented safely.
- Add `cloudflare`, `fastly` and `cloudfront` as a built-in `trusted_forwarded_proxies` alias and add the `rpxy-trusted-proxies` snapshot updater command for explicit provider range refreshes.
- **Breaking: harden `default_app` fallback against untrusted `Host` headers.** When a request matches the `default_app` fallback (i.e., its `Host` does not match any configured `server_name`), rpxy now force-overwrites the outgoing `Host` header with the default app's configured `server_name` regardless of the `keep_original_host` / `set_upstream_host` upstream options. In addition, the `default_app` fallback is now strictly limited to plaintext HTTP; TLS requests with an unknown server name are rejected unconditionally (independent of `sni_consistency`).
- **Sticky cookie security attributes.** The `Set-Cookie` issued by the sticky-session load balancer now always carries `HttpOnly` and `SameSite=Lax`, and additionally carries `Secure` when the client-visible request scheme is HTTPS. Operator-visible behavior changes:
  - Applications that previously read rpxy's sticky cookie from JavaScript (`document.cookie`) will no longer see it.
  - When rpxy itself terminates TLS, `Secure` is set automatically.
  - When rpxy runs behind an external TLS terminator (ALB, CloudFront, Nginx, HAProxy, etc.), the terminator's address must be listed in `trusted_forwarded_proxies` for `Secure` to be applied; rpxy honors `X-Forwarded-Proto: https` (or `Forwarded: proto=https`) only from trusted peers.
  - **Operator requirement.** Any proxy listed in `trusted_forwarded_proxies` must overwrite or normalize incoming `X-Forwarded-Proto` rather than appending a client-supplied value (e.g. Nginx `proxy_set_header X-Forwarded-Proto $scheme;`). Otherwise an attacker upstream of the trusted proxy can spoof the forwarded scheme. ALB and CloudFront satisfy this by default. This is the same operator requirement that 0.12.0 introduced for `X-Forwarded-For` chains.
- **Breaking: sticky cookie values are now opaque AEAD blobs.** Deployments using `load_balance = "sticky"` must configure the new global `sticky_cookie_secret` option as an unpadded base64url-encoded 32-byte secret. The default cookie name changed from `rpxy_srv_id` to `rpxy_sticky_token`; the old name is no longer treated as rpxy's sticky cookie. The sealed token contains the backend identifier and an expiration timestamp mirrored with the cookie `expires` / `Max-Age` attributes; expired, malformed, plaintext, or wrong-secret cookies are ignored and reissued automatically. Rotating the secret intentionally resets sticky-session affinity. Replay remains possible only within the sealed expiration window, so sticky cookies must not be used for authentication decisions.
- **Dependency note:** the sticky-cookie AEAD implementation currently pins `aes-gcm = 0.11.0-rc.4` intentionally for the 0.11 AEAD nonce-generation API. This pre-release dependency must be re-evaluated, replaced with a final 0.11.x release, or explicitly re-approved before the release dependency freeze.
- Rebuild `X-Forwarded-Host` as part of the general forwarding-header policy. rpxy no longer forwards a client-supplied `X-Forwarded-Host` value as-is; instead it rebuilds `X-Forwarded-Host` from the original client-visible host, alongside the other authoritative `X-Forwarded-*` headers. As with `Forwarded: host=`, this value is observational only and must not be used for security decisions.
- Harden TLS private key file permissions on Unix-like systems. Newly-created ACME cache files are now created with mode `0600`, newly-created ACME cache directories with mode `0700`, and existing cache artifacts keep their current modes. Manually provisioned TLS private key files are also checked at load time; rpxy emits a `warn!` log when any group or other permission bit is set, while still loading the key for backward compatibility.
- **Redact sensitive headers in DEBUG request logs.** The `debug!` line that logs the request to be forwarded now masks the values of `Authorization`, `Cookie`, and `Proxy-Authorization` with a `<redacted>` placeholder (header names stay visible). For troubleshooting, redaction can be disabled by setting the environment variable `RPXY_UNSAFE_DEBUG_HEADERS` to `1`, `true`, or `yes`; the variable is read once at startup and emits a `warn!` when enabled. Do not leave it enabled in production. The unredacted values still only appear when `RUST_LOG=debug`.
- **Fix: preserve the case of the sticky cookie `path` attribute.** The sticky-session `Set-Cookie` previously lowercased its `path`, which could mis-scope the cookie and silently break stickiness on case-sensitive route paths. The `path` is now emitted verbatim (the cookie `domain` is still lowercased). Because the path is bound into the sealed token, sticky cookies issued for a mixed-case path before the upgrade are ignored once and reissued; all-lowercase paths are unaffected.
- **Validate `server_name` as a hostname.** Each app's `server_name` is now validated at startup and must be a syntactically valid hostname: dot-separated labels of 1-63 characters, each starting and ending with an alphanumeric and otherwise containing only alphanumerics and `-`, with a total length up to 253 ASCII characters. This is defense-in-depth, in particular for the ACME on-disk paths derived from `server_name`. Valid hostnames are unaffected, but a `server_name` that is not a valid hostname (containing path separators, `..`, wildcards `*`, underscores `_`, IPv6 literals, or non-ASCII characters) is now rejected at startup where it was previously accepted (IPv4 literals are still accepted).
- **Add optional per-IP connection limit.** A new global `max_clients_per_ip` option caps the number of concurrent connections from a single source IP, in addition to the existing global `max_clients`, so one source cannot exhaust the connection pool. It defaults to `0` (disabled), preserving existing behavior. The source IP is the immediate TCP/QUIC peer, or the real client address recovered from an inbound PROXY protocol header; it is not derived from `X-Forwarded-For` / `Forwarded`, so the limit is only meaningful when rpxy is the edge or inbound PROXY protocol is enabled (behind a bare L7 load balancer every connection collapses to the balancer's IP). For HTTP/1.1 and HTTP/2 the slot is reserved before the TLS handshake so handshake floods are bounded too; for HTTP/3 it caps QUIC connections per source IP, and a single IP's concurrent HTTP/3 request streams are then bounded by `max_clients_per_ip` times `[experimental.h3] max_concurrent_bidistream`.
- **Structured audit logging for TLS / mTLS handshake failures.** TLS handshake failures, including mTLS client-certificate verification failures, are now logged as structured records carrying the source IP, the SNI, a stable failure category, and (for negotiation failures) whether the vhost enforces mutual TLS. The category is one of `client_cert` (a missing or invalid client certificate — the mTLS authentication failure case, determined from the rustls error, not from received TLS alerts), `handshake`, `acceptor`, `no_sni`, `unknown_sni`, `acme_no_config`, or `timeout`. `client_cert` and `handshake` failures are logged at `warn!`; routine misdirected/scanner cases (`no_sni`, `unknown_sni`, `acme_no_config`) at `info!`. Previously these were logged without the source IP or SNI, and an mTLS verification failure was misreported under a "Failed to build TLS acceptor" message.
- **Retain the last known-good certificate when a hot-reload read fails.** During certificate hot-reload, if a configured `server_name`'s certificate or key temporarily fails to read (for example, the file is missing or being rewritten at the moment of reload), rpxy now keeps serving that domain's previously loaded certificate instead of dropping the domain from the active SNI map. Previously a single transient read error took that domain's TLS offline until the next reload cycle that happened to read it successfully. The retained-certificate case is logged at `warn!`, and the never-loaded case (a `server_name` that has not yet loaded successfully since startup, where there is nothing to retain) remains a hard `error!`; both logs now include the target `server_name`. A domain whose files stay invalid therefore keeps serving its last-good certificate until the process restarts or a later reload succeeds.

### Improvement

- Document that `connection_handling_timeout = 0` (the default) means no forced timeout, and recommend a non-zero value in production unless long-lived connections (e.g. WebSocket) are required.
- Document the HTTP/3 `request_max_body_size` default of 256 MiB and recommend setting a lower explicit value in production when large uploads are not required.
- Add an optional global `redact_query_in_access_log` setting. When enabled, query-string values in the access log (both the request path+query and the upstream URL) are masked as `<redacted>` while the parameter keys and the path are kept, so URLs that carry tokens or PII (e.g. `?token=...`, `?email=...`) are not logged verbatim. It defaults to `false`, preserving the current full-query access-log behavior. Redaction is applied when the access-log record is built, so the record itself does not store the raw query values (the underlying request/upstream `http::Uri` still holds them in memory during request handling).
- deps and refactor

## 0.11.3

### Improvement

- Feat: Support `tcp` and `http` active health checks. This is to support the use case where rpxy needs to monitor the health of backend applications and avoid sending requests to unhealthy ones. To enable this feature, the `health-check` feature has to be enabled and the `health_check` option in the config file has to be specified for each reverse proxy backend group.

- Deps and refactor

## 0.11.2

### Improvement

- Feat: Support implementation of multiple address-binding: This is to support the use case where rpxy is used in a host with multiple network interfaces and needs to bind to multiple ones. Both `listen_address_v4` and `listen_address_v6` options in the config file accepts either a single address or a list of addresses.

- Deps and refactor

## 0.11.1

### Improvement

- Feat: Support specific listener address binding for both IPv4 and IPv6. This is to support the use case where rpxy is used in a host with multiple network interfaces and needs to bind to a specific one. To enable this feature, the `listen_address_v4` and `listen_address_v6` options in the config file have to be specified. If `listen_address_v6` is not specified and `listen_ipv6` is true, it binds to `::`. If `listen_address_v6` is not specified and `listen_ipv6` is false or undefined, IPv6 is disabled. (#239)

- Deps and refactor

## 0.11.0

### Improvement

- Feat: Support PROXY protocol for incoming TCP connections, i.e., HTTP/1.1 and HTTP/2. This is to support the use case where rpxy is used behind another load balancer or reverse proxy that supports PROXY protocol, e.g., rpxy-l4, AWS ELB, HAProxy, Nginx, etc. To enable this feature, the `proxy-protocol` feature has to be enabled and the `experimental.tcp_recv_proxy_protocol` option in the config file has to be specified. Note that this feature is only for incoming connections and does not affect outgoing connections towards backend applications. Also note that HTTP/3 (QUIC) is not supported for PROXY protocol since its underlying UDP is connectionless and does not fit the layer-4 connection-oriented nature of PROXY protocol.

- Deps and refactor

### Bugfix

- Fix: TLS listener hot-reload fix: Changed break to continue when certificate reload fails, allowing the listener to wait for ACME to provision certificates instead of stopping entirely (#454)
- Fix: Write permission preflight check: Added startup verification for ACME certificate directories to fail fast with clear error messages, preventing silent failures that waste ACME rate limits (#454)

## 0.10.4

### Improvement

- Deps and refactor

### Bugfix

- Fix: RFC compliance issue for the URL path string (#425)

## 0.10.3

### Improvement

- Feat: Update the reloading strategy for config toml from polling to realtime.
- Deps

### Bugfix

- Fix: Fix the bug that when only https_port is specified, rpxy does not start properly.

## 0.10.2

### Bugfix

- Fix: Fix the bug that the `forwarded_header` option does not work properly (`proto` param)

## 0.10.1

### Improvement

- Feat: Support `Forwarded` header in addition to `X-Forwarded-For` header. This is to support the standard forwarding header for reverse proxy applications (RFC 7239). Use the `forwarded_header` upstream option to enable this feature.
  By default, it is not appended to the outgoing header. However, if the incoming request has the forwarded header, it would be preserved and updated simultaneously with `x-forwarded-for` header. if both forwarded and x-forwarded-for headers exists (and they are inconsistent), x-forwarded-for is prioritized. This means that x-forwarded-for is first updated and it is then copied (overridden) to `for` param of forwarded header.
- Refactor: lots of minor improvements
- Deps

## 0.10.0

### Important Changes

- [Breaking] We removed non-`watch` execute option and enabled the dynamic reloading of the config file by default.
- We newly added `log-dir` execute option to specify the directory for `access.log`,`error.log` and `rpxy.log`. This is optional, and if not specified, the logs are written to the standard output by default.

### Improvement

- Refactor: lots of minor improvements
- Deps

## 0.9.7

### Improvement

- Feat: add version tag for docker images via github actions
- Feat: support gRPC: This makes rpxy to serve gRPC requests on the same port as HTTP and HTTPS, i.e., listen_port and listen_port_tls. This means that by using the different subdomain for HTTP(S) and gRPC, we can multiplex them on same ports without opening another port dedicated to gRPC. To this end, this update made the forwarder to force HTTP/2 for gRPC requests towards backend (gRPC) app.
- Deps and refactor

### Bugfix

- Fixed bug for the upstream option "force_http2_upstream"

### Other

- Tentative downgrade of github actions `runs-on` from ubuntu-latest to ubuntu-22.04.

## 0.9.6

### Improvement

- Feat: Change the default hashing algorithm for internal hashmaps and hashsets from FxHash to aHash. This change is to improve the security against HashDos attacks for colliding domain names and paths, and to improve the speed of hash operations for string keys (c.f., [the performance comparison](https://github.com/tkaitchuck/aHash/blob/master/compare/readme.md)).
- Deps and refactor

## 0.9.5

### Bugfix

- Fix docker image build options with `post-quantum` feature.

## 0.9.4

### Improvement

- Feat: Enable the hybrid post-quantum key exchange for TLS and QUIC with `X25519MLKEM768` by default.
- Deps and refactor

## 0.9.3

### Improvement

- Feat: Support post-quantum `X25519Kyber768Draft00` for incoming and outgoing TLS initiation. This is non-default feature [feature: `post-quantum`].
- Feat: emit WARN messages if there exist unused and unsupported options specified in configuration file.
- Docs: `rpxy.io` is now available for the official website of `rpxy`.
- Refactor: lots of minor improvements
- Deps

## 0.9.2

### Improvement

- Feat: Add Jenkins build pipeline (#182)
- Refactor: lots of minor improvements
- BugFix: Fix the bug related to the installation of `CryptoProvider` (#194)
- BugFix: h3 header to use https_redirection_port (#192)
- Deps

## 0.9.1

### Important Changes

- Feat: Support `https_redirection_port` option to redirect http requests to https with custom port.

### Improvement

- Refactor: lots of minor improvements
- Deps

## 0.9.0

### Important Changes

- Breaking: Experimental ACME support is added. Check the new configuration options and README.md for ACME support. Note that it is still under development and may have some issues.

### Improvement

- Refactor: lots of minor improvements
- Deps

### Bugfix

- Fix the bug that the dynamic config reload does not work properly.

## 0.8.1

### Improvement

- Refactor: lots of minor improvements
- Deps

## 0.8.0

### Important Changes

- Breaking: Support for `rustls`-0.23.x for http/1.1, 2 and 3. No configuration update is needed at this point.
- Breaking: Along with `rustls`, the cert manager was split from `rpxy-lib` and moved to a new inner crate `rpxy-cert`. This change is to make the cert manager reusable for other projects and to support not only static file based certificates but also other types, e.g., dynamic fetching and management via ACME, in the future.

### Improvement

- Refactor: lots of minor improvements
- Change the certificate verifier from `rustls-native-certs` to `rustls-platform-verifier` to use the system's default root cert store for better client (forwarder) performance in `hyper-rustls`.

## 0.7.1

- deps and patches

## 0.7.0

### Important Changes

- Breaking: `hyper`-1.0 for both server and client modules.
- Breaking: Remove `override_host` option in upstream options. Add a reverse option, i.e., `keep_original_host`, and the similar option `set_upstream_host`. While `keep_original_host` can be explicitly specified, `rpxy` keeps the original `host` given by the incoming request by default. Then, the original `host` header is maintained or added from the value of url request line. If `host` header needs to be overridden with the upstream host name (backend uri's host name), `set_upstream_host` has to be set. If both of `set_upstream_host` and `keep_original_host` are set, `keep_original_host` is prioritized since it is explicitly specified.
- Breaking: Introduced `native-tls-backend` feature to use the native TLS engine to access backend applications.
- Breaking: Changed the policy of the default cert store from `webpki` to the system-native store. Thus we terminated the feature `native-roots` and introduced `webpki-roots` feature to use `webpki` root cert store.

### Improvement

- Redesigned: Cache structure is totally redesigned with more memory-efficient way to read from cache file, and more secure way to strongly bind memory-objects with files with hash values.
- Redesigned: HTTP body handling flow is also redesigned with more memory-and-time efficient techniques without putting the whole objects on memory by using `futures::stream::Stream` and `futures::channel::mpsc`
- Feat: Allow to disable/enable forced-connection-timeout regardless of connection status (idle or not). [default: disabled]
- Refactor: lots of minor improvements

## 0.6.2

### Improvement

- Feat: Add a build feature of `native-roots` to use the system's default root cert store.
- Feat: Add binary release in addition to container release
- Refactor: lots of minor improvements

## 0.6.1

### Bugfix

- Fix: fix a "watch" bug for docker. Due to a docker limitation, we need to mount a dir, e.g, `/rpxy/config`, instead of a file, `rpxy.toml`, to track changes of the configuration file. We thus updated a start up script in docker container for the case "WATCH=true".

## 0.6.0

### Improvement

- Feat: Enabled `h2c` (HTTP/2 cleartext) requests to upstream app servers (in the previous versions, only HTTP/1.1 is allowed for cleartext requests)
- Feat: Initial implementation of caching feature using file + on memory cache. (Caveats: No persistance of the cache. Once config is updated, the cache is totally eliminated.)
- Refactor: lots of minor improvements

### Bugfix

- Fix: fix `server` in the response header (`rpxy_lib` -> `rpxy`)
- Fix: fix bug for hot-reloading configuration file (Add termination notification receiver in proxy services)

## 0.5.0

### Improvement

- Feat: `s2n-quic` with `s2n-quic-h3` is supported as QUIC and HTTP/3 library in addition to `quinn` with `h3-quinn`, related to #57.
- Feat: Publish dockerfile for `rpxy` with `s2n-quic` on both `amd64` and `arm64`.
- Feat: Start to publish docker images on `ghcr.io`
- Refactor: logs of minor improvements

## 0.4.0

### Improvement

- Feat: Continuous watching on a specified config file and hot-reloading the file when updated
- Feat: Enabled to specify TCP listen backlog in the config file
- Feat: Add a GitHub action to build `arm64` docker image.
- Bench: Add benchmark result on `amd64` architecture.
- Refactor: Split `rpxy` into `rpxy-lib` and `rpxy-bin`
- Refactor: lots of minor improvements

### Bugfix

- Fix bug to apply default backend application

## 0.3.0

### Improvement

- HTTP/3 Deps: Update `h3` with `quinn-0.10` or higher. But changed their crates from `crates.io` to `git submodule` as a part of work around. I think this will be back to `crates.io` in a near-future update.
- Load Balancing: Implement the session persistance function for load balancing using sticky cookie (initial implementation). Enabled in `default-features`.
- Docker UID:GID: Update `Dockerfile`s to allow arbitrary UID and GID (non-root users) for rpxy. Now they can be set as you like by specifying through env vars.
- Refactor: Various minor improvements

## 0.2.0

### Improvement

- Update docker of `nightly` built from `develop` branch along with `amd64-slim` and `amd64` images with `latest` and `latest:slim` tags built from `main` branch. `nightly` image is based on `amd64`.
- Update `h3` with `quinn-0.10` or higher.
- Implement path replacing option for each reverse proxy backend group.

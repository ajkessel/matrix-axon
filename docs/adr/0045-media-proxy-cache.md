# ADR 0045 — Media proxy: bounded LRU disk cache, range requests, deletion purge

## Context

Milestone 11 completes the media proxy. The request path was already wired
(`GET /v1/media/{account_id}/{server_name}/{media_id}` → an event lookup that
finds the encrypted-file descriptor → the SDK downloads and decrypts the MXC
content), but it buffered the whole object into a `Vec<u8>`, served a hardcoded
`Content-Type: application/octet-stream`, honored no `Range` requests, and cached
nothing. The milestone requires a **bounded LRU cache on local disk** (default
5 GiB), **proper caching headers and range-request support**, and — per ADR 0024
step 5 — a **media-cache purge on account deletion**. It explicitly forbids an S3
backend: the homeserver is the durable source of truth (off-host storage is solved
at the homeserver layer, e.g. `synapse-s3-storage-provider`), so axon's cache is a
bounded convenience only.

## Decision

### The cache owns single-flight + eviction; the downloader is an injected trait

The bounded cache lives in `axon-media`, which depends only on `axon-core` and
**must stay free of `matrix-sdk`**. The authenticated, decrypting download stays in
`axon-sync::SdkMediaProxy`. They meet at an SDK-free `MediaFetcher` trait
(`async fn fetch(account_id, mxc_url, encrypted_file) -> Result<Vec<u8>,
FetchError>`) that the cache calls on a miss. `axon-server` composes a
`CachingMediaProxy { cache, fetcher }` and adapts it onto the existing `axon-api`
`MediaProxy` port, so `axon-api`, `axon-media`, and `axon-sync` never depend on one
another. The cache — not a composition-root wrapper — owns `get_or_fetch`, because
correct single-flight requires the cache to *drive* the fetch (a passive
get/insert cache lets concurrent misses each download). Single-flight uses a
per-key `Arc<Mutex<HashMap<Key, Arc<AsyncMutex<()>>>>>` slot, the `ClientManager`
pattern, with the slot pruned once no waiter holds it.

### On-disk layout and the LRU index

Objects live at `cache_dir/<account_id>/<sha256_hex(mxc_url)>`. The per-account
subdirectory makes a per-account purge and the boot orphan-GC a directory removal,
mirroring `sync.data_dir/<account_id>/`. An in-memory index behind one
`std::sync::Mutex` tracks `{size, seq}` per key, a `BTreeMap<seq, key>` LRU order,
and a running `total_bytes`; the lock is **never held across an `.await`** (pick
victims + update bookkeeping under the lock, drop it, then unlink). On `open()` the
index is rebuilt by scanning the cache dir (ordering by mtime, oldest → lowest
seq), and the on-disk set is evicted down to the cap if it already exceeds it.
A stale `.tmp` staging dir is cleared each boot; non-UUID entries and stray files
are left strictly untouched.

### Serve via an open fd (unlink-after-open)

The `MediaProxy` port returns a `MediaResource { file: tokio::fs::File, len, etag
}` — the cache opens the file and hands back the handle. On Linux an open fd
survives `unlink`, so a concurrent eviction or account-purge that deletes the path
cannot break an in-flight serve; no refcounting is needed. (On Windows an open file
can't be unlinked; the deployment target is Linux, and this is the one caveat.)

### Range + conditional GET, hand-rolled

The handler parses a single-range `Range: bytes=…` (multi-range and malformed
headers fall back to a full `200` per RFC 7233; a well-formed range outside the
content is `416` with `Content-Range: bytes */len`), `seek`s the fd, and streams
`Content-Length` bytes via `tokio_util::io::ReaderStream`. It emits `Accept-Ranges:
bytes`, an `ETag` (the sha256, since MXC content is immutable), and answers `304`
when `If-None-Match` matches. `tower-http`'s `ServeFile` was rejected: it is not in
the dependency graph and wants a filesystem *path*, which reopens the eviction
race the open-fd design closes.

### Content-Type from the event, sanitized

The SDK does not surface a `Content-Type`, so the handler derives it from the
event content (the authoritative `info.mimetype` for a primary attachment,
`info.thumbnail_info.mimetype` for a thumbnail — matched the same way the encrypted
descriptor is), falling back to `application/octet-stream`. The value is
sender-controlled, so it is sanitized to a bare `type/subtype` token (no
parameters, bounded length, token charset only) before being echoed into a header,
defeating header injection. The cache itself stays content-type-agnostic (bytes
only). (Magic-byte sniffing via `infer` is possible future work; not in M11.)

### Resource bounds and never-fatal-to-serving

- **Per-object cap** (`max_object_bytes`, default 100 MiB): an object larger than
  the cap is refused rather than cached or served, so one object cannot blow the
  total cap or memory. (The SDK still buffers the full object before the cache sees
  it — true streaming download is an SDK limitation; the cap bounds it
  post-download.) This replaces the temporary 50 MiB in-memory guard.
- **Fetch timeout** (`fetch_timeout_secs`, default 60): the upstream download is
  wrapped in `tokio::time::timeout` so a hung homeserver can't await unbounded
  (the AGENTS outbound-call rule).
- **Cache-write failure degrades, never fails the request:** bytes are staged into
  a temp file that is opened *before* any rename; on a promote/index failure the
  handler serves from that fd (unlinked after open) and the object simply isn't
  retained. A genuine local disk failure (can't stage anywhere) is the only
  `MediaError::Internal` (`500`).
- **Kill-switch** (`media.enabled`, default true): when false the cache retains
  nothing — each request fetches to a short-lived temp file, serves it (range
  support intact), and deletes it.

### Account-deletion purge + boot orphan-GC (ADR 0024 step 5)

A `MediaCacheHandle` (cheap clone, mirroring `axon-search::IndexHandle`) is
threaded through `SyncEngine::start` into `AccountLifecycle`. `delete` calls
`purge_account` between the SDK-store-dir removal and the row delete: it drops the
account's LRU entries and `remove_dir_all`s its directory. Handle-threading beats
purging by path from config because it also keeps the *running* index consistent
(a path purge would leave stale entries counting against the cap). `open()` +
`prune_orphan_media_dirs` (the M11 analogue of `prune_orphan_store_dirs`, keyed off
account-row existence, never lifecycle state) is the boot backstop for a purge
interrupted by a crash or performed while the cache was disabled.

`purge_account` also fences the cache against a fetch that was already in flight
when deletion started: it and the cache-promotion step in `store_and_open` take
the same per-account lock and share a `purged` tombstone set, so whichever side
wins the race fully finishes before the other runs, and a promotion that loses
always sees the tombstone and refuses to recreate the just-purged directory. That
in-flight fetch still degrades to serving its bytes uncached (never fatal to
serving) rather than failing the request.

## Consequences

- **Pro:** repeat fetches are served from disk with range + conditional-GET
  support, so `axon-tui` (and any client) can render and seek media efficiently;
  the cache is bounded and self-healing across restarts.
- **Pro:** `axon-media` stays a matrix-free leaf, so the cache is unit-tested with
  a mock fetcher (hit/miss, single-flight, eviction, boot rebuild, per-object cap,
  purge, orphan-GC, unlink-after-open) with no DB or homeserver.
- **Con / accepted:** the SDK buffers each object fully before caching, so peak
  memory during a download is one object (bounded by `max_object_bytes`), not a
  streamed constant. Acceptable for the media sizes in scope; revisit if the SDK
  gains streaming download.
- **Con / accepted:** the port's `MediaError::TooLarge` maps to `413`, distinct
  from the other upstream-failure statuses (`502`/`503`); the message is
  explicit about the configured cap.
- **Scope:** no S3 / off-host backend, by decision — the homeserver is the source
  of truth and the cache is a bounded LRU in front of it.

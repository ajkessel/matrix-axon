# axon-media

Media proxy with bounded LRU disk cache for MXC URLs.

## Responsibility

Resolves `mxc://` URIs against the upstream homeserver for the relevant account, caches responses to a local disk LRU cache (configurable size, default 5 GB), and serves them at `GET /v1/media/{account_id}/{server}/{media_id}` with proper caching headers and range-request support.

## Owns vs. consumes

- **Owns:** the media cache directory on disk.
- **Consumes:** `axon-core` config; upstream homeserver HTTP.

## Design

- **SDK-free leaf.** This crate depends only on `axon-core`. The authenticated,
  decrypting download lives in `axon-sync` behind the `MediaFetcher` trait the
  cache calls on a miss; `axon-server` composes the cache in front of the fetcher
  and adapts the pair onto the `axon-api` `MediaProxy` port.
- **Bounded LRU on disk.** Objects live at `cache_dir/<account_id>/<sha256(mxc)>`
  under a global byte cap; an in-memory index (rebuilt from disk on boot) drives
  LRU eviction. A per-object cap refuses oversized media.
- **Serve via an open fd.** `get_or_fetch` returns an open `tokio::fs::File`; on
  Linux the fd survives `unlink`, so eviction/purge never breaks an in-flight
  serve. The `axon-api` handler streams ranges from it.
- **Account-deletion purge + boot orphan-GC** for `cache_dir/<account_id>/`
  (ADR 0024 step 5).

See ADR 0045 for the full rationale.

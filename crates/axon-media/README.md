# axon-media

Media proxy with bounded LRU disk cache for MXC URLs.

## Responsibility

Resolves `mxc://` URIs against the upstream homeserver for the relevant account, caches responses to a local disk LRU cache (configurable size, default 5 GB), and serves them at `GET /v1/media/{account_id}/{server}/{media_id}` with proper caching headers and range-request support.

## Owns vs. consumes

- **Owns:** the media cache directory on disk.
- **Consumes:** `axon-core` config; upstream homeserver HTTP.

## Status

Stub — no public API yet.

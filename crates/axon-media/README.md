# axon-media

Media proxy with bounded LRU disk cache for MXC URLs.

## Responsibility

Resolves `mxc://` URIs against the upstream homeserver for the relevant account, caches responses to a local disk LRU cache (configurable size, default 5 GB), and serves them at `GET /v1/media/{account_id}/{server}/{media_id}` with proper caching headers and range-request support.

## Owns vs. consumes

- **Owns:** the media cache directory on disk.
- **Consumes:** `axon-core` config; upstream homeserver HTTP.

## Status

Preparatory route/decryption support is implemented by PR 70. The bounded
on-disk LRU cache, range requests, cache headers, and resource bounds required
for the complete Milestone 11 contract are tracked separately in #97.

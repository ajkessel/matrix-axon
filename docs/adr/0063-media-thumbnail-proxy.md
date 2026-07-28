# ADR 0063 — Media thumbnail proxy

## Context

M11's media proxy (ADR 0045, `GET /v1/media/{account_id}/{server_name}/{media_id}`)
only ever proxies the full-resolution original of an `mxc://` object, even
though `matrix-sdk` (already a workspace dependency, 0.18, used via
`axon-sync`) exposes the homeserver's native thumbnail endpoint. Every inline
preview — currently the TUI's own client-side downscale in
`clients/tui/src/app/media.rs`, and in the future any web client — has to
download the full original and shrink it after the fact: wasted bandwidth,
decode time, and memory, worse on a mobile link.

Issue #253 (filed against this gap) cites two blockers that turn out not to
apply: an "ADR 0064" that does not exist anywhere in this repo's history, and
ADR 0059's deferral of "thumbnail generation," which is about *server-side
resizing of a client's own uploads at send time* (M15, still design-only) —
a different concern from proxying the homeserver's *existing* thumbnail
endpoint for already-received media. ADR 0046's web-client roadmap also notes
"no server thumbnails," but only because the web client didn't exist yet to
consume one — it still doesn't (`clients/web/` is absent from every local and
remote branch as of this writing). Neither actually blocks this work; it's
additive `crates/`-silo work with no client silo touched.

## Decision

### New sibling route, not a query-param variant of the existing one

`GET /v1/media/{account_id}/{server_name}/{media_id}/thumbnail?width=&height=&method=`,
registered alongside (not replacing) the existing full-media route in
`crates/axon-api/src/lib.rs`, inside the same bearer-gated `authed` block.
This mirrors the real Matrix C-S thumbnail endpoint's own path shape and
leaves `get_media`/its route entry byte-for-byte unchanged — zero regression
risk to the M11 proxy. `width`/`height` are required, clamped (not rejected)
into `[16, 1600]` px — the same "clamp, don't 400" convention the pagination
`limit` uses in `rooms.rs`/`search.rs` — then snapped *up* to the nearest of a
small set of standard sizes (`32, 96, 320, 640, 800, 1600`,
`snap_thumbnail_dimension`), never down, so the served thumbnail is never
smaller than requested. Snapping serves two purposes: it bounds how many
distinct `(width, height, method)` cache entries a single `mxc_url` can
accumulate (see the cache-key section below), and it mirrors the small preset
list a homeserver running with on-demand thumbnailing disabled (e.g.
Synapse's default `dynamic_thumbnails: false`) actually serves, so a request
is more likely to match a size the homeserver has pre-generated instead of
failing against every non-preset size. `method` (`crop`/`scale`) is optional,
defaulting to `scale` per the Matrix spec default.

### Encrypted media is a hard `400`, not a v1 corner-cut

`matrix-sdk` 0.18.0's `Media::get_media_content` only honors
`MediaFormat::Thumbnail` when `request.source` is `MediaSource::Plain` — for
`MediaSource::Encrypted` it always downloads and decrypts the full ciphertext
regardless of `format` (confirmed by reading the vendored SDK source,
`matrix-sdk-0.18.0/src/media.rs`, `get_media_content`'s match on
`request.source`). This isn't a version quirk: a homeserver never sees
encrypted-media plaintext, so it cannot generate a thumbnail of it — the
Matrix spec's own thumbnail endpoint has no encrypted-media story, by design.
The new route rejects any `mxc_url` whose owning event has a matching
`content.file`/`content.info.thumbnail_file` descriptor with `400` *before*
calling the proxy at all, reusing the existing `encrypted_file_for_mxc`
helper from the full-media handler. Consequently the new
`MediaProxy::get_thumbnail`/`MediaFetcher::fetch_thumbnail` methods take no
`encrypted_file` parameter at all — by construction, only plain media ever
reaches them. This will not be revisited unless the Matrix spec itself
changes; encrypted rooms keep using the existing full-media route plus
whatever sender-embedded `thumbnail_file` the sending client chose to attach.

### `Media::get_thumbnail()` is the wrong SDK method; `get_media_content` with `MediaFormat::Thumbnail` is right

`Media::get_thumbnail()` requires a typed `impl MediaEventContent` (e.g.
`ImageMessageEventContent`), which nothing in this call path has — `axon-api`
only ever passes a raw `mxc_url` string down to `axon-sync`. The existing
`SdkMediaProxy::download()` already calls `client.media().get_media_content(&request, false)`
directly; the new `download_thumbnail()` follows the identical shape, just
with `request.format = MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(...))`
and `request.source` forced to `MediaSource::Plain` (never `Encrypted`, per
the above). The issue's cited `MediaThumbnailSize` type does not exist in
0.18.0 either — the real type is `matrix_sdk::media::MediaThumbnailSettings`.

### Cache-key extension without touching the LRU/eviction machinery

`axon-media`'s cache key was `sha256(mxc_url)` with no size dimension. A
thumbnail at a given `(width, height, method)` is a genuinely different byte
payload than the original for the same `mxc_url`, so a new
`etag_for_thumbnail(mxc_url, spec)` hashes a distinctly-namespaced preimage
(`"thumb:{mxc_url}:{w}x{h}:{method}"`) — this can never collide with the bare
`sha256(mxc_url)` `etag_for` computes for the original, even though both live
as sibling files in the same per-account cache directory. `Index`, eviction,
purge, and `MediaCacheStats` all operate on opaque `(Uuid, String)` keys
already and needed zero changes. The single-flight/promote/evict body shared
by the plain and thumbnail paths was factored into a private
`MediaCache::get_or_fetch_keyed(account_id, hash, fetch_fn)` rather than
duplicated.

**Bounded, not unlimited**: because the cache key includes the resolved
`(width, height, method)`, a client can still multiply cache entries per
`mxc_url` by varying the request, but the dimension-snapping above caps this
to at most `6 buckets × 6 buckets × 2 methods = 72` distinct entries per
`mxc_url` (down from an effectively unbounded range), on top of the
pre-existing global LRU byte cap (`max_bytes`), which bounds worst-case disk
use regardless.

### Content-Type is corrected by sniffing the thumbnail bytes

The handler starts from `content_type_for_mxc` (derived from the event's
`info.mimetype`, unchanged from the full-media route), but for the thumbnail
route specifically, corrects it by sniffing the actual returned bytes' magic
number (`sniff_image_mime`/`correct_thumbnail_content_type`) — because a
homeserver-generated thumbnail may be re-encoded to a different format than
the original (e.g. a PNG source thumbnailed to JPEG by Synapse), and with
`X-Content-Type-Options: nosniff` set on every response, a declared-but-wrong
`Content-Type` would make a strict/browser consumer refuse to render bytes
that are in fact a valid, different image format. Sniffing only recognizes
the same small set of image formats already in `is_inline_safe`'s allowlist,
so a positive match is always safe to serve with its own type; an
unrecognized signature falls back to the declared type unchanged (the
original's `content_type_for_mxc` behavior). This sniffing is thumbnail-route
only — the full-media route is never re-encoded, so `get_media` keeps trusting
the declared type as before.

### Shared types live in `axon-core`

`ThumbnailMethod` (`Crop`/`Scale`) and `ThumbnailSpec` (`{width, height, method}`)
live in a new `axon_core::media` module — needed identically by `axon-api`
(the `MediaProxy` port signature) and `axon-media` (the cache-key input),
both of which already depend on `axon-core`, and neither of which depends on
the other. A separate `ThumbnailMethodDto` stays in `axon-api::dto` for the
wire/OpenAPI shape (`#[serde(rename_all = "lowercase")]`), converted to the
core enum at the handler boundary — the same split `MediaUploadKindDto`
already established for M15's staged-upload query.

## Consequences

- New route, fully additive: `get_media`'s existing behavior, route entry,
  and tests are untouched.
- `MediaProxy` (axon-api), `MediaFetcher` (axon-media), and `SdkMediaProxy`
  (axon-sync) each gain one new method (`get_thumbnail`/`fetch_thumbnail`)
  plus the etag-computation sibling — every existing implementor
  (`CachingMediaProxy`, and the test-only `StubMediaProxy`/
  `ConfiguredMediaProxy`) needed a matching addition, the same mechanical
  cost every port extension in this repo incurs.
- Encrypted media is permanently out of scope for server-side thumbnailing —
  not a gap to close later, but an architectural fact about what a
  homeserver can see. Clients rendering inline previews in encrypted rooms
  keep relying on sender-embedded `thumbnail_file` (if the sending client
  attached one) or the full-media proxy.
- Milestone: **M17**, single PR, server-only (`crates/` silo). No `clients/`
  changes: `clients/web/` still doesn't exist, and the TUI's existing
  client-side downscale (`clients/tui/src/app/media.rs`) has no dependency on
  this route and is untouched. A client consuming this endpoint for cheaper
  inline previews is separate follow-on client work, the same
  backend/client split ADR 0060 (device-list) and 7a-6/7c drew for their own
  server-only milestones.

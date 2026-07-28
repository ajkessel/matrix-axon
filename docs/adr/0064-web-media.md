# ADR 0064 — Web client media rendering (M-W8)

## Context

The web client cannot show you a picture. An `m.image` event reaches
`EventBody` — the app's only `msgtype` switch, in `RoomPage.tsx` — matches
neither `m.emote` nor `m.notice`, and falls through to `FormattedBody`, which
renders its plain-text `body`: usually a bare filename. `m.file`, `m.video`,
and `m.audio` do the same. An `<img>` inside a `formatted_body` is stripped by
the sanitizer, which lifts the `alt` text into a text node as a stand-in
(`src/html/sanitize.ts`, with a comment promising this milestone).

ADR 0046's roadmap sets the scope:

> **M-W8** — Media: MediaService (fetch → blob → object URL, LRU + revoke),
> lazy-load via IntersectionObserver with a concurrency cap (no server
> thumbnails), full-size lightbox. **Exit criterion:** encrypted attachment
> from the integration harness renders inline.

The server needs no changes. `GET /v1/media/{account_id}/{server_name}/{media_id}`
(M11, ADR 0045) already proxies `mxc://` URIs: it reassembles the URI from the
path segments, resolves the owning event to discover encryption and mimetype,
downloads through the account's SDK client, **decrypts encrypted attachments
server-side**, sanitizes the `Content-Type` against an inline-safe allowlist
(non-SVG `image/*`, `audio/*`, `video/*`; everything else becomes
`application/octet-stream` with `nosniff`), and serves the bytes from a bounded
on-disk LRU with ETag and Range support.

The single fact that shapes this entire design is that the route sits behind the
same bearer guard as every other `/v1` route (ADR 0029), and **a browser cannot
put an `Authorization` header on `<img src>`**. Every byte must therefore arrive
through `fetch()` → `Blob` → `URL.createObjectURL()`. ADR 0046's open question 2
raised a service worker injecting the header as the alternative — that would
allow native `<img>` URLs and streaming — and rejected it for now on lifecycle
complexity and Tauri WebView uncertainty. M-W12 hardens that into a rule: no
service workers, ever. So fetch-and-blob it is, and the memory pressure that
buys us has to be managed explicitly. That is most of what this ADR decides.

Media *upload* is not in scope. ADR 0059 (`m.image`/`m.file` send via staged
uploads) is Proposed with no server code, and ADR 0046 lists "media upload (no
API)" as out of scope for the whole web roadmap. M-W8 is read-only media.

## Decision

### Inline images and stickers; attachment cards for everything else

`m.image` and `m.sticker` render inline, with a click-to-open full-size
lightbox. `m.file`, `m.video`, and `m.audio` render an attachment card — icon,
filename, human-readable size, download button — and fetch no bytes until the
user asks for them.

This beats TUI parity, which renders pictures for `m.image`/`m.sticker` and a
bare `[file: name]` label for the rest (`clients/tui/src/api.rs`,
`media_label`). It stops short of native `<video>`/`<audio>` elements: a blob
URL means buffering the entire file in memory before the first frame plays,
which is the exact pressure ADR 0046's open question 2 flagged. A player is a
cheap follow-up once a streaming transport exists (a `?token=` query parameter,
or the service worker if Tauri ever permits one). Until then, download is
honest about what it costs.

### `MediaService`: refcounted cache, LRU over the unreferenced remainder

`src/media/media-service.ts`, a plain factory with no Preact dependency, wired
into the service graph in `src/services.ts` and reached through `useServices()`.

```ts
export interface MediaService {
  /** Resolve mxc → object URL, refcount-acquired. The caller MUST release(). */
  acquire(accountId: string, mxcUrl: string): Promise<MediaHandle>
  /** One-shot download: a blob URL the caller owns and revokes. */
  fetchBlobUrl(accountId: string, mxcUrl: string): Promise<MediaResult>
}
```

**The hazard this design exists to prevent** is a cache evicting — and thus
revoking — an object URL that a still-mounted `<img>` points at, which paints a
broken image with no error anyone can catch. ADR 0046 said "LRU + revoke" and
left the interaction open. It cannot be resolved by sizing: the timeline is not
windowed (`RoomPage` maps every loaded event into one `<ol>`, unlike the room
list, which does window), so the set of simultaneously mounted images grows with
every scroll-back page. There is no cache size that is safely "above the working
set" when the working set is *everything in the DOM*.

So the cache **refcounts**, and the LRU is demoted to a memory ceiling over the
remainder:

- `acquire()` increments a refcount; the component's effect cleanup calls
  `release()`. An entry with `refs > 0` is **never** revoked. Correctness is a
  property of the refcount, not of any capacity number.
- When an entry's refcount reaches zero it becomes evictable and joins the LRU
  tail; re-acquiring pulls it back out. Only when the count of zero-ref entries
  exceeds the cap (**32**, roughly two pages of scrolled-away images) is the
  oldest evicted and `URL.revokeObjectURL` called on it. Scrolling back up
  through recent history therefore repaints instantly.

Concurrent `acquire()`s for one key share a single in-flight promise (a dedupe
map keyed by `accountId + '\0' + mxc`), so a re-render storm cannot multiply
requests. A **FIFO semaphore of 6** bounds concurrent fetches — the browser's
classic per-host connection budget, against what is a single bearer-guarded
origin. The dedupe map sits outside the semaphore, so duplicates never occupy a
slot.

Transport is plain `fetch()`, not the generated `openapi-fetch` client: the
schema types this route's 200 body as `content?: never` because it is raw
binary, and there is no `{data}` envelope to unwrap. The auth seam is reused
exactly as `src/api/client.ts` does — `await auth.getToken()` into a
`Bearer` header, and `auth.onAuthFailure()` on a 401 before returning the
failure. (`getToken()` may return a Promise. The WebSocket path throws on that,
because a socket bakes its subprotocols in at construction; the media path has
no such excuse and must await, which is what M-W12's Tauri keychain provider
will need.) Statuses map to a typed failure the UI renders: `401 → auth`,
`404 → not_found`, `413 → too_large`, anything else → `network`.

### Lazy-load through one shared observer

`useMediaBlob(accountId, mxcUrl)` returns a status and an element ref. A single
module-level `IntersectionObserver` (`rootMargin: '200px'`, resolving just ahead
of the viewport) serves the whole timeline through a `WeakMap` of callbacks —
not one observer per image. On first intersection the hook unobserves, acquires,
and latches, so a re-intersection cannot double-acquire.

Observer and semaphore compose without coordinating: the observer decides
*whether* a fetch is enqueued, the semaphore decides *when* it runs. A fast
scroll enqueues many and they drain six at a time in intersection order.

jsdom has no `IntersectionObserver`, so the hook falls back to an eager acquire
on mount behind `typeof IntersectionObserver === 'undefined'` — the same guard
the timeline's scroll-back sentinel already uses. Component tests then drive the
real load path without a stub, and we deliberately do not shim the observer
globally.

### The aspect-ratio box is load-bearing

`MediaImage` reserves its final height *before* the blob resolves, from
`content.info.w`/`h`, via `aspect-ratio` on a wrapper. This is not polish. The
timeline scrolls to bottom on mount and on own-send and never re-anchors, so an
image that grows when its bytes arrive shoves every event below it and destroys
a scroll-back position the reader was using. Without dimensions in `info` we
accept one small shift against a fixed-min-height placeholder.

The displayed source is `info.thumbnail_url ?? info.thumbnail_file.url ?? url` —
a sender-embedded thumbnail when one exists. "No server thumbnails" means we
never ask the server to *generate* one, not that we ignore one already in the
event. The lightbox always loads the full-size `url`.

### The ciphertext-fallback 200

When the server holds no decryption key for an older message, the proxy returns
**raw ciphertext with a 200 and a plausible content-type**. The fetch succeeds,
the blob is created, and the failure surfaces only when the browser tries to
decode the image. So `MediaImage` must wire `onError` on the `<img>` and render
"encrypted media — server could not decrypt", matching the TUI, which catches
the same case at its decode step. A fetch-level status check will never see it.

### Re-admitting `<img>` to the sanitizer, without a remote-load vector

`src/html/sanitize.ts` gains `img` in `ALLOWED_TAGS` and `alt`/`width`/`height`/
`data-mxc` in `ALLOWED_ATTR`. It does **not** gain `src`.

The existing `uponSanitizeElement` hook — which today lifts `alt` into a text
node so a dropped void tag leaves something behind — is replaced by one that,
for an `img`, copies an `mxc://` `src` into `data-mxc` and then unconditionally
removes `src`. The hook runs before per-attribute filtering, so the mxc value
survives inside an allowlisted attribute while the original `src`, unlisted, is
dropped. (`data-mxc` also joins `ADD_URI_SAFE_ATTR`, or DOMPurify strips a value
that looks like a URI scheme — the same trap the `data-mx-spoiler` comment
already documents.)

An `http(s)` src therefore never lands in any surviving attribute, and the
browser never issues a request for it. This is deliberate and stronger than the
Matrix spec requires: a remote image in a `formatted_body` is a tracking pixel
that reports the reader's IP address to a server the sender chose. Element
proxies such images through the homeserver; we have no proxy for arbitrary
`https` URLs, so we render nothing. An unresolvable image degrades to the
browser's native `alt` rendering, which is what the old text-node trick was
imitating.

`FormattedBody` then resolves `img[data-mxc]` after mount, in a layout effect,
setting `src` to the blob URL and releasing every handle on cleanup. Because the
subtree comes from `dangerouslySetInnerHTML`, this is imperative DOM rather than
VNodes — contained to one effect, and the price of sanitizing HTML we did not
author.

### `EventBody` moves out of `RoomPage`, and threads get media too

`ThreadPanel` does not use `EventBody`. It calls `FormattedBody` directly for
the thread root and each reply, with its own inline redaction check. Left alone,
an image posted in a thread would render as a filename while the same event
renders inline in the main timeline.

So `EventBody` is extracted to `src/components/EventBody.tsx` and used by both.
`ThreadPanel` already has `accountId` in scope, so the lift is clean, and it
deletes a duplicated redaction branch. `FormattedBody` grows an `accountId`
prop, which `EditHistory` also passes through. This is the sort of divergence
that only appears once a second renderer of the same event exists; folding it
back now is cheaper than after the media components multiply.

### Verification

An automated e2e lane proves the transport: `e2e/mock-server.mjs` serves a tiny
PNG from a mock `/v1/media/...` route, and the spec asserts the timeline `<img>`
acquires a `blob:` src, opens the lightbox on click, and closes it on Escape.
That covers fetch → Bearer → blob → object URL → render in a real browser.

The milestone's exit criterion — "encrypted attachment from the integration
harness renders inline" — has no automated path. `crates/axon-itest`'s seeder
sends an encrypted attachment with `send_attachment` against a live homeserver
and prints its event id, but nothing wires it to the web client. Per the web
client's definition of done, this is the human pass: run the seeder, let axon
backfill and recover keys, mint a read token, open the client at that room, and
confirm the image renders inline. The server decrypts; the client only fetches.
Confirm the ciphertext-fallback placeholder on a message whose key the server
lacks. Per the project's standing rule, send nothing outside "Axon Testing".

## Consequences

- **Blob memory is bounded but real.** Every image the reader has scrolled past
  and not yet displaced from the 32-entry LRU is a decoded blob held in memory,
  plus every image currently mounted. A very long scroll-back with large images
  in a room without sender thumbnails is the worst case. If it bites, the fix is
  a windowed timeline (which the room list already demonstrates), not a smaller
  cache — shrinking the cache only makes scroll-back re-fetch.

- **A missing `release()` is a slow leak, not a crash.** It is invisible in
  review and invisible at runtime until memory grows. Acquire and release are
  therefore paired inside `useMediaBlob` and nowhere else; components never call
  `acquire()` directly.

- **No streaming, no `Range` from this client**, though the server supports it.
  Seeking within a video would need a transport that reaches the media route
  without a blob, which is the same missing piece as native players.

- **Deferred:** native `<video>`/`<audio>` players; upload (blocked on ADR 0059's
  server work); avatars, which are `mxc://` URIs on the room and member DTOs and
  could reuse `MediaService` unchanged, but are their own UI silo; a windowed
  timeline; and proxying remote `https` images in formatted bodies.

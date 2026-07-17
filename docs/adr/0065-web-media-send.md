# ADR 0065 — Web client media send (M-W8.5)

## Context

ADR 0064 gave the web client the read half of media: an `m.image` renders
inline, an `m.file` renders as a download card. The write half does not exist.
There is no `<input type="file">`, no drop target, no paste-of-a-screenshot
handler, and no outbound-bytes code path of any kind in `clients/web` — the
only thing the client can put in a room is text.

ADR 0059 (M15, PRs 254/256) landed the server side and explicitly scoped out
client UX: a staged upload (`POST …/media/uploads`) followed by a room-aware
send (`POST …/rooms/{room_id}/send-media`). ADR 0061 is that follow-on for
`axon-tui` — a `/send <path> [caption]` slash command. This ADR is the same
follow-on for the web client, and deliberately does _not_ mirror the TUI's
shape: a browser has no filesystem path to type, and it does have a file
picker, a drop event, and a clipboard that carries images.

The server contract fixes two things the client must respect:

- **`kind=image` requires an `image/*` content type** (`axon-sync/src/gateway.rs`
  rejects the mismatch as a `400`). The kind and the MIME cannot be chosen
  independently.
- **When `caption` is absent, the event body is the staged filename.** The
  client does not get to pick the body of a caption-less media event.

## Decision

### Three entry points, one staging step

Attach comes from a paperclip button (a hidden `<input type="file">`), a drop
anywhere on the room pane, or a paste carrying a file. All three land in the
same place: a _staged attachment_ held in `RoomPage`'s state and rendered as a
chip above the composer. Nothing is uploaded at attach time.

Staging rather than send-on-attach is what makes captions work: the composer
text at submit time becomes the `caption`. A drop that sent immediately would
leave no moment in which to type one. Enter with a staged file and an empty
composer sends the file bare — the common case, and the reason `submit()`'s
empty-body guard has to learn about attachments.

One file at a time. A multi-file drop takes the first and says so.

### `kind` is derived from `File.type`, not from a filename extension

`axon-tui` maps an extension to a `(kind, content_type)` pair
(`media_kind_and_content_type`) because a path on disk is all it has. The
browser hands us `File.type` — the platform's own MIME determination — so the
web client uses it directly:

```
kind = file.type.startsWith('image/') ? 'image' : 'file'
```

The two are then consistent by construction, which is exactly the invariant the
server's `image/*` rule polices. An empty `File.type` (the browser could not
determine one) falls to `file` with no `Content-Type` header at all, which the
handler accepts (`sanitize_content_type` treats the header as optional). The
extension table is not ported; a comment in `media-service.ts` points at the
Rust original and says why it is absent.

### The upload rides the hand-rolled `fetch`, not `openapi-fetch`

`openapi-fetch` is JSON-oriented and the staging body is raw
`application/octet-stream`. `media-service.ts` already owns a hand-rolled
`fetch` + `Bearer` path for downloads (a browser cannot put an `Authorization`
header on `<img src>`), so `upload()` joins it there and reuses its failure
taxonomy and its `401` → `onAuthFailure()` rule. The _send_ half is an ordinary
JSON mutation and goes through the typed client in the timeline store like every
other mutation.

A `File` is a `Blob`, so it is passed to `fetch` as the body directly — no
`FormData`, no base64, no reading the file into memory. `fetch` derives the
`Content-Type` from `file.type` and omits it when the type is empty, which is
precisely the behavior the server wants.

The upload does **not** pass through the download semaphore. That gate exists to
model the browser's per-host connection budget for many small thumbnails; a
100 MB upload parked in it would stall every image on screen.

`MAX_UPLOAD_BYTES` (100 MB) mirrors the TUI's constant and is checked before any
request, so an oversized file fails instantly and legibly instead of after a
long transfer. The server's `max_upload_bytes` is configurable and may be lower,
so the `413` path stays live regardless.

### An optimistic local echo, unlike the TUI

ADR 0061 deferred a local echo for `/send`: the sent event just arrives over the
WebSocket. The web client does not follow, because the latency it would be
hiding is different in kind. A text send is one small JSON round trip; a media
send is a whole file's transfer time. Several seconds of a composer that has
visibly cleared and a timeline that shows nothing reads as a dropped message,
and the user's instinct is to send it again.

So a media send renders an echo the moment it is submitted, reusing the existing
`localEcho` machinery rather than a parallel one: an image echo shows the actual
image immediately, from an `URL.createObjectURL()` of the local `File`, under
the same "Sending…" status a text echo uses, with the same Retry/Discard
controls on failure.

Two things fall out of the existing design for free, and one has to be added:

- **Reconciliation is free.** The echo's `body` is `caption ?? file.name` —
  exactly what the server sets the event body to. The `confirmsEcho` body match
  in `ingestLive` therefore drops a media echo when the live frame lands, with
  no new matching rule.
- **Retry is free, but only if the `File` survives.** The echo retains the
  `File`, so a retry re-uploads the same bytes. This is the root AGENTS.md rule
  that user-entered text must survive a failed mutation, applied to an
  attachment: a failed upload must never cost the user the file they picked.
- **Object-URL revocation is not free.** Every path that removes an echo — a
  discard, a reconcile, a live frame confirming it — must revoke the preview
  url, so those three separate `filter` calls collapse into one `dropEcho()`.

### The echo cannot render through `parseMedia`

`parseMedia` returns `null` for an image whose url is not an `mxc://` URI — a
deliberate ADR 0064 rule, since a non-`mxc` src is not something the media proxy
can fetch. A local echo has no mxc url yet, so it would parse as _not media_ and
fall through to `FormattedBody`, rendering as the bare filename: precisely the
bug ADR 0064 was written to fix, reintroduced from the other direction.

The echo therefore gets an explicit seam rather than a synthetic mxc url:
`EventBody` branches on `localEcho.media` _before_ the `parseMedia` dispatch,
and `MediaImage` takes an optional `previewUrl` that, when present, is rendered
directly and passes `null` to `useMediaBlob` (which already no-ops on a null
url, keeping hook order unconditional). `MediaAttachment` needs no change at
all: it already treats a non-`mxc` url as not-downloadable, which is the correct
state for a file that is still uploading.

## Consequences

- The web client gains its first outbound-bytes path and its first drop/paste
  handling.
- `createTimelineStore` gains the media service as a dependency, so its call
  sites and the test harness widen.
- Reply and thread relations compose with media exactly as they do with text —
  `send-media` takes the same `reply_to`/`thread_root`, and the thread panel's
  composer is wired identically.
- Not in scope, matching ADR 0059's own exclusions: audio/video msgtypes (the
  server's `kind` enum is `image|file`), client-side thumbnail generation,
  image dimensions or EXIF, multi-file sends, cancelling an in-flight upload,
  and `DELETE …/media/uploads/{id}` cleanup of a stage that was never sent.
- Upload _progress_ is indeterminate ("Uploading…"), not a percentage: `fetch`
  cannot report upload progress, which needs `XMLHttpRequest`. If the
  indeterminate state proves too thin for large files, that is the follow-up.

# clients/web — agent & developer notes

Preact + TypeScript + Vite SPA (ADR 0046). Setup, scripts, and env vars are
in [README.md](README.md); this file is the working knowledge that isn't
obvious from the code. The governing design is
`docs/adr/0046-web-client-framework-and-roadmap.md` — read its roadmap table
before starting a milestone.

## Ground rules

- **One silo per PR** (project-wide rule): a web PR touches only this
  package (plus its CI workflow). Server changes — even one-line ones this
  client needs — are separate PRs on separate bookmarks.
- **The OpenAPI contract is the boundary.** `src/api/schema.d.ts` is
  generated (`pnpm gen:api`) and committed; CI fails if it drifts from
  `openapi/openapi.json`. Never hand-edit it; never widen a type to work
  around the contract — if the server lacks a field, model the gap
  explicitly (see the `sync_state` note below).
- **TUI parity is the spec for shared semantics.** Room-list
  sort/filter/titles (`src/stores/room-list.ts`) are ported from
  `clients/tui/src/app/rooms.rs`; the HTML subset mirrors
  `clients/tui/src/html.rs`. When behavior is ambiguous, read the TUI
  source and match it — and leave a comment pointing at the Rust original.
- **Live testing:** message sends and other mutations against the live
  server go to the "Axon Testing" room only
  (`!SScJmZuEkBUnuydXdf:bostoncoop.net`). Everything else is a real account.
  Reads are safe anywhere. Mint tokens with `axon token issue`, revoke when
  done.

## Architecture

- **Service graph** (`src/services.ts`): auth provider → typed API client →
  stores, built once in `createServices()`, provided via context
  (`useServices()`). Tests build the same graph over msw + in-memory
  storage via `src/test/services.ts` — components never construct services.
- **State is @preact/signals** in plain store factories
  (`src/stores/*.ts`), unit-testable without rendering. Direct
  `signal.value = x` writes are the idiom; the `react-hooks/immutability`
  lint rule is disabled for exactly this reason.
- **Auth seam** (`src/auth/provider.ts`, per ADR 0031): `getToken()` (sync
  or async), `onAuthFailure()`, `LoginBootstrap` UI slot. Token-paste over
  `localStorage` is the alpha implementation; OAuth/PKCE and a Tauri
  keychain provider must fit this interface without consumer changes.
- **Settings** (`src/stores/settings.ts`): one schema-versioned
  `localStorage` envelope. Add fields with defaults (old envelopes must
  parse); bump the version only for incompatible reshapes. Anything
  unparseable resets to defaults.
- **Timelines** (`src/stores/timeline.ts`): server pages newest-first;
  stores hold ascending display order. One factory serves both the room
  timeline and thread timelines (`threadRoot` param). Sends render an
  optimistic local echo (`TimelineEvent.localEcho`, a client-only extension
  of `EventDto` — not server-driven, same pattern as `sync_state` below) with
  a synthetic `local:<uuid>` event id, then reconcile by re-fetching the
  confirmed event and patching it in place, or marking the echo `failed`
  (retryable via `retrySend`/discardable via `discardSend`) on error;
  edit/redact/react use the same re-fetch-and-patch shape against a real
  event id (scroll position survives throughout — no full reload).
- **Sanitizer** (`src/html/sanitize.ts`): DOMPurify + Matrix subset +
  transforms (data-mx-color/bg → inline style, spoilers → click-to-reveal,
  legacy `font color`, mx-reply dropped with contents, bare-URL
  linkification skipping a/code/pre). Gotcha: custom attributes whose
  values look like URI schemes need `ADD_URI_SAFE_ATTR` or DOMPurify drops
  them. `<img>` is admitted for `mxc://` only (M-W8, ADR 0064): the
  `uponSanitizeElement` hook copies a safe `mxc://` src to `data-mxc` and
  always drops `src`, so a remote `http(s)` src (a tracking pixel we cannot
  proxy) never survives; `FormattedBody` resolves `data-mxc` after mount.
- **Media** (`src/media/`, ADR 0064): a browser cannot put a bearer token on
  `<img src>`, so `MediaService` fetches every `mxc://` through the proxy and
  hands the DOM a blob URL. The cache **refcounts** — the timeline is not
  windowed, so a size-based LRU would revoke a URL a mounted `<img>` still
  points at; `acquire()` returns a handle whose `release()` the caller must
  call, and only zero-ref entries are eligible for the 32-entry LRU. Lazy-load
  via one shared `IntersectionObserver` (`useMediaBlob`), which falls back to
  eager acquire under jsdom. A 200 of raw ciphertext (server lacks the key)
  fails only at `<img>` decode, caught by `onError`.
- **Markdown-on-send** (`src/markdown/markdown.ts`): plain prose sends a
  bare body; detected formatting sends `org.matrix.custom.html`. The server
  never interprets Markdown. Raw inline HTML in composer input is escaped.
- **Routing**: history mode (signed off). The deep-link contract is
  `/:accountId/rooms/:roomId` + `?thread=<root_id>` + `?event=<event_id>` —
  search (M-W10) and the mobile clients build on it; do not change it.
  Deployment requires unknown-path → `index.html` rewrite.
- **Live ephemerals** (`src/stores/ephemeral.ts`): `ephemeral.passthrough`
  frames are live-only overlays. `m.typing` is whole-list replace per room,
  self-expires, and clears on socket gaps. `m.receipt` is parsed from Matrix's
  nested raw content; the UI renders public read receipts only on the current
  user's own messages. Presence is still deferred.

## Guardrails (from the 2026-07 review)

Recurring failure modes from the M-W1–M-W8 review
(`docs/reviews/2026-07-web-client-review.md`); the WCR numbers below refer to
its findings. The repo-wide guardrails in the root `AGENTS.md` (notably
"user-entered text must survive a failed mutation" and "every view of server
state declares its freshness story") apply here too.

- **Keys go on the outermost element a `.map()` returns.** If a row needs a
  rendered sibling (a day separator), wrap both in `<Fragment key={id}>` —
  never a bare `<>`. Preact reconciles unkeyed fragments by index, so a
  prepend attaches per-row state (an open confirm, a picker) to the wrong
  row. (WCR-01; `RoomList.tsx` learned this once already.)
- **`openapi-fetch` rejects on network failure; only HTTP errors come back
  as the `{error}` envelope.** Every fire-and-forget call (`void
api.GET(...)`, a background `.then`) must attach a rejection handler: wrap
  it in `inBackground(...)` (`src/api/client.ts`) when failure needs no UI,
  or handle the rejection yourself when it does (`ServerStatus.tsx`,
  `EditHistory.tsx`). Corollary: a vitest run whose tests all pass but whose
  exit code is nonzero with an "Unhandled Errors" block is a **failing**
  gate; that block is the tripwire for exactly this bug class, never noise
  to ship past. (WCR-02/04.)
- **A store method that replaces or splices a signal-held collection must
  assume a sibling request is in flight.** Guard with a request-generation
  token and discard stale completions — two responses for the same resource
  can land out of order (pagination vs. reconnect gap-fill is the canonical
  interleaving). (WCR-03.)
- **Overlays follow one modal contract:** capture-phase Escape, focus saved
  on open and restored on close, Tab trapped inside. Use
  `useModalFocus()` (`src/components/use-modal-focus.ts`) for the focus
  half and a capture-phase `useShortcuts` Escape binding for the other;
  `Lightbox.tsx` shows both together. (WCR-14.)
- **Composite in-memory cache keys join with `'\0'`** (as in
  `media-service.ts`), never a printable character — and always written as
  the _escape sequence_, never a raw control byte in source. A raw NUL sat
  in `device-state.ts` and rendered invisibly, making the code look like it
  joined on a space; it fooled the 2026-07 review into reporting exactly
  that (WCR-11's premise was this artifact, not a real space).

## Test environment gotchas (all discovered the hard way)

- jsdom under Node 25 exposes `window.localStorage` as a bare object —
  inject `memoryStorage()` from `src/test/memory-storage.ts` instead.
- testing-library auto-cleanup needs vitest globals (not enabled): add
  `afterEach(cleanup)` in every component test file.
- msw handler paths: use `:param` segments for ids containing `$`/`:`
  (Matrix event/room ids) — literal or percent-encoded paths don't match.
- Generated free-form objects (`content`, `relates_to`) type as
  `Record<string, never>`; test fixtures take them loosely and cast
  `as unknown as EventDto`.
- preact-iso's `Router` type wants ≥ 2 children; add a `default` route.
- Run pnpm from this directory — from the repo root it fails with
  `ERR_PNPM_NO_PKG_MANIFEST`.

## Server gaps this client already accounts for

- **ADR 0030 `sync_state`** is unimplemented server-side. The accounts UI
  reads it opportunistically through one typed extension
  (`src/stores/accounts.ts`); when the server adds the field, `gen:api`
  makes it real and the extension alias gets deleted. Tracked in the parent
  repo's issues.
- **ADR 0055 `is_direct`** is docs-only; the DM heuristic (blank name +
  alias, `isLikelyDm`) is the interim, swapped in one function when the
  server field lands — same plan as the TUI.

## Roadmap position (ADR 0046 table)

M-W1–M-W8.5 are done (M-W7 was built before M-W6
deliberately — messaging is pure HTTP; M-W8.5, media send, was unblocked late
by M15's upload API and so sits between M-W8 and M-W9). Remaining: **M-W9**
(verification/SAS + trust glyphs), **M-W10** (search UI over `GET /v1/search`, deep-linking via
`?event=`), **M-W11** (hardening/a11y/parity audit), **M-W12** (Tauri —
no service workers, `document.cookie`, or `window.open` anywhere, ever).

## Testing traps

- **A jsdom `File` is not undici's `Blob`.** Hand a `File` to `fetch` as a
  request body under vitest and the body arrives at msw as the literal string
  `"undefined"` — the `Content-Type` still comes through, so the request _looks_
  right and only the bytes are silently wrong. Upload **bytes** therefore cannot
  be asserted in a unit test; `e2e/media-send.spec.ts` exists to assert them in
  a real browser (it compares a digest, since media is binary). Unit tests may
  still assert the query params, the headers, and the failure mapping.
- **`tsc --noEmit` is not the typecheck.** Only `pnpm build` (`tsc -b`) uses the
  project's real config. The generated schema types an event's `content` as
  `Record<string, never>`, and `--noEmit` waves through assignments to it that
  the build rejects — which is why local echoes cast (`as unknown as
TimelineEvent`).
- **The e2e mock server outlives a single spec file** (`reuseExistingServer`).
  A spec that appends to its seeded `timeline` array pollutes every later spec;
  `send-media` deliberately only broadcasts and records for `/events/:id`.
- **`e2e/media.spec.ts` is flaky here:** headless `IntersectionObserver`
  sometimes never fires, so lazy-loaded media stays a skeleton and no proxy
  fetch is issued. It reproduces on unmodified code — don't chase it as a
  regression in your diff.

## Definition of done for a milestone

`pnpm test && pnpm lint && pnpm format:check && pnpm build` all green; new
logic has unit tests (stores) and interaction tests (pages, msw-backed);
README status paragraph updated; a human pass against the live server
(read-only outside the test room); one commit on a jj bookmark stacked on
the previous milestone, described but not pushed unless asked.

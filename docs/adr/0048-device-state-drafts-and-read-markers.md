# ADR 0048 — Per-device state: drafts and read markers (M12)

## Context

M12 (`docs/mvp/implementation.md` §12) gives clients a generic per-device
key/value store so drafts, read markers, and similar client-side state sync
across a user's devices: a `device_state` table keyed by
`(account_id, device_id, namespace, key)` with an opaque value and
`updated_at`, `GET`/`PUT /v1/devices/{device_id}/state/{namespace}`, and
last-write-wins change fan-out over `/v1/ws`. The spec leaves several wire-
and semantics-level choices open; this ADR records them before
implementation.

Constraints that shaped the design: bearer tokens are instance-global (ADR
0029) so auth carries no device or account identity; the live bus is a single
global `broadcast::Sender<LiveFrame>` with no per-socket identity or
filtering (ADR 0020); and axon has no client-device concept anywhere today —
every existing `device_id` is a *Matrix* device, not a client of axon.

## Decision

- **Account scoping via a required `account_id` query parameter** on the
  spec's URL (the search-endpoint style), keeping the URL exactly as written
  in the implementation plan. The table carries `account_id` with the
  standard `REFERENCES accounts ON DELETE CASCADE`, so device state dies
  with its account like every other account-scoped table. A device (one
  client install) is one UUID across all accounts; its rows are partitioned
  per account.
- **Device identity is a client-supplied UUID, registered implicitly by its
  first PUT.** No `devices` table, no registration endpoint, no listing or
  revocation (YAGNI for MVP). The path segment is validated as a UUID and
  that is all: with instance-global tokens, any authorized client may read
  or write any device's state — the token is already the trust boundary for
  the whole read surface, and device state is the *least* sensitive data
  behind it.
- **Last-write-wins on the server clock.** `updated_at` is set by the
  shared `trigger_set_updated_at` machinery (`DEFAULT now()` on insert);
  the last PUT to *arrive* wins. No client timestamps, no freshness-guarded
  upsert: client clocks are not trusted, and the offline-reconnect case is
  handled by convention — a client GETs (the merged view) before its first
  PUT after reconnecting, so it never blindly replays stale state.
- **PUT is a merge-upsert of an `{key: value}` map; `null` writes a
  tombstone.** Only the keys present in the body are touched, so two rooms'
  drafts never race each other (whole-namespace replace would). A cleared
  draft must *win* the merge, not vanish from it: deleting the row would let
  another device's older row resurface, so a clear is stored as a row with
  `value = NULL` and a fresh `updated_at`.
- **GET returns the LWW-merged view across all the account's devices** —
  per key, the newest `updated_at` wins (ties broken by `device_id` for
  determinism) and tombstone winners are omitted. Without merging, a
  restarted client could never pick up the draft typed on another device,
  which is the whole point of the milestone. Per-device raw reads can be
  added later if a use case appears.
- **Values are nullable `JSONB`.** The wire is JSON either way; `JSONB`
  matches `account_data.content` and avoids base64. "Opaque" means axon
  never interprets the value, not that it is binary.
- **Writes are size-capped, not shape-validated.** Because the values are
  opaque, the server cannot validate their shape without coupling itself to
  client semantics — so it bounds their size instead: at most 64 entries per
  PUT, 64 KiB per serialized value (Matrix's own whole-event cap), 512 bytes
  per key, and 64 bytes of namespace, each violation a readable `400`. Row
  *count* is already structurally bounded (devices × namespaces × keys —
  rooms-scale for the intended namespaces), so no quota machinery beyond
  these caps.
- **Fan-out rides the existing global bus with client-side echo
  suppression.** A successful PUT publishes one `LiveFrame::DeviceState`
  frame (tag `device_state.changed`) carrying the originator `device_id`,
  the namespace, and the written entries; clients drop frames whose
  `device_id` is their own, exactly as they already self-filter by
  `account_id`. This is the only option the bus permits today and matches
  its existing contract (lossy, clients re-read on reconnect).
- **An incoming value is adopted into a visible input only while that input
  is "clean": empty, or still holding exactly the value last synced to it.**
  Anything else is an unsynced local edit, and a sibling's write must never
  clobber in-progress typing. Crucially, a *clear* (`''`, the tombstone's
  rendered form) is an ordinary value under this rule, not a special case to
  be skipped: a device that clears its draft clears its siblings' composers
  too, or the two clients disagree about what the merged state says. Both
  clients implement exactly this — `buffer_clean` in the TUI's
  `handle_draft_frame` (`clients/tui/src/app/drafts.rs`) and the `synced`-ref
  adoption effect in the web `Composer`. Note the rule keys on the *last
  synced value*, not on a sticky "has the user ever typed here" flag: such a
  flag conflates "dirty" with "was touched", so a field that has been typed
  in and then emptied — or a composer reused across rooms — never adopts
  again.

## Consequences

- Clients get cross-device drafts/read-markers with one GET at startup, one
  debounced PUT per change, and one WS frame type — no new auth or transport
  machinery.
- Server-clock LWW means a client that skips GET-before-PUT after a long
  offline gap can overwrite newer state; accepted for MVP simplicity, and
  recoverable (it's a draft, not history).
- The merged GET hides which device authored a value unless the client looks
  at the per-entry `device_id` the DTO exposes; tombstoned keys are simply
  absent.
- The clean-buffer rule makes per-key UI state a correctness concern, not a
  cosmetic one: a composer shared across rooms (one component instance, an
  unkeyed route) keeps the previous room's unsent draft, and pressing Enter
  sends it to the wrong room. Clients must scope the input to its key — the
  web client keys the composer on `roomId`.
- Tombstone rows accrete (one per cleared key per device); bounded by
  (devices × namespaces × keys), which for drafts/read-markers is rooms-
  scale. A sweep can be added later if it ever matters.
- Every socket sees every account's and device's frames (status quo of the
  global bus); acceptable under the single-human premise, revisit if the
  bus ever grows per-connection scoping.

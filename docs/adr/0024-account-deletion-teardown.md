# ADR 0024 — Account deletion: an ordered, crash-recoverable teardown

## Context

ADR 0022 introduced the account lifecycle state machine and reserved `deleting`
as a *transient teardown breadcrumb*, with a forward note that the destructive
`DELETE` verb, a boot-time reconcile, and an orphan-store-dir GC would land
together in a later 7a PR. This ADR records the decisions that PR makes.

Deleting an account is not a single `DELETE FROM accounts`. "Every trace" of an
account spans more than the Postgres row: the per-account on-disk SDK store
(`data_dir/<account_id>/`, holding the device's Olm/Megolm + cross-signing
material), a live in-process SDK `Client`, a supervised sync task, and — once
those subsystems exist — a Tantivy search index (M9) and a media cache (M11). A
plain row delete would strand all of these. Worse, a process crash *mid-teardown*
must not leave an account half-removed with no way to finish.

The driving requirement (spec §7, GH #14/#24): make removal **ordered,
idempotent, and crash-recoverable**, so an account can be retired explicitly
(`DELETE`) and any crash mid-teardown converges on restart.

**Scope note — what this does *not* close.** GH #24 has two halves. This PR
delivers the *explicit-removal* half (a durable `DELETE` verb) plus crash recovery
and orphan-directory cleanup. It does **not** auto-retire an account that was
*dropped from `sync.account`* — a config-dropped row is still `active`, still has
its row and dir, and neither the `deleting`-reconcile nor the orphan-dir GC touches
it (both key off teardown state / row absence, not config membership). ADR 0022
filed that auto-retirement under this subphase, but it does not survive the arrival
of runtime `login`: with accounts addable at runtime, "deactivate any active row
not in config" would wrongly kill every runtime-added account — config is no longer
the source of truth for which accounts should exist. The real close of that half is
to **retire config-based provisioning** once the recovery-key path it carries has a
runtime home (the `recover` endpoint, a later 7a PR); see *Consequences*. Until
then this PR adds a transitional guard so `DELETE` is at least never silently undone
(below). **This has since happened** — see *Consequences* → "Resolved" for the
retirement that closed this half for good.

## Decision

### The row is deleted last; the order is load-bearing

The `accounts` row is the only durable key from which a reconcile can re-find the
*external* resources (the SDK store dir is by id; search docs and media entries
would be keyed by id too). So the teardown deletes the row **last**, and on a
crash the row — left in `deleting` — is exactly what tells the next boot that
external cleanup is still owed. The sequence (`AccountLifecycle::delete`, in
`axon-sync`):

1. **Flip the row to `deleting`.** A durable "cleanup owed" marker, *and* — since
   it moves the row out of `active` — the flip that makes `get_or_connect`'s
   cold-connect gate refuse any *new* client before the cached one is taken
   (flip-before-take, the same ordering logout uses).
2. **Sever the live session** (`sever_session`, shared with logout): reap the
   supervised task **awaiting its drain** (cooperative cancel → abort escalation),
   then take the cached `Client` out of its slot and best-effort, time-capped,
   invalidate the device token upstream.
3. *(reserved)* delete the account's documents from the search index — **M9, not
   built; see below.**
4. **Remove the on-disk SDK store dir** (`data_dir/<account_id>/` and its
   `<account_id>.prev` staging backup).
5. *(reserved)* purge the account's entries from the media cache — **M11, not
   built; see below.**
6. **Delete the `accounts` row.** FK cascades (`ON DELETE CASCADE` on `events`,
   `room_state`, `account_data`, and the event crypto siblings) drop the Postgres
   archive in the same statement.

Then the identity's per-identity lock-map entry is pruned (the identity is retired
for good).

### A wedged task aborts the teardown *before* anything is removed

Reaping can fail: a task wedged in non-yielding code survives both cancel and
abort (ADR 0022). When it does, `sever_session` (step 2) returns
`LifecycleError::Draining` and the teardown **stops there** — the row stays
`deleting`, the store dir is **not** touched, and the verb returns `409`. A retry
(or the next boot's reconcile) finishes once the task finally dies. This is why
store-dir removal sits *after* a successful sever: a live task still holds the
dir's SQLite handles, and removing it out from under the task is the one thing the
ordering must prevent.

### Idempotent and resumable

Every step tolerates being re-run: `set_account_state` is a no-op if already
`deleting`, `remove_account_store_dirs` treats an absent dir as success,
`delete_account_row` deletes zero rows for an already-gone account. So `delete`
by id resolves:

- `active` / `deactivated` → full teardown.
- `deleting` → **resume** (the branch the reconcile and a client retry hit).
- no row → `NotFound` (`404`) — also what a second concurrent delete sees once the
  first wins the per-identity lock.

### Superseded: the transitional config-provisioning guard

This PR originally shipped a transitional guard here: `delete` refused to
*initiate* on an account the running `sync.account` config still named
(`matches_account`), returning `409` with "remove it from `sync.account` first",
so a boot's unconditional `upsert_account` couldn't silently recreate a just-deleted
row. A row already `deleting` was exempt, so the boot reconcile could still finish
an in-flight teardown — but that exemption had its own two-restart hole (GH #66): a
config-provisioned identity already `deleting` at boot (a crash mid-teardown, or
manually-set state) would have its deletion completed by *this* boot's reconcile,
and then resurrected by the *next* boot's unconditional upsert, since nothing
recorded that the identity had been deliberately torn down.

A tombstone table (`deleted_account_identities`, written atomically with the row's
removal, consulted by boot provisioning before upserting) closed that hole as a
narrow fix. It was short-lived: the real blocker for the *principled* close — no
runtime equivalent for the `sync.account.recovery_key` boot path — cleared once
`POST /v1/accounts/{id}/recover` landed, so config-based provisioning was retired
in the same effort rather than carrying the tombstone forward as permanent
apparatus for a path that no longer exists. See *Consequences* below: with no
boot-time provisioning, nothing can resurrect a deleted row, by construction — the
guard, the tombstone, and this whole failure class are gone rather than "made
correct."

### Boot reconcile + orphan-store-dir GC

Two startup sweeps (`reconcile` module), run inside `SyncEngine::start` **before**
the account-spawn loop and **before** `axon-server` binds the HTTP listener — so
neither races API traffic or a supervised task creating a fresh store dir:

- **`reconcile_deleting`** lists rows still in `deleting` and drives each through
  the *same* `delete` verb to completion. Resilient: a row that fails (or, at
  boot, unexpectedly `Draining`s) is logged and left for the next boot; one bad
  row never blocks the healthy accounts from coming online.
- **`prune_orphan_store_dirs`** removes a `data_dir/<uuid>/` dir (or its `.prev`
  backup) whose `uuid` matches **no `accounts` row in any state**. Keyed off row
  *existence*, never lifecycle state: a `deactivated` row is real and may be
  reactivated, so its dir must be kept — pruning by "not active" is exactly the
  #24 failure mode. Non-`<uuid>` entries are left strictly untouched.

The two are complementary: the `deleting`-state reconcile drives the *whole*
teardown to completion (it alone reaches the search/media steps and the row),
while the orphan-dir GC is only a **backstop for row-less dirs** — it can't reach
search docs or media entries, which are keyed by a row that, in the orphan case,
is already gone. So GC must never be relied on for a normal delete; it cleans up
dirs stranded by an *older*, pre-`deleting`-marker world (the genuine orphans #14
observed).

### Concurrency: the per-identity lock carries it

`delete` holds the per-identity async lock (keyed by the canonical
`(user_id, homeserver_url)`, the same key login/logout use) across the **entire**
teardown, so a concurrent login/logout/delete on that identity is strictly
serialized — and, after a delete, observes a gone row (login mints a fresh
`account_id`; logout/delete `404`). The one subtlety is pruning that lock entry:
doing it while another verb is *parked* on the same `Arc` would let a later
`lock_for` mint a fresh lock and run without mutual exclusion. So `prune_lock`
removes the entry only when `Arc::strong_count == 2` (just the map + the holder,
no parked waiters), under the map mutex so no waiter can appear between the check
and the removal; otherwise it leaves the entry (a tiny, bounded leak — correctness
over reclaiming a slot).

### Deferred at authoring, now landed: search-index and media-cache purge (M9 / M11)

The spec orders a search-doc deletion (step 3) and a media-cache purge (step 5)
into the teardown. When this ADR was written those subsystems did not exist —
`axon-search` and `axon-media` were crate stubs — so it added **no** code seam or
no-op hook for them (an abstraction with one trivial caller and no implementation
earns its keep only once the implementation exists) and recorded their ordering
slots here in prose. Both have since landed at their numbered positions, ahead of
the row delete:

- **Step 3 (search) — M9.** `delete_account_row` appends a durable account-purge
  sentinel to `search_outbox` in the *same* statement that drops the row (no FK,
  so it outlives the cascade); when the indexer is live the verb also `flush`es so
  the documents are gone before it returns (ADR 0039).
- **Step 5 (media) — M11.** `AccountLifecycle::delete` calls
  `MediaCacheHandle::purge_account` between the SDK-store-dir removal and the row
  delete: it drops the account's in-memory LRU entries and `remove_dir_all`s its
  `cache_dir/<account_id>/` directory. A boot `prune_orphan_media_dirs` sweep (the
  M11 analogue of `prune_orphan_store_dirs`) is the backstop for a purge that was
  interrupted or happened while the cache was disabled (ADR 0045).

The row-last invariant guarantees a reconcile can re-find and re-run either
cleanup, because the row outlives every external resource.

## Consequences

- **Pro:** removal is now a supported, crash-safe operation, replacing the manual
  DB surgery of #14 (a stale row is removed by an explicit `DELETE`, not by hand).
  The row-last ordering plus the `deleting` marker make any interrupted teardown
  converge on the next boot.
- **Resolved — config-based provisioning retired (closes the config-drop half,
  GH #65/#66).** This PR originally left the *config-drop* half open (a row
  dropped from `sync.account` stayed `active`) behind a transitional `DELETE`
  guard, with a follow-up tombstone patching a two-restart resurrection hole
  the guard didn't cover (GH #66). Once `POST /v1/accounts/{id}/recover`
  landed, the blocker on the principled close — no runtime equivalent for
  `sync.account.recovery_key` — cleared, so `AccountProvision` /
  `SyncConfig.account` and the entire boot-provisioning path were removed
  outright: `SyncEngine::start` no longer touches `accounts` before spawning
  tasks, `connect_account` only ever restores a stored session (every account
  is minted with one already, via `login` or the new `POST /v1/accounts/import`
  token-import verb), and the transitional guard (`LifecycleError::Provisioned`)
  and the tombstone table are both gone — there was nothing left for either to
  guard against. `DELETE` is durable **by construction**: no code path anywhere
  can recreate a row that isn't the result of an explicit runtime call.
- **Pro:** logout and delete share `sever_session`, so the "stop a live account"
  semantics (drain-awaiting reap, flip-before-take, best-effort upstream
  invalidation) are defined once.
- **Con / accepted residual:** a send that obtained a `Client` clone *before* the
  flip+take can still complete (or error when the store dir vanishes) during the
  teardown — the same residual logout carries, inherent to the Arc-backed SDK
  client. The account is being destroyed, so a last-gasp send is acceptable.
- **Con:** the per-identity lock is held for the duration of a teardown (up to the
  upstream-logout cap + drain), so a same-identity verb can block for that long.
  Acceptable: it is rare and per-identity (different identities never contend).
- **Scope:** `DELETE` is loopback-bound (a per-method layer, so the sibling `GET`
  on `/v1/accounts/{id}` stays open) until 7b's bearer gate lands, like the other
  destructive/secret-bearing lifecycle verbs. (Search/media purge have since landed
with M9/M11 — see the section above; the bearer gate landed in 7b.)

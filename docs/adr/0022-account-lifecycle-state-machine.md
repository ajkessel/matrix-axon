# ADR 0022 — Account lifecycle: an explicit state machine, and the M7 breakdown

## Context

Until now a Matrix account is provisioned exactly once, from config, and there is
no supported way to add, verify, recover, or remove one at runtime. The sync
engine brings online **every** `accounts` row it can decrypt a token for, and the
M6 mutations gateway (ADR 0021) will connect lazily for any `account_id` an API
send names. That "any row with a decryptable token connects" rule is the bug
behind GH #24: changing `sync.account.user_id` in config does not *replace* the
account — it inserts a new row and strands the old one, which keeps syncing and
can still **send**. This was hit in a real debugging session (a message went out
authored by a previously-configured account that was no longer in config).

Milestone 7 ("Account lifecycle and auth") makes the account lifecycle a
first-class concern. It is large, so it is sliced into small, reviewable PRs; this
ADR records both the **slicing** (so the plan is durable in-repo, not only in a
scratch planning doc) and the **first load-bearing decision** that the rest build
on: an explicit account state machine and the connection gate it drives.

The MVP spec (`docs/mvp/implementation.md`, §7) is the source of the requirements;
this ADR is the in-repo decision record those requirements produce.

## Decision

### An explicit lifecycle `state`, orthogonal to verification

Add a `state` column to `accounts` with three values:

- **`active`** — the normal state: the account syncs and can send.
- **`deactivated`** — a *reversible pause that retains all of axon's data*
  (decrypted archive, search index, media cache). Reached via `logout` or an
  internal token failure. It does not sync or send; a fresh `login` reactivates
  the **same row** (same `account_id` + retained archive), as a fresh Matrix
  device. This is **not** a soft-delete: it is the soft *stop*.
- **`deleting`** — a *transient* teardown breadcrumb set while a `DELETE` is in
  flight. A boot-time reconcile drives any row left here to completion. It is
  never a resting state a client observes long-term; deletion is a hard removal of
  the row, not a resting `deleted` tombstone.

`state` is **orthogonal to verification**: a device can be `active` but not yet
cross-signed — it syncs and shows UTDs until it acquires keys. So a separate
`verified` flag caches whether axon's own device is currently cross-signed. It is
**not** write-once: it is re-derived from the SDK's current cross-signing state and
invalidated when that changes (its derivation is wired up in a later subphase; it
defaults `false` for now).

There are no state-setter endpoints. `state` is a *consequence* of the lifecycle
verbs (`login` → `active`, `logout` → `deactivated`, re-`login` → `active`) plus
internal failure handling — never a value a client sets directly.

### Connection is gated on `state = active`

The single rule that replaces "any row with a decryptable token connects":

- `Store::list_accounts()` returns **only `active` rows** — "list accounts" means
  "the accounts you act on", so the sync engine's boot loop is safe by
  construction and a `deactivated`/`deleting` row gets no supervised task. Making
  the *unqualified* accessor the safe set (rather than "all rows") keeps the
  dangerous default off the table; surfacing other states is a separate,
  explicitly-named query added when a caller needs it (the read API showing a
  logged-out account; the teardown reconcile finding `deleting` rows).
- `ClientManager::get_or_connect` refuses a non-`active` account with
  `GatewayError::AccountNotActive`, so the **lazy gateway path** (an API send that
  arrives before sync) is gated too, not just boot. The gate lives at the single
  choke point both callers share — though only on a **cold** connect; an account
  already holding a cached client is not severed by a later state change here (see
  the cold-connect caveat under *Consequences*).

`AccountNotActive` maps to `403 Forbidden` at the API boundary (the composition-root
adapter, ADR 0021): the account exists, but sending through a logged-out/paused
account is not permitted and is not retryable without a login.

### Why a column + `CHECK`, not a Postgres `ENUM`

`state` is `TEXT` with a `CHECK (state IN (...))`, consistent with the rest of the
schema (e.g. `event_sender_device_keys.verification_state`). A Rust `AccountState`
enum (in `axon-store`, re-exported) gives the compile-time safety; an unknown
stored value surfaces as a column-decode error rather than a silent default. Adding
a future state is an additive migration, not a type alteration.

### Store-dir GC and `deleting` recovery (forward note)

Two related mechanisms land with the destructive `DELETE` verb in a later subphase,
recorded here so the state semantics are complete: a boot-time reconcile drives
`deleting` rows to completion, and an orphan-store-dir GC prunes `data_dir/<id>/`
dirs that have **no matching row in any state** — *not* "no active account", since a
`deactivated` row is real and may be reactivated. Keying GC off row existence rather
than lifecycle state is the load-bearing distinction (pruning by "not active" is the
#24 failure mode).

## The Milestone 7 breakdown (durable record of the plan)

M7 is delivered in three phases (the spec's subphases), each sliced into PRs of
~500–1500 LOC. Phase order is fixed: **7a → 7b → 7c**. Within 7a the interactive
SAS verification flow lands **last** — the lifecycle CRUD is independently useful
and lower-risk, the recovery-key path already provides device verification
meanwhile, and SAS is easier to build on a stable lifecycle foundation. Until 7b
ships, the destructive/secret-bearing 7a endpoints are bound to loopback only.

**7a — Homeserver account lifecycle & verification**
1. **Account state machine (store + sync gating)** — *this PR / this ADR.* The
   `state`/`verified` columns, `AccountState`, active-only `list_accounts`,
   `set_account_state`, and the `active`-gated connection.
2. Account read API (`GET /v1/accounts[/{id}]`) + runtime `POST /v1/accounts/login`
   (+ per-account lifecycle lock, loopback binding).
3. `logout` (non-destructive stop → `deactivated`, archive retained).
4. `DELETE` teardown (ordered, crash-safe) + boot reconcile of `deleting` rows +
   orphan-store-dir GC.
5. `recover` endpoint + real verification-status derivation.
6. Interactive SAS verification (`verify` flow over `/v1/ws`, bidirectional;
   timeout/mismatch/reconnect). Closes the verification deferred from M5 ("5c").

**7b — Client ↔ axon bearer-token auth** (formerly M8)
1. `tokens` table + `axon token issue/list/revoke` CLI.
2. Bearer-auth middleware on every `/v1/` route (HTTP + WS); lifts the 7a loopback
   restriction.

**7c — Sender-device trust & content authentication**
1. Evaluate + persist a per-event `sender_trust` snapshot at decryption; expose it
   on timeline reads and `/v1/ws`.
2. Verification-bundle endpoint (durable snapshot + live cross-signing lookup).
3. `verification_violation` overlay on a later sender-identity change (snapshot
   stays immutable).

M7 **as a whole** closes GH #14 (stale-DB cleanup) and #24 (lifecycle /
active-account gating / runtime provisioning). The active-account gate in *this*
PR is the foundation; the stale-row reconcile + orphan-dir GC that actually retire
the rows behind #24 land in **7a-4** (see the groundwork caveats under
*Consequences*).

## Consequences

- **Pro:** this lays the load-bearing groundwork for closing #24: a single,
  durable source of truth (`state`) for liveness that survives restarts and is
  crash-recoverable (`deleting`), plus a `state = active` gate at the **cold-connect**
  choke point both callers share (boot loop + lazy gateway). `verified` being
  orthogonal lets the API report key-acquisition state without conflating it with
  liveness.
- **Groundwork, not yet the fix (#24):** this PR does *not* on its own close #24,
  and the gate has two gaps that the later 7a verbs fill — stated plainly so the
  claim isn't over-read:
  - **Nothing deactivates a stale row yet.** Boot only `upsert_account`s the
    current provision; an account dropped from config keeps its `active` row, so
    the `state = active` filter still returns it and it still syncs/sends. The
    `state` column only helps once a verb (`logout`, `DELETE`) or the boot reconcile
    has actually moved a row out of `active` — the reconcile + orphan-dir GC that
    structurally retire stale rows land in **7a-4**.
  - **The gate is cold-connect only.** `get_or_connect` returns an already-cached
    client before it rereads `state`, so the gate *alone* doesn't sever a
    *connected* account on a state change. Severing a live account is instead done
    actively by the lifecycle verbs: **`logout` (landed in 7a-3)** flips the row to
    `deactivated` *first* (so the gate refuses any new connect from that point on),
    then cancels that account's supervised task and **awaits its drain** (the
    per-account cancellation + join handles introduced there — cancellation is
    cooperative, and the task holds the account's SDK store dir until the drain
    finishes, so a re-login must not restage it earlier; an overrunning drain is
    escalated to an abort, and a task that survives even that fails the verb
    with the task left registered, which `login` refuses until a retry reaps
    it — the quiescent-store postcondition is never traded for a return), then
    takes the cached
    client out of its slot and best-effort invalidates the device token upstream;
    `DELETE` (7a-4) reuses the same mechanism. Flip-then-take closes the
    logout-side reconnect race (a connect that read `active` pre-flip has its
    freshly cached client taken right back out — `take` serializes on the slot
    lock); the only residual is the login-side microseconds between caching a
    freshly-logged-in client and that row's flip *to* `active`, so the gate's own
    structural invariant stays the narrower "a non-`active` row gets no *new*
    client".
- **Con:** the broad "every row regardless of state" set is intentionally *not*
  exposed — `list_accounts` is the active-only connect/boot default. Each caller
  that needs a wider view gets its **own** explicitly-named accessor rather than a
  shared "all rows" method that would be a footgun on the connect path: the read
  API uses `list_client_visible_accounts` (active + deactivated, so a logged-out
  account is discoverable; landed in 7a-2), and the teardown reconcile + orphan-dir
  GC will add their own (`deleting`-only / by-existence) when they land in 7a-4.
- **Scope (this PR):** schema + store + the gate only. No new HTTP surface; the
  lifecycle verbs, the lock, reconcile/GC, recovery, and SAS are the later 7a PRs
  above. `verified` is persisted but its derivation is a stub (defaults `false`)
  until subphase 5.
- **Out of scope (M7):** `store_key` rotation stays deferred (ADR 0008), and
  per-account *authorization* scoping remains a non-goal — one human owns all
  their accounts.

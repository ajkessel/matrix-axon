//! The background indexing actor: the sole owner of the Tantivy
//! [`IndexWriter`](tantivy::IndexWriter).
//!
//! The actor turns the durable `search_outbox` change log into Tantivy writes
//! (ADR 0039). It is the single point that ever writes the index, so there is
//! never writer contention. Its loop is:
//!
//! 1. **Seed** the corpus from the event store *iff* the physical index is fresh
//!    (a brand-new, emptied, schema-bumped, or partially-seeded directory — see
//!    [`SearchIndex::open`](crate::SearchIndex::open)). The rebuild source of
//!    truth is `events`, never the outbox. Only after the seed completes does the
//!    actor stamp the seed-completion marker, so an interrupted seed reseeds on the
//!    next boot instead of being trusted as complete.
//! 2. **Drain** the outbox: apply each work item to Tantivy in `seq` order,
//!    commit a batch, then advance the durable cursor and prune. The cursor only
//!    moves after a commit, so it is always a contiguous complete prefix.
//! 3. **Wait** for a wakeup hint ([`IndexHandle::notify`]), a periodic safety
//!    tick, or a [`flush`](IndexHandle::flush) request — then drain again.
//!
//! Correctness rests on the durable outbox + cursor, never on the in-memory
//! channel: a dropped `notify` costs nothing because the obligation is still on
//! disk and the next drain (tick, later notify, or restart) applies it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use axon_store::{IndexableEvent, SearchOutboxEntry, Store, TimelineRow};
use tantivy::IndexWriter;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::schema::SearchSchema;
use crate::SearchError;

/// Tuning for the indexing actor. Defaults come from `SearchConfig` at the server
/// boundary.
#[derive(Debug, Clone)]
pub struct IndexerOptions {
    /// Bounded capacity of the wakeup-hint channel. Wakeups coalesce, so a small
    /// capacity is fine; a full channel just drops a redundant hint.
    pub channel_capacity: usize,
    /// Rows per batch for both the corpus seed and the outbox drain. Bounds the
    /// work (and the held connection) per database round-trip.
    pub batch_size: i64,
    /// How often to drain even without a wakeup — a safety net against a dropped
    /// hint, and the steady-state poll when notifications are quiet.
    pub idle_poll_interval: Duration,
    /// Tantivy writer heap budget, in megabytes.
    pub writer_heap_mb: usize,
    /// Delay between corpus-seed batches (a throttle so a large seed doesn't
    /// starve live work).
    pub seed_throttle: Duration,
}

impl IndexerOptions {
    /// The writer heap budget in bytes, floored at Tantivy's practical minimum.
    pub(crate) fn writer_heap_bytes(&self) -> usize {
        (self.writer_heap_mb * 1024 * 1024).max(15_000_000)
    }
}

/// A signal to the actor over the wakeup-hint channel.
enum Signal {
    /// "There may be new outbox work" — best-effort, coalescing, droppable.
    Wake,
    /// "Drain to empty and tell me when you have" — used by account deletion to
    /// close the privacy window synchronously when the actor is live.
    Flush(oneshot::Sender<()>),
}

/// The producer side of the actor's wakeup channel. Cheaply cloneable; hand a
/// clone to every ingestion site. Dropping all clones closes the channel and lets
/// the actor do a final drain and exit.
#[derive(Debug, Clone)]
pub struct IndexHandle {
    tx: mpsc::Sender<Signal>,
}

impl IndexHandle {
    /// Hint that new outbox work may exist. **Best-effort and non-blocking**: the
    /// durable obligation is already in `search_outbox` (written transactionally
    /// with the event), so a dropped hint only delays indexing to the next drain —
    /// it never loses work. Never blocks or fails the caller's ingestion path.
    pub fn notify(&self) {
        let _ = self.tx.try_send(Signal::Wake);
    }

    /// Drain the outbox to empty and return only once it has been applied and
    /// committed. Account deletion calls this after the purge obligation is written
    /// (transactionally with the row delete) so a deleted account's documents are
    /// gone before the verb returns — closing the privacy window when the actor is
    /// live. If the actor is absent/stopped the durable obligation still heals it
    /// on the next enabled boot, so this returning early is safe.
    pub async fn flush(&self) {
        let (ack, done) = oneshot::channel();
        if self.tx.send(Signal::Flush(ack)).await.is_ok() {
            let _ = done.await;
        }
    }
}

/// What [`SearchIndex::spawn_indexer`](crate::SearchIndex::spawn_indexer) returns:
/// the producer handle and the actor's join handle.
pub struct IndexerHandles {
    /// Clone and distribute to ingestion sites.
    pub handle: IndexHandle,
    /// Resolves when the actor has done its final drain after the channel closed.
    pub join: JoinHandle<()>,
}

/// Create the channel and spawn the actor task. `fresh` requests a one-time
/// corpus seed (the physical index is new/empty/schema-bumped/partially-seeded);
/// `dir` is the index directory, stamped with the seed-completion marker once the
/// seed succeeds.
pub(crate) fn spawn(
    writer: IndexWriter,
    schema: SearchSchema,
    store: Store,
    dir: PathBuf,
    fresh: bool,
    opts: IndexerOptions,
) -> IndexerHandles {
    let (tx, rx) = mpsc::channel(opts.channel_capacity);
    let join = tokio::spawn(run(writer, schema, store, dir, rx, fresh, opts));
    IndexerHandles {
        handle: IndexHandle { tx },
        join,
    }
}

/// The actor entry point: optional seed, then the drain/wait loop.
async fn run(
    mut writer: IndexWriter,
    schema: SearchSchema,
    store: Store,
    dir: PathBuf,
    mut rx: mpsc::Receiver<Signal>,
    fresh: bool,
    opts: IndexerOptions,
) {
    if fresh {
        match seed(&mut writer, &schema, &store, &opts).await {
            // Stamp the seed-completion marker only now that the corpus is durably
            // committed, so the next boot trusts (and reopens) this index instead of
            // reseeding. A marker-write failure is non-fatal: it just means the next
            // boot reseeds (wasteful but correct), so we log and keep serving.
            Ok(()) => {
                if let Err(err) = crate::index::write_seed_marker(&dir) {
                    tracing::warn!(error = %err, "search seed-completion marker write failed; index will reseed on next boot");
                }
            }
            // Leave the index unmarked so the next open reseeds. Draining the outbox
            // onto a partial/empty index would serve permanently incomplete results
            // this session, so stop instead — the degraded state is honest (empty)
            // and self-heals on restart.
            Err(err) => {
                tracing::warn!(error = %err, "search corpus seed failed; leaving index unmarked to reseed on next boot");
                return;
            }
        }
    }

    let mut ticker = tokio::time::interval(opts.idle_poll_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // skip the immediate first tick

    loop {
        if let Err(err) = drain_all(&mut writer, &schema, &store, &opts).await {
            tracing::warn!(error = %err, "search outbox drain failed; will retry");
        }
        tokio::select! {
            sig = rx.recv() => match sig {
                None => break, // all handles dropped → shut down
                Some(Signal::Wake) => {}
                Some(Signal::Flush(ack)) => {
                    if let Err(err) = drain_all(&mut writer, &schema, &store, &opts).await {
                        tracing::warn!(error = %err, "search outbox flush drain failed");
                    }
                    let _ = ack.send(());
                }
            },
            _ = ticker.tick() => {}
        }
    }

    // A row may have been written between the last drain and the channel closing.
    if let Err(err) = drain_all(&mut writer, &schema, &store, &opts).await {
        tracing::warn!(error = %err, "search outbox final drain failed");
    }
}

/// Drain the outbox to empty, one batch at a time. Each batch is applied to the
/// writer, committed, and only then has its `seq` recorded as the durable cursor
/// and its rows pruned — so a crash mid-batch re-runs the batch (idempotent) and
/// the cursor never claims uncommitted work.
async fn drain_all(
    writer: &mut IndexWriter,
    schema: &SearchSchema,
    store: &Store,
    opts: &IndexerOptions,
) -> Result<(), SearchError> {
    loop {
        let cursor = store.search_outbox_cursor().await?;
        let batch = store.drain_search_outbox(cursor, opts.batch_size).await?;
        let Some(last_seq) = batch.last().map(|e| e.seq) else {
            break; // outbox drained
        };
        apply_batch(writer, schema, store, &batch).await?;
        writer.commit()?;
        store.set_search_outbox_cursor(last_seq).await?;
        store.prune_search_outbox(last_seq).await?;
        if (batch.len() as i64) < opts.batch_size {
            break;
        }
    }
    Ok(())
}

/// Apply one drained batch to the writer (no commit). Re-derivations dedup within
/// the batch — an event edited several times enqueues its target repeatedly, but
/// the resolved projection is the same, so we resolve each `(account, event)` once.
/// A purge is never deduped: it removes every document for the account.
///
/// A failed store read aborts the whole batch (propagated) so the cursor does not
/// advance past unindexed work — the batch is retried rather than silently lost.
async fn apply_batch(
    writer: &IndexWriter,
    schema: &SearchSchema,
    store: &Store,
    batch: &[SearchOutboxEntry],
) -> Result<(), SearchError> {
    let mut seen: HashSet<(Uuid, &str)> = HashSet::new();
    for entry in batch {
        if entry.is_purge() {
            schema.delete_account(writer, entry.account_id);
        } else if seen.insert((entry.account_id, entry.event_id.as_str())) {
            apply_event(writer, schema, store, entry.account_id, &entry.event_id).await?;
        }
    }
    Ok(())
}

/// (Re)derive a single `(account_id, event_id)` document from the resolved M8
/// projection and upsert or delete it. Order-independent: a target not (yet)
/// stored, redacted, a standalone relation, or non-text resolves to "no document",
/// so a relation/redaction that lands before its target self-heals once the target
/// arrives and re-enqueues.
async fn apply_event(
    writer: &IndexWriter,
    schema: &SearchSchema,
    store: &Store,
    account_id: Uuid,
    event_id: &str,
) -> Result<(), SearchError> {
    match store.get_event(account_id, event_id).await? {
        Some(row) => match indexable(account_id, row) {
            Some(ev) => schema.upsert(writer, &ev)?,
            None => schema.delete(writer, account_id, event_id),
        },
        None => schema.delete(writer, account_id, event_id),
    }
    Ok(())
}

/// Seed the index from the event store — the authoritative rebuild. Clears the
/// (fresh) index, then streams the corpus in keyset batches through the same
/// resolved projection the live path derives from, so a from-scratch rebuild
/// converges with incremental indexing. Throttled between batches. Returning `Ok`
/// is the actor's signal to stamp the seed-completion marker; an `Err` (or a crash)
/// leaves the index unmarked so the next boot reseeds rather than trusting a partial
/// index.
///
/// Leaves the durable cursor at the outbox high-water mark captured **before** the
/// seed: everything committed by then is reflected in the corpus scan that runs
/// after it, so the drain need only apply changes made since. (A change whose
/// transaction is in flight at that instant and that the scan races past is the one
/// bounded, self-healing window — the next edit/redaction/redecryption to that
/// event re-enqueues it, and `axon search reindex` forces a clean rebuild.)
async fn seed(
    writer: &mut IndexWriter,
    schema: &SearchSchema,
    store: &Store,
    opts: &IndexerOptions,
) -> Result<(), SearchError> {
    let watermark = store.search_outbox_high_water().await?;

    tracing::info!("seeding search index from event store");
    writer.delete_all_documents()?;
    writer.commit()?;

    let mut after_id = 0i64;
    let mut total = 0usize;
    loop {
        let batch = store.events_for_index(after_id, opts.batch_size).await?;
        let Some(last) = batch.last() else { break };
        after_id = last.id;
        let n = batch.len();
        for ev in &batch {
            if let Err(err) = schema.add(writer, ev) {
                tracing::warn!(event_id = %ev.event_id, error = %err, "search index add failed during seed");
            }
        }
        writer.commit()?;
        total += n;
        if (n as i64) < opts.batch_size {
            break;
        }
        if !opts.seed_throttle.is_zero() {
            tokio::time::sleep(opts.seed_throttle).await;
        }
    }

    store.set_search_outbox_cursor(watermark).await?;
    store.prune_search_outbox(watermark).await?;
    tracing::info!(
        indexed = total,
        cursor = watermark,
        "search index seed complete"
    );
    Ok(())
}

/// Resolve a timeline row into the document to index, or `None` if the event
/// should have **no** document of its own:
///
/// * standalone relation events (`m.replace` edits, `m.reaction` annotations) —
///   their text is folded into the target's document, never indexed alone;
/// * redacted events (the projection masks the body to `None`) — unsearchable;
/// * non-text events (no body) and empty bodies.
///
/// This is the live-path mirror of the seed's filter in `Store::events_for_index`
/// and of the outbox fan-out's reply exclusion. All three must stay in lockstep so
/// a from-scratch rebuild converges with incremental indexing; change them
/// together.
fn indexable(account_id: Uuid, row: TimelineRow) -> Option<IndexableEvent> {
    if is_standalone_relation(&row) {
        return None;
    }
    let body = row.decrypted_body_text.filter(|b| !b.is_empty())?;
    Some(IndexableEvent {
        id: row.id,
        account_id,
        event_id: row.event_id,
        room_id: row.room_id,
        sender: row.sender,
        origin_ts: row.origin_ts,
        body,
    })
}

/// Whether `row` is a standalone relation whose body must not be indexed on its
/// own row: an `m.replace` edit, or an `m.reaction` annotation.
fn is_standalone_relation(row: &TimelineRow) -> bool {
    let rel_type = row
        .relates_to
        .as_ref()
        .and_then(|v| v.get("rel_type"))
        .and_then(|v| v.as_str());
    matches!(rel_type, Some("m.replace"))
        || (row.event_type == "m.reaction" && matches!(rel_type, Some("m.annotation")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(
        event_type: &str,
        relates_to: Option<serde_json::Value>,
        body: Option<&str>,
    ) -> TimelineRow {
        TimelineRow {
            id: 1,
            event_id: "$e".to_owned(),
            room_id: "!r".to_owned(),
            sender: "@u:x".to_owned(),
            state_key: None,
            origin_ts: 1,
            event_type: event_type.to_owned(),
            content: None,
            decrypted_body_text: body.map(str::to_owned),
            relates_to,
            redacts: None,
            redaction_event_id: None,
            sender_trust: None,
            edited: false,
            edit_count: 0,
            latest_edit_ts: None,
            reactions: None,
        }
    }

    #[test]
    fn plain_message_is_indexable() {
        let acct = Uuid::new_v4();
        let got = indexable(acct, row("m.room.message", None, Some("hello")));
        assert_eq!(got.map(|e| e.body), Some("hello".to_owned()));
    }

    #[test]
    fn edit_event_is_not_indexable_on_its_own() {
        // A standalone m.replace: its body is folded into the target, never indexed.
        let acct = Uuid::new_v4();
        let rel = json!({ "rel_type": "m.replace", "event_id": "$t" });
        assert!(indexable(acct, row("m.room.message", Some(rel), Some("* edited"))).is_none());
    }

    #[test]
    fn reaction_event_is_not_indexable() {
        let acct = Uuid::new_v4();
        let rel = json!({ "rel_type": "m.annotation", "event_id": "$t", "key": "👍" });
        assert!(indexable(acct, row("m.reaction", Some(rel), None)).is_none());
    }

    #[test]
    fn redacted_or_bodyless_is_not_indexable() {
        let acct = Uuid::new_v4();
        // Redaction masks the body to None in the projection → not indexable.
        assert!(indexable(acct, row("m.room.message", None, None)).is_none());
        // Empty body → not indexable.
        assert!(indexable(acct, row("m.room.message", None, Some(""))).is_none());
    }

    #[test]
    fn reply_is_indexable() {
        // A reply nests m.in_reply_to (no top-level rel_type) and has its own body.
        let acct = Uuid::new_v4();
        let rel = json!({ "m.in_reply_to": { "event_id": "$t" } });
        let got = indexable(acct, row("m.room.message", Some(rel), Some("a reply")));
        assert_eq!(got.map(|e| e.body), Some("a reply".to_owned()));
    }
}

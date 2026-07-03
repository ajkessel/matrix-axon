//! Boot-time reconciliation for the account-delete teardown (ADR 0024).
//!
//! Account deletion is an ordered, crash-recoverable sequence whose durable
//! breadcrumb is the row's `deleting` state and whose external side effects (the
//! on-disk SDK store dir) outlive a single process. Two sweeps at startup make a
//! crash mid-teardown converge:
//!
//! - [`reconcile_deleting`] re-finds any row a crash left in `deleting` and drives
//!   the *same* [`AccountLifecycle::delete`](crate::AccountLifecycle::delete) to
//!   completion — the teardown is idempotent, so resuming it is just re-running it.
//! - [`prune_orphan_store_dirs`] removes store dirs under `data_dir/` whose
//!   `<uuid>` matches **no** account row in any state — the backstop for a dir
//!   whose row vanished in an older, pre-`deleting`-marker world (GH #14/#24).
//!
//! Both run inside [`SyncEngine::start`](crate::SyncEngine::start), before the
//! account-spawn loop and before the HTTP listener binds, so neither races API
//! traffic or a supervised task creating a fresh store dir.

use std::collections::HashSet;

use axon_core::SyncConfig;
use axon_media::MediaCacheHandle;
use axon_store::Store;
use uuid::Uuid;

use crate::lifecycle::AccountLifecycle;

/// Drive every account left in `deleting` to completion via the runtime delete
/// verb. Resilient by construction: each row is independent, and a row that fails
/// to complete (e.g. a store error, or a — at boot, unexpected — `Draining`) is
/// logged and left `deleting` for the next boot to retry. Never returns an error:
/// a failed reconcile must not block the engine from bringing the healthy
/// accounts online.
pub(crate) async fn reconcile_deleting(lifecycle: &AccountLifecycle, store: &Store) {
    let rows = match store.list_deleting_accounts().await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, "reconcile: listing deleting accounts failed; skipping");
            return;
        }
    };
    if rows.is_empty() {
        return;
    }
    tracing::info!(
        count = rows.len(),
        "reconcile: completing interrupted account deletion(s)"
    );
    for acct in rows {
        match lifecycle.delete(acct.account_id).await {
            Ok(()) => {
                tracing::info!(account_id = %acct.account_id, "reconcile: account deletion completed")
            }
            Err(err) => tracing::error!(
                account_id = %acct.account_id,
                error = %err,
                "reconcile: account deletion did not complete; will retry next boot"
            ),
        }
    }
}

/// Prune orphan SDK store dirs under `data_dir`: a `<uuid>/` (or its staging
/// backup `<uuid>.prev`) whose `uuid` matches **no** `accounts` row in any state.
///
/// Keyed off row *existence*, never lifecycle state — a `deactivated` row is real
/// and may be reactivated, so its dir must be kept; pruning by "not active" is the
/// #24 failure mode (ADR 0022/0024). Entries whose name is not a `<uuid>`/`<uuid>.prev`
/// are left strictly untouched (never our files), as is anything if the id lookup
/// fails. Never returns an error — a GC failure must not block boot.
pub(crate) async fn prune_orphan_store_dirs(config: &SyncConfig, store: &Store) {
    let known: HashSet<Uuid> = match store.list_all_account_ids().await {
        Ok(ids) => ids.into_iter().collect(),
        Err(err) => {
            tracing::error!(error = %err, "orphan-dir GC: listing account ids failed; skipping");
            return;
        }
    };

    let mut entries = match tokio::fs::read_dir(&config.data_dir).await {
        Ok(entries) => entries,
        // No data dir yet (no account has ever connected) — nothing to prune.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::error!(
                error = %err,
                dir = %config.data_dir.display(),
                "orphan-dir GC: reading data_dir failed; skipping"
            );
            return;
        }
    };

    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(err) => {
                tracing::error!(error = %err, "orphan-dir GC: reading data_dir entry failed; stopping");
                break;
            }
        };

        let name = entry.file_name();
        // Non-UTF-8 names can't be one of our `<uuid>` dirs — leave them.
        let Some(name) = name.to_str() else { continue };
        // A store dir is `<uuid>`; login's staging leaves a `<uuid>.prev` backup.
        let id_str = name.strip_suffix(".prev").unwrap_or(name);
        let Ok(id) = Uuid::parse_str(id_str) else {
            continue;
        };
        // A row exists for this id in some state — keep the dir (and its `.prev`).
        if known.contains(&id) {
            continue;
        }
        // Only ever remove directories; never touch a stray file that happens to
        // parse as a uuid.
        match entry.file_type().await {
            Ok(ft) if ft.is_dir() => {}
            Ok(_) => continue,
            Err(err) => {
                tracing::warn!(error = %err, entry = %entry.path().display(), "orphan-dir GC: stat failed; skipping");
                continue;
            }
        }

        let path = entry.path();
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => {
                tracing::info!(orphan = %path.display(), "orphan-dir GC: pruned row-less store dir")
            }
            Err(err) => tracing::warn!(
                error = %err,
                orphan = %path.display(),
                "orphan-dir GC: failed to prune store dir"
            ),
        }
    }
}

/// Prune orphan media-cache dirs: a `<uuid>/` under the media `cache_dir` whose
/// id matches **no** `accounts` row in any state (the M11 analogue of
/// [`prune_orphan_store_dirs`], ADR 0024 step 5). The backstop for a deletion
/// that happened while the media cache was disabled or a crash mid-purge. Keyed
/// off row *existence*, never lifecycle state, for the same reason. The scan and
/// stray-file discipline live in `axon-media`; never returns an error.
pub(crate) async fn prune_orphan_media_dirs(media: &MediaCacheHandle, store: &Store) {
    let known: HashSet<Uuid> = match store.list_all_account_ids().await {
        Ok(ids) => ids.into_iter().collect(),
        Err(err) => {
            tracing::error!(error = %err, "media orphan-GC: listing account ids failed; skipping");
            return;
        }
    };
    media.purge_orphans(&known).await;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use axon_store::AccountState;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use tokio_util::task::TaskTracker;

    use super::*;
    use crate::manager::ClientManager;

    async fn connect_store() -> Store {
        let url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
        Store::connect(&url, 5).await.expect("connect + migrate")
    }

    /// A unique temp `data_dir` per test so the orphan-GC sweeps are isolated.
    fn temp_data_dir() -> PathBuf {
        std::env::temp_dir().join(format!("axon-reconcile-test-{}", Uuid::new_v4()))
    }

    fn config(data_dir: PathBuf) -> SyncConfig {
        SyncConfig {
            data_dir,
            store_key: Some("test-key".to_owned()),
            account: None,
            timeline_limit: 1,
            live_event_buffer: 16,
            ..SyncConfig::default()
        }
    }

    async fn lifecycle(store: Store, config: SyncConfig) -> AccountLifecycle {
        let manager = ClientManager::new(store.clone(), config.clone());
        let (live_tx, _rx) = broadcast::channel(16);
        let media = axon_media::MediaCache::open(&axon_core::MediaConfig {
            enabled: true,
            cache_dir: std::env::temp_dir()
                .join(format!("axon-media-reconcile-{}", Uuid::new_v4())),
            max_bytes: 1 << 20,
            max_object_bytes: 1 << 20,
            fetch_timeout_secs: 30,
            max_concurrent_downloads: 8,
        })
        .await
        .expect("open media cache")
        .handle();
        AccountLifecycle::new(
            store,
            config,
            manager,
            live_tx,
            CancellationToken::new(),
            TaskTracker::new(),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            crate::verification::new_registry(),
            crate::verification::VerificationRooms::new(),
            None,
            media,
            crate::backfill::BackfillHealth::new(None),
        )
    }

    async fn delete_row(store: &Store, id: Uuid) {
        sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
            .bind(id)
            .execute(store.pool())
            .await
            .expect("cleanup");
    }

    /// The boot reconcile drives a row a crash left in `deleting` to full
    /// completion: the row is gone and its on-disk store dir removed.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn reconcile_completes_a_deleting_row() {
        let store = connect_store().await;
        let dir = temp_data_dir();
        let config = config(dir.clone());
        let lc = lifecycle(store.clone(), config.clone()).await;

        let user = format!("@reconcile-{}:localhost", Uuid::new_v4());
        let acct = store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();
        store
            .set_account_state(acct.account_id, AccountState::Deleting)
            .await
            .unwrap();
        let store_dir = dir.join(acct.account_id.to_string());
        std::fs::create_dir_all(&store_dir).unwrap();

        reconcile_deleting(&lc, &store).await;

        assert!(store.get_account(acct.account_id).await.unwrap().is_none());
        assert!(!store_dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Orphan-dir GC prunes a `<uuid>/` dir with no matching row, but keeps the
    /// dirs (and `.prev` backup) of *any* real row — including a `deactivated` one
    /// (the #24-safe distinction: keyed off row existence, not lifecycle state) —
    /// and never touches a non-`<uuid>` entry.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn orphan_gc_prunes_rowless_dirs_only() {
        let store = connect_store().await;
        let dir = temp_data_dir();
        std::fs::create_dir_all(&dir).unwrap();

        let user = format!("@gc-live-{}:localhost", Uuid::new_v4());
        let live = store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();
        let user2 = format!("@gc-deact-{}:localhost", Uuid::new_v4());
        let deact = store
            .upsert_account(&user2, "https://hs.example.org")
            .await
            .unwrap();
        store
            .set_account_state(deact.account_id, AccountState::Deactivated)
            .await
            .unwrap();

        let live_dir = dir.join(live.account_id.to_string());
        let live_prev = dir.join(format!("{}.prev", live.account_id));
        let deact_dir = dir.join(deact.account_id.to_string());
        let orphan_dir = dir.join(Uuid::new_v4().to_string()); // no row
        let stray_file = dir.join("not-a-uuid.txt");
        for d in [&live_dir, &live_prev, &deact_dir, &orphan_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(&stray_file, b"keep me").unwrap();

        prune_orphan_store_dirs(&config(dir.clone()), &store).await;

        assert!(live_dir.exists(), "live account dir kept");
        assert!(live_prev.exists(), "live account .prev kept");
        assert!(
            deact_dir.exists(),
            "deactivated account dir kept (real row)"
        );
        assert!(!orphan_dir.exists(), "row-less orphan dir pruned");
        assert!(stray_file.exists(), "non-uuid file untouched");

        delete_row(&store, live.account_id).await;
        delete_row(&store, deact.account_id).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Media orphan-GC (M11): prunes a `<uuid>/` cache dir with no account row,
    /// keeps one for a real (even deactivated) row, and never touches strays.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn media_orphan_gc_prunes_rowless_dirs_only() {
        let store = connect_store().await;
        let cache_dir = std::env::temp_dir().join(format!("axon-media-gc-{}", Uuid::new_v4()));
        let media = axon_media::MediaCache::open(&axon_core::MediaConfig {
            enabled: true,
            cache_dir: cache_dir.clone(),
            max_bytes: 1 << 20,
            max_object_bytes: 1 << 20,
            fetch_timeout_secs: 30,
            max_concurrent_downloads: 8,
        })
        .await
        .expect("open media cache");

        let user = format!("@media-gc-{}:localhost", Uuid::new_v4());
        let live = store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();

        let live_dir = cache_dir.join(live.account_id.to_string());
        let orphan_dir = cache_dir.join(Uuid::new_v4().to_string()); // no row
        let stray_file = cache_dir.join("not-a-uuid.txt");
        for d in [&live_dir, &orphan_dir] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("deadbeef"), b"cached").unwrap();
        }
        std::fs::write(&stray_file, b"keep me").unwrap();

        prune_orphan_media_dirs(&media.handle(), &store).await;

        assert!(live_dir.exists(), "live account media dir kept");
        assert!(!orphan_dir.exists(), "row-less orphan media dir pruned");
        assert!(stray_file.exists(), "non-uuid file untouched");
        assert!(cache_dir.join(".tmp").exists(), "temp dir untouched");

        delete_row(&store, live.account_id).await;
        let _ = std::fs::remove_dir_all(&cache_dir);
    }
}

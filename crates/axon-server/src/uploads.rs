//! Composition-root staged-upload service (M15a, ADR 0059).
//!
//! This adapter satisfies `axon-api`'s upload port by streaming request bytes to
//! a durable staging directory and recording the metadata row in `axon-store`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axon_api::{
    ClaimedUpload, StageUploadError, StageUploadRequest, StagedUpload, StagedUploadService,
    UploadStream,
};
use axon_core::MediaConfig;
use axon_store::{AccountState, MediaUploadKind, NewMediaUpload, Store};
use chrono::{TimeDelta, Utc};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;
use uuid::Uuid;

/// Filesystem-backed staged-upload implementation.
pub struct FilesystemStagedUploads {
    store: Store,
    root: PathBuf,
    max_upload_bytes: u64,
    upload_timeout: Duration,
    ttl: Duration,
    concurrent_uploads: Arc<Semaphore>,
    /// `config.max_concurrent_uploads`, kept alongside the semaphore purely for
    /// logging context — `Semaphore` doesn't expose its original capacity.
    concurrent_uploads_total: usize,
}

impl FilesystemStagedUploads {
    pub fn new(store: Store, config: &MediaConfig) -> anyhow::Result<Self> {
        anyhow::ensure!(
            config.max_concurrent_uploads > 0,
            "media.max_concurrent_uploads must be greater than zero"
        );
        anyhow::ensure!(
            config.staged_upload_ttl_secs > 0,
            "media.staged_upload_ttl_secs must be greater than zero"
        );
        Ok(Self {
            store,
            root: config.uploads_dir.clone(),
            max_upload_bytes: config.max_upload_bytes,
            upload_timeout: Duration::from_secs(config.upload_request_timeout_secs),
            ttl: Duration::from_secs(config.staged_upload_ttl_secs),
            concurrent_uploads: Arc::new(Semaphore::new(config.max_concurrent_uploads)),
            concurrent_uploads_total: config.max_concurrent_uploads,
        })
    }

    async fn stage_upload_inner(
        &self,
        request: StageUploadRequest,
        mut body: UploadStream,
    ) -> Result<StagedUpload, StageUploadError> {
        let account = self
            .store
            .get_account(request.account_id)
            .await
            .map_err(internal)?;
        let Some(account) = account else {
            return Err(StageUploadError::NotFound("account not found".to_owned()));
        };
        if account.state != AccountState::Active {
            return Err(StageUploadError::Forbidden(format!(
                "account {} is not active",
                request.account_id
            )));
        }

        let upload_id = Uuid::new_v4();
        let account_dir = self.root.join(request.account_id.to_string());
        tokio::fs::create_dir_all(&account_dir)
            .await
            .map_err(internal)?;
        let tmp_path = account_dir.join(format!(".{upload_id}.tmp"));
        let final_path = account_dir.join(upload_id.to_string());
        let mut cleanup = CleanupFileGuard::new(tmp_path.clone());
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .await
            .map_err(internal)?;

        let mut size = 0_u64;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|err| StageUploadError::Invalid(err.to_string()))?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or(StageUploadError::TooLarge {
                    cap: self.max_upload_bytes,
                })?;
            if size > self.max_upload_bytes {
                return Err(StageUploadError::TooLarge {
                    cap: self.max_upload_bytes,
                });
            }
            file.write_all(&chunk).await.map_err(internal)?;
        }
        file.sync_all().await.map_err(internal)?;
        drop(file);

        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .map_err(internal)?;
        cleanup.move_to(final_path.clone());
        let path = final_path_to_string(&final_path)?;
        let expires_at = Utc::now()
            + TimeDelta::from_std(self.ttl)
                .map_err(|err| StageUploadError::Internal(err.to_string()))?;
        let row = self
            .store
            .insert_media_upload(&NewMediaUpload {
                upload_id,
                account_id: request.account_id,
                kind: match request.kind {
                    axon_api::MediaUploadKindDto::Image => MediaUploadKind::Image,
                    axon_api::MediaUploadKindDto::File => MediaUploadKind::File,
                },
                filename: &request.filename,
                content_type: request.content_type.as_deref(),
                size_bytes: i64::try_from(size).map_err(|_| StageUploadError::TooLarge {
                    cap: self.max_upload_bytes,
                })?,
                path: &path,
                expires_at,
            })
            .await
            .map_err(internal)?;
        cleanup.disarm();

        Ok(StagedUpload {
            upload_id: row.upload_id,
            kind: request.kind,
            filename: row.filename,
            content_type: row.content_type,
            size_bytes: row.size_bytes as u64,
            expires_at: row.expires_at.to_rfc3339(),
        })
    }

    fn to_api_kind(kind: MediaUploadKind) -> axon_api::MediaUploadKindDto {
        match kind {
            MediaUploadKind::Image => axon_api::MediaUploadKindDto::Image,
            MediaUploadKind::File => axon_api::MediaUploadKindDto::File,
        }
    }
}

#[async_trait]
impl StagedUploadService for FilesystemStagedUploads {
    async fn stage_upload(
        &self,
        request: StageUploadRequest,
        body: UploadStream,
    ) -> Result<StagedUpload, StageUploadError> {
        let account_id = request.account_id;
        let queued_at = Instant::now();
        let available_permits = self.concurrent_uploads.available_permits();
        let _permit = self
            .concurrent_uploads
            .clone()
            .acquire_owned()
            .await
            .map_err(|err| StageUploadError::Internal(err.to_string()))?;
        let wait = queued_at.elapsed();
        // Anything more than trivial wait means every permit was in use — worth
        // knowing even on success, since it's the leading signal that uploads are
        // backing up (a slow/stuck upload elsewhere, or genuine load).
        if wait > Duration::from_millis(50) {
            tracing::warn!(
                account_id = %account_id,
                wait_ms = wait.as_millis(),
                available_permits_before_wait = available_permits,
                max_concurrent_uploads = self.concurrent_uploads_total,
                "stage_upload waited for a concurrent-upload permit"
            );
        }
        tracing::debug!(account_id = %account_id, filename = %request.filename, "stage_upload: permit acquired, starting write");
        let started_at = Instant::now();
        let result = tokio::time::timeout(self.upload_timeout, self.stage_upload_inner(request, body))
            .await
            .map_err(|_| {
                tracing::warn!(
                    account_id = %account_id,
                    elapsed_ms = started_at.elapsed().as_millis(),
                    timeout_secs = self.upload_timeout.as_secs(),
                    "stage_upload timed out — request never completed within upload_request_timeout_secs"
                );
                StageUploadError::Timeout(format!(
                    "upload timed out after {}s",
                    self.upload_timeout.as_secs()
                ))
            })?;
        match &result {
            Ok(staged) => tracing::info!(
                account_id = %account_id,
                upload_id = %staged.upload_id,
                size_bytes = staged.size_bytes,
                elapsed_ms = started_at.elapsed().as_millis(),
                "stage_upload completed"
            ),
            Err(err) => tracing::warn!(
                account_id = %account_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                error = ?err,
                "stage_upload failed"
            ),
        }
        result
    }

    async fn delete_upload(
        &self,
        account_id: Uuid,
        upload_id: Uuid,
    ) -> Result<(), StageUploadError> {
        let Some(row) = self
            .store
            .delete_staged_media_upload(account_id, upload_id)
            .await
            .map_err(internal)?
        else {
            return Err(StageUploadError::NotFound("upload not found".to_owned()));
        };
        remove_file_if_exists(Path::new(&row.path)).await;
        Ok(())
    }

    async fn claim_upload(
        &self,
        account_id: Uuid,
        upload_id: Uuid,
    ) -> Result<ClaimedUpload, StageUploadError> {
        let Some(row) = self
            .store
            .claim_staged_media_upload(account_id, upload_id)
            .await
            .map_err(internal)?
        else {
            return Err(StageUploadError::NotFound("upload not found".to_owned()));
        };
        let bytes = match tokio::fs::read(&row.path).await {
            Ok(bytes) => bytes,
            Err(err) => {
                if let Err(release_err) = self
                    .store
                    .release_sending_media_upload(account_id, upload_id)
                    .await
                {
                    tracing::warn!(
                        account_id = %account_id,
                        upload_id = %upload_id,
                        error = %release_err,
                        "failed to release upload after staged file read failed"
                    );
                }
                return if err.kind() == std::io::ErrorKind::NotFound {
                    Err(StageUploadError::NotFound(
                        "upload file not found".to_owned(),
                    ))
                } else {
                    Err(internal(err))
                };
            }
        };
        Ok(ClaimedUpload {
            upload_id: row.upload_id,
            kind: Self::to_api_kind(row.kind),
            filename: row.filename,
            content_type: row.content_type,
            size_bytes: row.size_bytes as u64,
            bytes,
        })
    }

    async fn complete_upload(
        &self,
        account_id: Uuid,
        upload_id: Uuid,
    ) -> Result<(), StageUploadError> {
        let Some(row) = self
            .store
            .complete_sending_media_upload(account_id, upload_id)
            .await
            .map_err(internal)?
        else {
            return Err(StageUploadError::NotFound("upload not found".to_owned()));
        };
        remove_file_if_exists(Path::new(&row.path)).await;
        Ok(())
    }

    async fn release_upload(
        &self,
        account_id: Uuid,
        upload_id: Uuid,
    ) -> Result<(), StageUploadError> {
        let Some(_row) = self
            .store
            .release_sending_media_upload(account_id, upload_id)
            .await
            .map_err(internal)?
        else {
            return Err(StageUploadError::NotFound("upload not found".to_owned()));
        };
        Ok(())
    }
}

fn final_path_to_string(path: &Path) -> Result<String, StageUploadError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| StageUploadError::Internal("upload path is not valid UTF-8".to_owned()))
}

fn internal(err: impl std::fmt::Display) -> StageUploadError {
    StageUploadError::Internal(err.to_string())
}

struct CleanupFileGuard {
    path: PathBuf,
    armed: bool,
}

impl CleanupFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn move_to(&mut self, path: PathBuf) {
        self.path = path;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CleanupFileGuard {
    fn drop(&mut self) {
        if self.armed {
            schedule_remove_file(self.path.clone());
        }
    }
}

fn schedule_remove_file(path: PathBuf) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let _cleanup = handle.spawn_blocking(move || remove_file_if_exists_sync(&path));
        }
        Err(_) => {
            let _cleanup = std::thread::spawn(move || remove_file_if_exists_sync(&path));
        }
    };
}

fn remove_file_if_exists_sync(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "staged upload cleanup failed")
        }
    }
}

async fn remove_file_if_exists(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "staged upload cleanup failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;
    use std::time::Duration;

    use uuid::Uuid;

    use super::CleanupFileGuard;

    #[tokio::test]
    async fn cleanup_guard_removes_armed_file_on_drop() {
        let path = std::env::temp_dir().join(format!("axon-upload-guard-{}", Uuid::new_v4()));
        std::fs::File::create(&path)
            .expect("create temp")
            .write_all(b"partial")
            .expect("write temp");

        {
            let _guard = CleanupFileGuard::new(path.clone());
        }

        wait_for_removal(&path).await;
        assert!(!path.exists(), "armed guard removes partial staged file");
    }

    #[test]
    fn cleanup_guard_disarm_keeps_file() {
        let path = std::env::temp_dir().join(format!("axon-upload-guard-{}", Uuid::new_v4()));
        std::fs::File::create(&path)
            .expect("create temp")
            .write_all(b"complete")
            .expect("write temp");

        {
            let mut guard = CleanupFileGuard::new(path.clone());
            guard.disarm();
        }

        assert!(path.exists(), "disarmed guard leaves committed file");
        std::fs::remove_file(&path).expect("cleanup temp");
    }

    async fn wait_for_removal(path: &Path) {
        for _ in 0..20 {
            if !path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

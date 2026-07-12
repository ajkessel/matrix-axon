//! Metadata for staged client-originated media uploads (M15a, ADR 0059).
//!
//! The bytes live on disk under the configured upload staging directory; this
//! table is the durable index that lets Axon recover pending local mutations
//! after restart. Filesystem cleanup is owned by the composition-root staging
//! service, while the store keeps the account-scoped metadata transactional.

use chrono::{DateTime, Utc};
use sqlx_core::row::Row;
use sqlx_postgres::{PgRow, Postgres};
use uuid::Uuid;

use crate::{Store, StoreError};

/// Supported outbound media message shape for a staged upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaUploadKind {
    Image,
    File,
}

impl MediaUploadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaUploadKind::Image => "image",
            MediaUploadKind::File => "file",
        }
    }

    fn from_db(s: &str) -> Result<Self, sqlx_core::Error> {
        match s {
            "image" => Ok(MediaUploadKind::Image),
            "file" => Ok(MediaUploadKind::File),
            other => Err(sqlx_core::Error::ColumnDecode {
                index: "kind".to_owned(),
                source: format!("unknown media upload kind {other:?}").into(),
            }),
        }
    }
}

/// Durable state of a staged upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaUploadState {
    Staged,
    Sending,
}

impl MediaUploadState {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaUploadState::Staged => "staged",
            MediaUploadState::Sending => "sending",
        }
    }

    fn from_db(s: &str) -> Result<Self, sqlx_core::Error> {
        match s {
            "staged" => Ok(MediaUploadState::Staged),
            "sending" => Ok(MediaUploadState::Sending),
            other => Err(sqlx_core::Error::ColumnDecode {
                index: "state".to_owned(),
                source: format!("unknown media upload state {other:?}").into(),
            }),
        }
    }
}

/// New staged-upload metadata. The referenced file must already be durable on
/// disk before this row is inserted.
pub struct NewMediaUpload<'a> {
    pub upload_id: Uuid,
    pub account_id: Uuid,
    pub kind: MediaUploadKind,
    pub filename: &'a str,
    pub content_type: Option<&'a str>,
    pub size_bytes: i64,
    pub path: &'a str,
    pub expires_at: DateTime<Utc>,
}

/// One staged-upload metadata row.
#[derive(Debug, Clone)]
pub struct MediaUpload {
    pub upload_id: Uuid,
    pub account_id: Uuid,
    pub kind: MediaUploadKind,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: i64,
    pub path: String,
    pub state: MediaUploadState,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for MediaUpload {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        let kind: String = row.try_get("kind")?;
        let state: String = row.try_get("state")?;
        Ok(MediaUpload {
            upload_id: row.try_get("upload_id")?,
            account_id: row.try_get("account_id")?,
            kind: MediaUploadKind::from_db(&kind)?,
            filename: row.try_get("filename")?,
            content_type: row.try_get("content_type")?,
            size_bytes: row.try_get("size_bytes")?,
            path: row.try_get("path")?,
            state: MediaUploadState::from_db(&state)?,
            expires_at: row.try_get("expires_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

const MEDIA_UPLOAD_COLUMNS: &str = "upload_id, account_id, kind, filename, content_type, \
    size_bytes, path, state, expires_at, created_at, updated_at";

impl Store {
    /// Insert a newly staged upload. The filesystem service writes and renames
    /// the file first; this row is the durable index that makes it discoverable.
    pub async fn insert_media_upload(
        &self,
        upload: &NewMediaUpload<'_>,
    ) -> Result<MediaUpload, StoreError> {
        let sql = format!(
            "INSERT INTO media_uploads \
               (upload_id, account_id, kind, filename, content_type, size_bytes, path, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             RETURNING {MEDIA_UPLOAD_COLUMNS}"
        );
        let row = sqlx_core::query_as::query_as::<Postgres, MediaUpload>(&sql)
            .bind(upload.upload_id)
            .bind(upload.account_id)
            .bind(upload.kind.as_str())
            .bind(upload.filename)
            .bind(upload.content_type)
            .bind(upload.size_bytes)
            .bind(upload.path)
            .bind(upload.expires_at)
            .fetch_one(&self.pool)
            .await?;
        Ok(row)
    }

    /// Fetch an account-scoped upload by id.
    pub async fn get_media_upload(
        &self,
        account_id: Uuid,
        upload_id: Uuid,
    ) -> Result<Option<MediaUpload>, StoreError> {
        let sql = format!(
            "SELECT {MEDIA_UPLOAD_COLUMNS} FROM media_uploads \
             WHERE account_id = $1 AND upload_id = $2"
        );
        let row = sqlx_core::query_as::query_as::<Postgres, MediaUpload>(&sql)
            .bind(account_id)
            .bind(upload_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    /// Delete a staged upload row, returning its metadata so the caller can
    /// unlink the file. `sending` rows are deliberately not deleted here; M15b
    /// will own that transition and consume-on-success behavior.
    pub async fn delete_staged_media_upload(
        &self,
        account_id: Uuid,
        upload_id: Uuid,
    ) -> Result<Option<MediaUpload>, StoreError> {
        let sql = format!(
            "DELETE FROM media_uploads \
             WHERE account_id = $1 AND upload_id = $2 AND state = 'staged' \
             RETURNING {MEDIA_UPLOAD_COLUMNS}"
        );
        let row = sqlx_core::query_as::query_as::<Postgres, MediaUpload>(&sql)
            .bind(account_id)
            .bind(upload_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }
}

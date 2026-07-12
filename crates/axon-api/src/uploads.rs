//! Port for staged media uploads (M15a, ADR 0059).
//!
//! The API crate owns the capability it needs: accept a bounded stream of bytes,
//! persist normalized metadata, and later delete an unsent staged upload. The
//! concrete filesystem + store implementation is wired in by `axon-server`.

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use uuid::Uuid;

use crate::dto::MediaUploadKindDto;

/// Raw upload request body as a fallible byte stream.
pub type UploadStream = Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send + 'static>>;

/// Metadata needed to stage one media upload.
#[derive(Debug, Clone)]
pub struct StageUploadRequest {
    pub account_id: Uuid,
    pub kind: MediaUploadKindDto,
    pub filename: String,
    pub content_type: Option<String>,
}

/// Normalized metadata returned after a successful stage operation.
#[derive(Debug, Clone)]
pub struct StagedUpload {
    pub upload_id: Uuid,
    pub kind: MediaUploadKindDto,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: u64,
    pub expires_at: String,
}

/// What can go wrong while staging or deleting upload bytes.
#[derive(Debug)]
pub enum StageUploadError {
    /// Malformed request metadata or body stream. -> `400`.
    Invalid(String),
    /// The account or upload does not exist. -> `404`.
    NotFound(String),
    /// The account exists but cannot accept mutations. -> `403`.
    Forbidden(String),
    /// The body exceeded the configured cap. -> `413`.
    TooLarge { cap: u64 },
    /// The client upload timed out. -> `503`.
    Timeout(String),
    /// Local filesystem/database failure. Logged and returned as a generic `500`.
    Internal(String),
}

/// Stages and deletes client-originated upload bytes.
#[async_trait]
pub trait StagedUploadService: Send + Sync {
    async fn stage_upload(
        &self,
        request: StageUploadRequest,
        body: UploadStream,
    ) -> Result<StagedUpload, StageUploadError>;

    async fn delete_upload(
        &self,
        account_id: Uuid,
        upload_id: Uuid,
    ) -> Result<(), StageUploadError>;
}

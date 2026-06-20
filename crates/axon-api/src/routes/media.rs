//! Media proxy endpoint: `GET /v1/media/{account_id}/{server_name}/{media_id}`.
//!
//! The handler reconstructs the `mxc://` URI from the path components and
//! delegates the authenticated download to the injected [`MediaProxy`]. It
//! returns raw bytes (not the `{data}` JSON envelope) because the payload is
//! binary media, not structured data.

use std::sync::Arc;

use axon_store::Store;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use uuid::Uuid;

use crate::media::{MediaError, MediaProxy};
use crate::response::ApiError;

/// Return the encrypted-file descriptor whose URL matches the requested MXC.
///
/// Primary encrypted attachments use `content.file`; encrypted thumbnails use
/// `content.info.thumbnail_file`. Plain media and non-matching descriptors
/// return `None`.
fn encrypted_file_for_mxc(content: &Value, mxc_url: &str) -> Option<Value> {
    let candidates = [
        content.get("file"),
        content
            .get("info")
            .and_then(|info| info.get("thumbnail_file")),
    ];

    candidates.into_iter().flatten().find_map(|file| {
        (file.get("url").and_then(Value::as_str) == Some(mxc_url)).then(|| file.clone())
    })
}

/// Proxy an `mxc://` download through the account's homeserver connection.
///
/// The `server_name` and `media_id` path segments form the `mxc://` URI that
/// was embedded in a Matrix event's primary or thumbnail media descriptor.
/// The response body is the raw media bytes. `Content-Type` is always
/// `application/octet-stream` for now (the matrix-rust-sdk media API does not
/// surface the upstream header); callers should sniff magic bytes to determine
/// the actual media type.
#[utoipa::path(
    get,
    path = "/v1/media/{account_id}/{server_name}/{media_id}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account whose credentials are used for the download"),
        ("server_name" = String, Path, description = "Server-name component of the MXC URI (the part after `mxc://`)"),
        ("media_id" = String, Path, description = "Media-ID component of the MXC URI"),
    ),
    responses(
        (status = 200, description = "Media bytes; Content-Type is always `application/octet-stream` (sniff magic bytes for actual type)"),
        (status = 400, description = "Syntactically invalid MXC URI components", body = crate::response::ErrorResponse),
        (status = 404, description = "Account not found, or media not found on the homeserver", body = crate::response::ErrorResponse),
        (status = 500, description = "Internal media-metadata lookup failure", body = crate::response::ErrorResponse),
        (status = 502, description = "The homeserver was unreachable or returned an error", body = crate::response::ErrorResponse),
    ),
    tag = "media",
)]
pub async fn get_media(
    State(proxy): State<Arc<dyn MediaProxy>>,
    State(store): State<Store>,
    Path((account_id, server_name, media_id)): Path<(Uuid, String, String)>,
) -> Result<Response, ApiError> {
    let mxc_url = format!("mxc://{server_name}/{media_id}");

    // Fail closed: if no event row is found we cannot determine whether the
    // media is encrypted. Serving ciphertext as plain bytes under 200 is worse
    // than a 404. Also fail when content is NULL — a redacted or not-yet-
    // decrypted (UTD) event — for the same reason.
    let event = store
        .get_event_by_mxc_url(account_id, &mxc_url)
        .await?
        .ok_or_else(|| ApiError::from(MediaError::NotFound(mxc_url.clone())))?;

    let content = event.content.ok_or_else(|| {
        ApiError::from(MediaError::NotFound(format!(
            "media unavailable (redacted or not yet decrypted): {mxc_url}"
        )))
    })?;

    let encrypted_file = encrypted_file_for_mxc(&content, &mxc_url);

    let content = proxy
        .get_media(account_id, &mxc_url, encrypted_file)
        .await?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content.content_type)],
        content.data,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use serde_json::json;

    use super::encrypted_file_for_mxc;
    use crate::media::MediaError;

    #[test]
    fn selects_primary_encrypted_file_by_url() {
        let content = json!({
            "file": { "url": "mxc://example.org/main", "key": "main" },
            "info": {
                "thumbnail_file": {
                    "url": "mxc://example.org/thumb",
                    "key": "thumb"
                }
            }
        });

        assert_eq!(
            encrypted_file_for_mxc(&content, "mxc://example.org/main")
                .and_then(|file| file.get("key").cloned()),
            Some(json!("main"))
        );
    }

    #[test]
    fn selects_encrypted_thumbnail_by_url() {
        let content = json!({
            "file": { "url": "mxc://example.org/main", "key": "main" },
            "info": {
                "thumbnail_file": {
                    "url": "mxc://example.org/thumb",
                    "key": "thumb"
                }
            }
        });

        assert_eq!(
            encrypted_file_for_mxc(&content, "mxc://example.org/thumb")
                .and_then(|file| file.get("key").cloned()),
            Some(json!("thumb"))
        );
    }

    #[test]
    fn ignores_nonmatching_encrypted_descriptors() {
        let content = json!({
            "file": { "url": "mxc://example.org/other" },
            "info": { "thumbnail_url": "mxc://example.org/plain-thumb" }
        });

        assert!(encrypted_file_for_mxc(&content, "mxc://example.org/plain-thumb").is_none());
    }

    #[tokio::test]
    async fn media_errors_use_the_shared_json_envelope() {
        let response =
            crate::response::ApiError::from(MediaError::NotFound("media missing".to_owned()))
                .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("JSON"),
            json!({
                "error": {
                    "code": "not_found",
                    "message": "media missing"
                }
            })
        );
    }
}

//! Media proxy endpoint: `GET /v1/media/{account_id}/{server_name}/{media_id}`.
//!
//! The handler reconstructs the `mxc://` URI from the path components, looks up
//! the owning event (to discover the encrypted-file descriptor and the MIME
//! type), delegates the authenticated download-and-cache to the injected
//! [`MediaProxy`], then streams the resulting file back — honoring HTTP range
//! requests and conditional (`If-None-Match`) GETs, with `ETag` /
//! `Accept-Ranges` / `Content-Type` set. The body is raw binary media, not the
//! `{data}` JSON envelope.

use std::io::SeekFrom;
use std::sync::Arc;

use axon_store::Store;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::media::{MediaError, MediaProxy, MediaResource};
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

/// Derive the MIME type to serve from the event content — the authoritative
/// source, since the SDK download does not surface a `Content-Type`.
///
/// A primary attachment (matched via `content.url` or `content.file.url`) uses
/// `content.info.mimetype`; a thumbnail (matched via `content.info.
/// thumbnail_url` or `content.info.thumbnail_file.url`) uses
/// `content.info.thumbnail_info.mimetype`. The value is sender-controlled, so
/// it is sanitized (against header injection) *and* restricted to an
/// inline-safe allowlist (see [`is_inline_safe`]): anything unusable or not
/// inline-safe falls back to `application/octet-stream`. Combined with the
/// `X-Content-Type-Options: nosniff` header the handler always sets, this
/// prevents a hostile attachment declaring `text/html` / `image/svg+xml` from
/// being rendered as active content by a browser-based client.
fn content_type_for_mxc(content: &Value, mxc_url: &str) -> String {
    let info = content.get("info");

    let primary_url = content.get("url").and_then(Value::as_str).or_else(|| {
        content
            .get("file")
            .and_then(|f| f.get("url"))
            .and_then(Value::as_str)
    });
    let thumbnail_url = info
        .and_then(|i| i.get("thumbnail_url"))
        .and_then(Value::as_str)
        .or_else(|| {
            info.and_then(|i| i.get("thumbnail_file"))
                .and_then(|f| f.get("url"))
                .and_then(Value::as_str)
        });

    let raw = if primary_url == Some(mxc_url) {
        info.and_then(|i| i.get("mimetype")).and_then(Value::as_str)
    } else if thumbnail_url == Some(mxc_url) {
        info.and_then(|i| i.get("thumbnail_info"))
            .and_then(|ti| ti.get("mimetype"))
            .and_then(Value::as_str)
    } else {
        None
    };

    raw.and_then(sanitize_mime)
        .filter(|mime| is_inline_safe(mime))
        .unwrap_or_else(|| "application/octet-stream".to_owned())
}

/// Whether a sanitized MIME type is safe to serve with its declared type for
/// inline rendering. Only images (excluding SVG, which can carry script), audio,
/// and video are allowed through; everything else (`text/html`, `image/svg+xml`,
/// `application/javascript`, `application/pdf`, …) is downgraded to
/// `application/octet-stream` so a client cannot be tricked into executing
/// attacker-supplied active content.
fn is_inline_safe(mime: &str) -> bool {
    if mime == "image/svg+xml" {
        return false;
    }
    mime.starts_with("image/") || mime.starts_with("audio/") || mime.starts_with("video/")
}

/// Accept only a well-formed `type/subtype` MIME token (no parameters), guarding
/// against header injection from hostile event content. Returns `None` for
/// anything unusable so the caller falls back to `application/octet-stream`.
fn sanitize_mime(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 255 {
        return None;
    }
    let (ty, subtype) = raw.split_once('/')?;
    let is_token = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#' | b'$' | b'&' | b'-' | b'^' | b'_' | b'.' | b'+'
                    )
            })
    };
    (is_token(ty) && is_token(subtype)).then(|| format!("{ty}/{subtype}"))
}

/// The outcome of interpreting a `Range` header against a known content length.
#[derive(Debug, PartialEq, Eq)]
enum RangeSpec {
    /// No (or an ignorable/malformed) range — serve the full body with `200`.
    Full,
    /// A single satisfiable byte range (inclusive) — serve `206`.
    Satisfiable { start: u64, end: u64 },
    /// A syntactically valid range that falls outside the content — serve `416`.
    Unsatisfiable,
}

/// Parse a single-range `Range: bytes=…` header against `len`.
///
/// Per RFC 7233 a malformed or unsupported (e.g. multi-range) header is ignored
/// (→ full body); a well-formed range wholly outside the content is
/// unsatisfiable (→ `416`).
fn parse_range(header: Option<&str>, len: u64) -> RangeSpec {
    let Some(raw) = header else {
        return RangeSpec::Full;
    };
    let Some(spec) = raw.strip_prefix("bytes=") else {
        return RangeSpec::Full; // unknown unit → ignore
    };
    let spec = spec.trim();
    if spec.contains(',') {
        return RangeSpec::Full; // multi-range unsupported → serve full
    }
    let Some((start_s, end_s)) = spec.split_once('-') else {
        return RangeSpec::Full;
    };

    if start_s.is_empty() {
        // Suffix range: bytes=-N → final N bytes.
        return match end_s.parse::<u64>() {
            Ok(0) => RangeSpec::Unsatisfiable,
            Ok(n) if len == 0 => {
                let _ = n;
                RangeSpec::Unsatisfiable
            }
            Ok(n) => {
                let n = n.min(len);
                RangeSpec::Satisfiable {
                    start: len - n,
                    end: len - 1,
                }
            }
            Err(_) => RangeSpec::Full,
        };
    }

    let Ok(start) = start_s.parse::<u64>() else {
        return RangeSpec::Full;
    };
    if len == 0 || start >= len {
        return RangeSpec::Unsatisfiable;
    }
    let end = if end_s.is_empty() {
        len - 1
    } else {
        match end_s.parse::<u64>() {
            Ok(e) => e.min(len - 1),
            Err(_) => return RangeSpec::Full,
        }
    };
    if end < start {
        return RangeSpec::Full; // invalid range → ignore
    }
    RangeSpec::Satisfiable { start, end }
}

/// Whether an `If-None-Match` header matches our (quoted) entity tag, so the
/// handler can answer `304 Not Modified`.
fn if_none_match_matches(header: Option<&str>, etag_quoted: &str) -> bool {
    let Some(header) = header else {
        return false;
    };
    header
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate.trim_start_matches("W/") == etag_quoted)
}

/// Proxy an `mxc://` download through the account's homeserver connection.
///
/// The `server_name` and `media_id` path segments form the `mxc://` URI that
/// was embedded in a Matrix event's primary or thumbnail media descriptor.
/// The response body is the raw media bytes, streamed from the on-disk cache
/// with range-request and conditional-GET support.
#[utoipa::path(
    get,
    path = "/v1/media/{account_id}/{server_name}/{media_id}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account whose credentials are used for the download"),
        ("server_name" = String, Path, description = "Server-name component of the MXC URI (the part after `mxc://`)"),
        ("media_id" = String, Path, description = "Media-ID component of the MXC URI"),
    ),
    responses(
        (status = 200, description = "Full media bytes. `Content-Type` is derived from the event (inline-safe types only; else `application/octet-stream`), with `X-Content-Type-Options: nosniff`, `Accept-Ranges: bytes`, and `ETag` set."),
        (status = 206, description = "Partial media bytes for a satisfiable `Range` request (with `Content-Range`)."),
        (status = 304, description = "The `If-None-Match` entity tag matched; body omitted."),
        (status = 400, description = "Syntactically invalid MXC URI components", body = crate::response::ErrorResponse),
        (status = 404, description = "Account not found, or media not found on the homeserver", body = crate::response::ErrorResponse),
        (status = 413, description = "The media object exceeds the configured per-object cache limit", body = crate::response::ErrorResponse),
        (status = 416, description = "The requested `Range` is not satisfiable (with `Content-Range: bytes */len`).", body = crate::response::ErrorResponse),
        (status = 500, description = "Internal media-metadata lookup failure", body = crate::response::ErrorResponse),
        (status = 502, description = "The homeserver was unreachable or returned an error", body = crate::response::ErrorResponse),
    ),
    tag = "media",
)]
pub async fn get_media(
    State(proxy): State<Arc<dyn MediaProxy>>,
    State(store): State<Store>,
    Path((account_id, server_name, media_id)): Path<(Uuid, String, String)>,
    headers: HeaderMap,
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

    // Conditional GET: the entity tag is content-addressed (a hash of the MXC
    // URI) and needs no download, so answer `304` *before* fetching. This is
    // safe here because the fail-closed event checks above have already passed
    // (the event exists and is decrypted), so a matching tag means the client
    // already holds the current bytes.
    let etag = format!("\"{}\"", proxy.etag(&mxc_url));
    let if_none_match = header_str(&headers, header::IF_NONE_MATCH);
    if if_none_match_matches(if_none_match.as_deref(), &etag) {
        return Ok(not_modified(&etag));
    }

    let encrypted_file = encrypted_file_for_mxc(&content, &mxc_url);
    let content_type = content_type_for_mxc(&content, &mxc_url);

    let resource = match proxy.get_media(account_id, &mxc_url, encrypted_file).await {
        Ok(resource) => resource,
        Err(err) => {
            // The 500 body is generic, so log the real cause with the account
            // before converting (per the structured-logging convention).
            if let MediaError::Internal(detail) = &err {
                tracing::error!(%account_id, mxc = %mxc_url, error = %detail, "media proxy internal error");
            }
            return Err(err.into());
        }
    };

    let range = header_str(&headers, header::RANGE);
    serve_media(account_id, resource, content_type, range, etag).await
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Build a `304 Not Modified` response carrying the entity tag.
fn not_modified(etag: &str) -> Response {
    Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(header::ETAG, etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::empty())
        .expect("valid 304 response")
}

/// Build the media response for an open [`MediaResource`], applying range
/// serving and the standard media headers (`etag` is the already-quoted entity
/// tag). Conditional-GET (`304`) is handled by the caller before fetching.
/// Factored out so it can be unit-tested without a store or homeserver.
async fn serve_media(
    account_id: Uuid,
    mut resource: MediaResource,
    content_type: String,
    range: Option<String>,
    etag: String,
) -> Result<Response, ApiError> {
    match parse_range(range.as_deref(), resource.len) {
        RangeSpec::Unsatisfiable => {
            // Reuse the shared JSON error envelope (advertised by the OpenAPI
            // spec for this status) rather than an empty body, then layer the
            // range-specific headers on top.
            let mut response = ApiError::range_not_satisfiable(format!(
                "requested range is not satisfiable for a {}-byte resource",
                resource.len
            ))
            .into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{}", resource.len))
                    .expect("valid header value"),
            );
            headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            headers.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            headers.insert(
                header::ETAG,
                HeaderValue::from_str(&etag).expect("valid header value"),
            );
            Ok(response)
        }

        RangeSpec::Satisfiable { start, end } => {
            resource
                .file
                .seek(SeekFrom::Start(start))
                .await
                .map_err(|e| {
                    tracing::error!(%account_id, error = %e, "media cache seek failed");
                    ApiError::internal()
                })?;
            let length = end - start + 1;
            let body = Body::from_stream(ReaderStream::new(resource.file.take(length)));
            Ok(Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CONTENT_LENGTH, length)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{}", resource.len),
                )
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                .header(header::ETAG, &etag)
                .body(body)
                .expect("valid 206 response"))
        }

        RangeSpec::Full => {
            let body = Body::from_stream(ReaderStream::new(resource.file));
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CONTENT_LENGTH, resource.len)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                .header(header::ETAG, &etag)
                .body(body)
                .expect("valid 200 response"))
        }
    }
    .map(IntoResponse::into_response)
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use serde_json::json;

    use super::*;
    use crate::media::{MediaError, MediaResource};

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

    #[test]
    fn content_type_prefers_primary_mimetype() {
        let content = json!({
            "url": "mxc://example.org/main",
            "info": { "mimetype": "image/png" }
        });
        assert_eq!(
            content_type_for_mxc(&content, "mxc://example.org/main"),
            "image/png"
        );
    }

    #[test]
    fn content_type_uses_thumbnail_mimetype_for_thumb() {
        let content = json!({
            "url": "mxc://example.org/main",
            "info": {
                "mimetype": "image/png",
                "thumbnail_url": "mxc://example.org/thumb",
                "thumbnail_info": { "mimetype": "image/jpeg" }
            }
        });
        assert_eq!(
            content_type_for_mxc(&content, "mxc://example.org/thumb"),
            "image/jpeg"
        );
    }

    #[test]
    fn content_type_falls_back_to_octet_stream() {
        let content = json!({ "url": "mxc://example.org/main", "info": {} });
        assert_eq!(
            content_type_for_mxc(&content, "mxc://example.org/main"),
            "application/octet-stream"
        );
    }

    #[test]
    fn content_type_downgrades_active_and_scriptable_types() {
        // A hostile attachment declaring an active/scriptable type is coerced to
        // octet-stream so a browser client can't render it as executable content.
        for dangerous in ["text/html", "image/svg+xml", "application/javascript"] {
            let content =
                json!({ "url": "mxc://example.org/x", "info": { "mimetype": dangerous } });
            assert_eq!(
                content_type_for_mxc(&content, "mxc://example.org/x"),
                "application/octet-stream",
                "{dangerous} must be downgraded"
            );
        }
        // Inline-safe media keeps its type.
        for safe in ["image/png", "audio/ogg", "video/mp4"] {
            let content = json!({ "url": "mxc://example.org/x", "info": { "mimetype": safe } });
            assert_eq!(content_type_for_mxc(&content, "mxc://example.org/x"), safe);
        }
    }

    #[test]
    fn hostile_mimetype_is_rejected() {
        // Header-injection attempt and control characters are refused.
        assert_eq!(sanitize_mime("image/png\r\nSet-Cookie: x"), None);
        assert_eq!(sanitize_mime("not-a-mime"), None);
        assert_eq!(sanitize_mime("image/png; charset=x"), None);
        assert_eq!(sanitize_mime("image/png"), Some("image/png".to_owned()));
    }

    #[test]
    fn range_parsing_covers_the_cases() {
        assert_eq!(parse_range(None, 100), RangeSpec::Full);
        // A range covering the whole body is still a (satisfiable) range → 206.
        assert_eq!(
            parse_range(Some("bytes=0-99"), 100),
            RangeSpec::Satisfiable { start: 0, end: 99 }
        );
        assert_eq!(
            parse_range(Some("bytes=0-49"), 100),
            RangeSpec::Satisfiable { start: 0, end: 49 }
        );
        // Open-ended.
        assert_eq!(
            parse_range(Some("bytes=50-"), 100),
            RangeSpec::Satisfiable { start: 50, end: 99 }
        );
        // Suffix.
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            RangeSpec::Satisfiable { start: 90, end: 99 }
        );
        // Clamped end.
        assert_eq!(
            parse_range(Some("bytes=0-999"), 100),
            RangeSpec::Satisfiable { start: 0, end: 99 }
        );
        // Out of bounds → unsatisfiable.
        assert_eq!(
            parse_range(Some("bytes=200-300"), 100),
            RangeSpec::Unsatisfiable
        );
        // Multi-range unsupported → full.
        assert_eq!(parse_range(Some("bytes=0-1,2-3"), 100), RangeSpec::Full);
        // Malformed → full.
        assert_eq!(parse_range(Some("chars=0-1"), 100), RangeSpec::Full);
    }

    #[test]
    fn if_none_match_recognizes_star_and_tag() {
        assert!(if_none_match_matches(Some("*"), "\"abc\""));
        assert!(if_none_match_matches(Some("\"abc\""), "\"abc\""));
        assert!(if_none_match_matches(Some("\"x\", \"abc\""), "\"abc\""));
        assert!(if_none_match_matches(Some("W/\"abc\""), "\"abc\""));
        assert!(!if_none_match_matches(Some("\"other\""), "\"abc\""));
        assert!(!if_none_match_matches(None, "\"abc\""));
    }

    async fn resource_from(bytes: &[u8]) -> MediaResource {
        use tokio::io::AsyncWriteExt;
        let dir = std::env::temp_dir().join(format!("axon-media-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("obj");
        let mut f = tokio::fs::File::create(&path).await.unwrap();
        f.write_all(bytes).await.unwrap();
        f.sync_all().await.unwrap();
        drop(f);
        let file = tokio::fs::File::open(&path).await.unwrap();
        let _ = std::fs::remove_file(&path); // fd survives unlink
        MediaResource {
            file,
            len: bytes.len() as u64,
            etag: "unused".to_owned(), // serve_media takes the etag as an argument
        }
    }

    #[tokio::test]
    async fn full_response_sets_headers_and_body() {
        let resource = resource_from(b"hello world").await;
        let response = serve_media(
            Uuid::new_v4(),
            resource,
            "image/png".to_owned(),
            None,
            "\"abc\"".to_owned(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(response.headers()[header::ETAG], "\"abc\"");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "11");
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"hello world");
    }

    #[tokio::test]
    async fn range_request_returns_206_with_content_range() {
        let resource = resource_from(b"0123456789").await;
        let response = serve_media(
            Uuid::new_v4(),
            resource,
            "application/octet-stream".to_owned(),
            Some("bytes=2-5".to_owned()),
            "\"abc\"".to_owned(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "4");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"2345");
    }

    #[tokio::test]
    async fn not_modified_response_is_304_with_etag() {
        let response = not_modified("\"abc\"");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::ETAG], "\"abc\"");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn unsatisfiable_range_returns_416() {
        let resource = resource_from(b"short").await;
        let response = serve_media(
            Uuid::new_v4(),
            resource,
            "image/png".to_owned(),
            Some("bytes=100-200".to_owned()),
            "\"abc\"".to_owned(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */5");
        assert_eq!(response.headers()[header::ETAG], "\"abc\"");

        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
        assert_eq!(json["error"]["code"], "range_not_satisfiable");
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

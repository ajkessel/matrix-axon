//! Full-text search endpoint (M9b).
//!
//! `GET /v1/search` runs a BM25 query against the `axon-search` index (via the
//! [`SearchQuery`] port) and hydrates each hit into the same resolved [`EventDto`]
//! the rest of the read API returns. The index holds only `(account_id, event_id)`
//! keys, so hydration is a per-hit store read; an index/DB race (a hit whose row
//! was since deleted) drops that hit rather than failing the page.

use std::sync::Arc;

use axon_store::Store;
use axum::extract::State;
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::cursor;
use crate::dto::{EventDto, SearchPage, SearchResultDto};
use crate::extract::Query;
use crate::response::{ApiError, ApiResponse};
use crate::search::{SearchQuery, SearchQueryParams};

/// Default page size when `limit` is omitted.
const DEFAULT_LIMIT: i64 = 50;
/// Hard cap on page size, regardless of the requested `limit`.
const MAX_LIMIT: i64 = 200;
/// Hard cap on the paging offset a cursor may decode to. Offset pagination is
/// skip-N work at the index (Tantivy's collector keeps the top `offset + limit`
/// docs), so an attacker-forged cursor for a huge offset would amplify one request
/// into large allocation/CPU. Cursors past this bound are rejected as a `400`
/// rather than executed. At `MAX_LIMIT` per page this is still thousands of pages.
const MAX_OFFSET: usize = 100_000;

/// Query parameters for `GET /v1/search`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchQueryDto {
    /// The full-text query string (required, non-empty). Parsed against message
    /// bodies; all terms are required (AND).
    pub q: Option<String>,
    /// Restrict to one account. Omit to search across all accounts.
    pub account_id: Option<Uuid>,
    /// Restrict to one room.
    pub room_id: Option<String>,
    /// Restrict to one sender (Matrix user id).
    pub sender: Option<String>,
    /// Inclusive lower bound on `origin_server_ts`, Unix milliseconds.
    pub from: Option<i64>,
    /// Inclusive upper bound on `origin_server_ts`, Unix milliseconds.
    pub to: Option<i64>,
    /// Page size (default 50, max 200).
    pub limit: Option<i64>,
    /// Opaque cursor from a previous page's `next_cursor`; omit for the first page.
    pub cursor: Option<String>,
}

/// Full-text search across the index, BM25-ranked, paginated.
///
/// `503` when search is disabled (`search.enabled = false`). A missing/empty `q`
/// or a malformed `cursor` is a `400`. Results are the resolved read-API event
/// view (latest edited body, redaction-masked) plus each hit's score.
#[utoipa::path(
    get,
    path = "/v1/search",
    params(SearchQueryDto),
    responses(
        (status = 200, description = "A page of ranked search results", body = ApiResponse<SearchPage>),
        (status = 400, description = "Missing/empty query or malformed cursor", body = crate::response::ErrorResponse),
        (status = 503, description = "Search is disabled", body = crate::response::ErrorResponse),
    ),
    tag = "search",
)]
pub async fn search(
    State(store): State<Store>,
    State(search): State<Option<Arc<dyn SearchQuery>>>,
    Query(q): Query<SearchQueryDto>,
) -> Result<ApiResponse<SearchPage>, ApiError> {
    let Some(search) = search else {
        return Err(ApiError::service_unavailable("search is disabled"));
    };

    let text = q.q.unwrap_or_default();
    let text = text.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request("q is required"));
    }

    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let offset = match q.cursor.as_deref() {
        Some(c) => cursor::decode_offset_bounded(c, MAX_OFFSET)
            .ok_or_else(|| ApiError::bad_request("invalid cursor"))?,
        None => 0,
    };

    let params = SearchQueryParams {
        text: text.to_owned(),
        account_id: q.account_id,
        room_id: q.room_id,
        sender: q.sender,
        from_ts: q.from,
        to_ts: q.to,
        limit,
        offset,
    };
    let found = search.search(&params).await?;

    // The index returns `(account_id, event_id)` keys; hydrate each from the store.
    // A hit whose row was deleted between indexing and now is skipped, not a 500.
    let mut results = Vec::with_capacity(found.hits.len());
    for hit in &found.hits {
        if let Some(row) = store.get_event(hit.account_id, &hit.event_id).await? {
            results.push(SearchResultDto {
                event: EventDto::from_row(hit.account_id, row),
                score: hit.score,
            });
        }
    }

    // Advance by the number of hits the index returned (pre-hydration), so a
    // skipped row doesn't desync paging. More pages remain while we're short of
    // the total. Checked arithmetic so a near-`usize::MAX` offset can't wrap;
    // `offset` is already bounded to `MAX_OFFSET`, so this never actually saturates.
    let next_offset = offset.saturating_add(found.hits.len());
    let next_cursor = (next_offset < found.total).then(|| cursor::encode_offset(next_offset));

    Ok(ApiResponse::new(SearchPage {
        results,
        total: found.total,
        next_cursor,
    }))
}

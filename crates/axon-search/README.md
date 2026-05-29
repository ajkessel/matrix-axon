# axon-search

Tantivy full-text search index, populated on event ingestion.

## Responsibility

Opens and manages the Tantivy index, indexes events as they are written by `axon-sync`, and serves `GET /v1/search` queries with BM25 ranking and account/room/sender facet filters.

## Owns vs. consumes

- **Owns:** the Tantivy index directory on disk.
- **Consumes:** `axon-core` config, `axon-store` event types.

## Status

Stub — no public API yet.

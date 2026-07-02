//! The Tantivy schema, body analyzer, and document build/delete helpers.
//!
//! Fields:
//!
//! * `doc_key` — `"<account_id>\u{1f}<event_id>"`, the unique document key. Indexed
//!   (untokenized) so `delete_term` removes exactly one account's copy of an event
//!   (the same Matrix `event_id` can exist under two accounts).
//! * `event_id` / `account_id` — stored (and indexed) so a hit can be hydrated
//!   back to `(account_id, event_id)` and the query can filter by account.
//! * `room_id` / `sender` — indexed keyword fields for exact-match filtering.
//! * `origin_ts` — i64 milliseconds, indexed + fast for range filtering.
//! * `body` — the message text, analyzed by [`BODY_TOKENIZER`].
//!
//! `account_id` / `room_id` / `sender` are exact-match keyword (`STRING`) fields
//! rather than Tantivy *facets* (which the spec names): MVP filtering is flat
//! equality, `STRING` fields are robust to the `!`/`:`/`@`/`/` characters in room
//! ids and senders without facet-path escaping, and they double as stored
//! retrieval fields. Hierarchical faceting (e.g. faceted counts) can revisit this.
//! See the M9 search ADR.

use axon_store::IndexableEvent;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED, STRING,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer,
    TextAnalyzer,
};
use tantivy::{Index, IndexWriter, TantivyDocument, Term};
use uuid::Uuid;

/// Name the body analyzer is registered under (referenced from the field schema).
pub(crate) const BODY_TOKENIZER: &str = "axon_body";

/// Drop tokens longer than this many bytes before indexing — Tantivy's default
/// `en_stem` uses the same limit. Applied identically at index and query time, so
/// it only means absurdly long "words" don't match; it bounds term length against
/// hostile input.
const MAX_TOKEN_LEN: usize = 40;

/// The schema plus typed handles to each field. Cheap to clone (every field is a
/// `Copy` handle and [`Schema`] is `Arc`-backed).
#[derive(Clone)]
pub(crate) struct SearchSchema {
    pub schema: Schema,
    pub doc_key: Field,
    pub event_id: Field,
    pub account_id: Field,
    pub room_id: Field,
    pub sender: Field,
    pub origin_ts: Field,
    pub body: Field,
}

impl SearchSchema {
    /// Build the schema and its field handles.
    pub fn build() -> Self {
        let mut b = Schema::builder();
        let doc_key = b.add_text_field("doc_key", STRING);
        let event_id = b.add_text_field("event_id", STRING | STORED);
        let account_id = b.add_text_field("account_id", STRING | STORED);
        let room_id = b.add_text_field("room_id", STRING);
        let sender = b.add_text_field("sender", STRING);
        let origin_ts = b.add_i64_field("origin_ts", INDEXED | FAST);
        let body_indexing = TextFieldIndexing::default()
            .set_tokenizer(BODY_TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let body = b.add_text_field(
            "body",
            TextOptions::default().set_indexing_options(body_indexing),
        );
        Self {
            schema: b.build(),
            doc_key,
            event_id,
            account_id,
            room_id,
            sender,
            origin_ts,
            body,
        }
    }

    /// Register the body analyzer on `index`: the default tokenizer plus the three
    /// built-in token filters from the spec — `LowerCaser` (case-insensitive),
    /// `AsciiFoldingFilter` (diacritics fold: `café` ≈ `cafe`), and an English
    /// `Stemmer` (`cats` ≈ `cat`) — bounded by [`RemoveLongFilter`]. Must be called
    /// on every `Index` handle before it indexes or queries the `body` field.
    pub fn register_tokenizer(&self, index: &Index) {
        let analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(MAX_TOKEN_LEN))
            .filter(LowerCaser)
            .filter(AsciiFoldingFilter)
            .filter(Stemmer::new(Language::English))
            .build();
        index.tokenizers().register(BODY_TOKENIZER, analyzer);
    }

    /// The unique document key for `(account_id, event_id)`.
    fn doc_key_value(account_id: Uuid, event_id: &str) -> String {
        // Unit separator can't appear in a UUID or a Matrix event id.
        format!("{account_id}\u{1f}{event_id}")
    }

    /// Add a document for `ev` (no delete first — used by the from-scratch bulk
    /// build, which clears the index before streaming).
    pub fn add(&self, writer: &IndexWriter, ev: &IndexableEvent) -> tantivy::Result<()> {
        let mut doc = TantivyDocument::default();
        doc.add_text(
            self.doc_key,
            Self::doc_key_value(ev.account_id, &ev.event_id),
        );
        doc.add_text(self.event_id, &ev.event_id);
        doc.add_text(self.account_id, ev.account_id.to_string());
        doc.add_text(self.room_id, &ev.room_id);
        doc.add_text(self.sender, &ev.sender);
        doc.add_i64(self.origin_ts, ev.origin_ts);
        doc.add_text(self.body, &ev.body);
        writer.add_document(doc)?;
        Ok(())
    }

    /// Replace the document for `ev` (delete the prior copy, then add) — the live
    /// path, where a target may already be indexed and is being re-derived.
    pub fn upsert(&self, writer: &IndexWriter, ev: &IndexableEvent) -> tantivy::Result<()> {
        self.delete(writer, ev.account_id, &ev.event_id);
        self.add(writer, ev)
    }

    /// Delete the document for `(account_id, event_id)`, if present.
    pub fn delete(&self, writer: &IndexWriter, account_id: Uuid, event_id: &str) {
        let term = Term::from_field_text(self.doc_key, &Self::doc_key_value(account_id, event_id));
        writer.delete_term(term);
    }

    /// Delete every document belonging to `account_id`.
    pub fn delete_account(&self, writer: &IndexWriter, account_id: Uuid) {
        let term = Term::from_field_text(self.account_id, &account_id.to_string());
        writer.delete_term(term);
    }

    /// Delete every document belonging to `account_id` in `room_id`. The index is
    /// combined across accounts, so this is scoped by *both* fields — a room id can
    /// be shared by two accounts (both joined the same room), and purging one
    /// account's copy must not touch the other's. `delete_term` takes a single
    /// term, so this uses `delete_query` with a conjunction of the two keyword
    /// terms (both fields are indexed, so the query resolves to a term set).
    pub fn delete_room(
        &self,
        writer: &IndexWriter,
        account_id: Uuid,
        room_id: &str,
    ) -> tantivy::Result<()> {
        let account = TermQuery::new(
            Term::from_field_text(self.account_id, &account_id.to_string()),
            IndexRecordOption::Basic,
        );
        let room = TermQuery::new(
            Term::from_field_text(self.room_id, room_id),
            IndexRecordOption::Basic,
        );
        let query = BooleanQuery::new(vec![
            (Occur::Must, Box::new(account) as Box<dyn Query>),
            (Occur::Must, Box::new(room) as Box<dyn Query>),
        ]);
        writer.delete_query(Box::new(query))?;
        Ok(())
    }
}

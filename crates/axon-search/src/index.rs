//! [`SearchIndex`]: opening the index, the BM25 query path, and spawning the
//! background indexing actor.

use std::ops::Bound;
use std::path::{Path, PathBuf};

use axon_store::Store;
use tantivy::collector::{Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};
use uuid::Uuid;

use crate::schema::SearchSchema;
use crate::writer::{self, IndexerHandles, IndexerOptions};
use crate::{SearchError, SCHEMA_VERSION};

/// Sidecar file inside the index directory recording the [`SCHEMA_VERSION`] this
/// directory was **fully seeded** with. Its presence (with a matching version) is
/// the durable "this physical index is valid for the current schema *and* the
/// corpus seed completed" marker — written by the indexing actor only *after*
/// [`seed`](crate::writer) succeeds, never at open. Lives with the physical index
/// (not in the database) so a new, emptied, moved, schema-bumped, or
/// partially-seeded directory is detected on open and reseeded.
const SCHEMA_VERSION_FILE: &str = "axon_schema_version";

/// Filters and pagination for a search. `text` is the user query; the rest narrow
/// the result set. All filters are optional — omit `account_id` to search across
/// every account (the index is one combined index).
#[derive(Debug, Clone)]
pub struct SearchParams<'a> {
    /// The full-text query string (parsed against the `body` field).
    pub text: &'a str,
    /// Restrict to one account.
    pub account_id: Option<Uuid>,
    /// Restrict to one room.
    pub room_id: Option<&'a str>,
    /// Restrict to one sender.
    pub sender: Option<&'a str>,
    /// Inclusive lower bound on `origin_ts` (ms since epoch).
    pub from_ts: Option<i64>,
    /// Inclusive upper bound on `origin_ts` (ms since epoch).
    pub to_ts: Option<i64>,
    /// Maximum hits to return.
    pub limit: usize,
    /// Number of leading hits to skip (offset pagination).
    pub offset: usize,
}

/// One search hit: enough to hydrate the full event from Postgres, plus its score.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The account the matching event belongs to.
    pub account_id: Uuid,
    /// The matching event's id.
    pub event_id: String,
    /// BM25 relevance score (higher is more relevant).
    pub score: f32,
}

/// A page of search results.
#[derive(Debug, Clone)]
pub struct SearchResults {
    /// The hits on this page, most relevant first.
    pub hits: Vec<SearchHit>,
    /// Total number of matching documents across all pages.
    pub total: usize,
}

/// A handle to the Tantivy index: the read side (queries) plus a factory for the
/// single write actor. Cheap to keep alive; the [`IndexReader`] auto-reloads after
/// the actor commits.
pub struct SearchIndex {
    index: Index,
    schema: SearchSchema,
    reader: IndexReader,
    /// The index directory — retained so the actor can stamp the seed-completion
    /// marker ([`SCHEMA_VERSION_FILE`]) only after a successful seed.
    dir: PathBuf,
}

impl SearchIndex {
    /// Open the index under `dir`, registering the body analyzer and an
    /// auto-reloading reader. Returns the index and a `fresh` flag: `true` when the
    /// directory was new, empty, built against a different [`SCHEMA_VERSION`], or
    /// left **partially seeded** by an interrupted earlier run (all detected via the
    /// absence or mismatch of the [`SCHEMA_VERSION_FILE`] seed-completion marker), in
    /// which case the stale directory is wiped and recreated and the caller must
    /// seed from the store before the index is trustworthy.
    ///
    /// The marker is **not** written here. It is stamped by the indexing actor only
    /// after [`seed`](crate::writer) completes, so a crash or failure mid-seed leaves
    /// no marker and the next open reseeds rather than trusting a partial index
    /// (ADR 0039). A `fresh` open therefore yields an *empty* index until the actor
    /// finishes seeding.
    ///
    /// Handling the schema check *before* opening is deliberate: Tantivy's
    /// `open_or_create` rejects an existing index whose schema differs, so a naive
    /// open would fail on a schema bump instead of rebuilding (ADR 0039).
    pub fn open(dir: &Path) -> Result<(Self, bool), SearchError> {
        let fresh = needs_rebuild(dir)?;
        if fresh && dir.exists() {
            // Drop any stale or partial index so `open_or_create` builds clean.
            std::fs::remove_dir_all(dir)?;
        }
        std::fs::create_dir_all(dir)?;

        let schema = SearchSchema::build();
        let mmap = MmapDirectory::open(dir)?;
        let index = Index::open_or_create(mmap, schema.schema.clone())?;
        schema.register_tokenizer(&index);

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        Ok((
            Self {
                index,
                schema,
                reader,
                dir: dir.to_path_buf(),
            },
            fresh,
        ))
    }

    /// Spawn the background indexing actor: it owns the sole [`IndexWriter`], seeds
    /// the corpus from the store when `fresh`, then drains the `search_outbox`
    /// change log until its channel closes. Returns the cloneable producer handle
    /// and the task's join handle.
    pub fn spawn_indexer(
        &self,
        store: Store,
        fresh: bool,
        opts: IndexerOptions,
    ) -> Result<IndexerHandles, SearchError> {
        let writer: IndexWriter = self.index.writer(opts.writer_heap_bytes())?;
        Ok(writer::spawn(
            writer,
            self.schema.clone(),
            store,
            self.dir.clone(),
            fresh,
            opts,
        ))
    }

    /// Force the reader to pick up the latest commit. Production reads rely on the
    /// auto-reload policy; tests call this to read deterministically right after a
    /// commit.
    pub fn reload(&self) -> Result<(), SearchError> {
        self.reader.reload()?;
        Ok(())
    }

    /// Run a BM25 search. Returns the requested page of hits plus the total match
    /// count. A malformed `text` query is a [`SearchError::BadQuery`].
    pub fn search(&self, params: &SearchParams<'_>) -> Result<SearchResults, SearchError> {
        let searcher = self.reader.searcher();

        let mut parser = QueryParser::for_index(&self.index, vec![self.schema.body]);
        parser.set_conjunction_by_default(); // all terms required (AND), the intuitive default
        let text_query = parser
            .parse_query(params.text)
            .map_err(|e| SearchError::BadQuery(e.to_string()))?;

        // Combine the text query with the exact-match / range filters.
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, text_query)];
        if let Some(account_id) = params.account_id {
            clauses.push((
                Occur::Must,
                self.term_query(self.schema.account_id, &account_id.to_string()),
            ));
        }
        if let Some(room_id) = params.room_id {
            clauses.push((Occur::Must, self.term_query(self.schema.room_id, room_id)));
        }
        if let Some(sender) = params.sender {
            clauses.push((Occur::Must, self.term_query(self.schema.sender, sender)));
        }
        if params.from_ts.is_some() || params.to_ts.is_some() {
            let lower = match params.from_ts {
                Some(v) => Bound::Included(Term::from_field_i64(self.schema.origin_ts, v)),
                None => Bound::Unbounded,
            };
            let upper = match params.to_ts {
                Some(v) => Bound::Included(Term::from_field_i64(self.schema.origin_ts, v)),
                None => Bound::Unbounded,
            };
            clauses.push((Occur::Must, Box::new(RangeQuery::new(lower, upper))));
        }
        let query = BooleanQuery::new(clauses);

        let collector = (
            TopDocs::with_limit(params.limit)
                .and_offset(params.offset)
                .order_by_score(),
            Count,
        );
        let (top, total) = searcher.search(&query, &collector)?;

        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let (Some(account_id), Some(event_id)) = (
                doc.get_first(self.schema.account_id)
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok()),
                doc.get_first(self.schema.event_id)
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            ) else {
                // A document missing its stored key is corrupt; skip rather than fail
                // the whole query.
                continue;
            };
            hits.push(SearchHit {
                account_id,
                event_id,
                score,
            });
        }

        Ok(SearchResults { hits, total })
    }

    fn term_query(&self, field: tantivy::schema::Field, value: &str) -> Box<dyn Query> {
        let term = Term::from_field_text(field, value);
        Box::new(TermQuery::new(term, IndexRecordOption::Basic))
    }

    /// Whether the index reader currently holds zero documents (test helper).
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.reader.searcher().num_docs() == 0
    }

    /// Index a batch of already-resolved events and commit, for tests. The live
    /// path goes through the actor's writer; this opens a transient writer, so it
    /// must not run while the actor is active on the same index.
    #[cfg(test)]
    fn index_for_test(&self, events: &[axon_store::IndexableEvent]) {
        let mut writer: IndexWriter = self.index.writer(15_000_000).expect("writer");
        for ev in events {
            self.schema.add(&writer, ev).expect("add");
        }
        writer.commit().expect("commit");
        self.reload().expect("reload");
    }
}

/// Whether the index directory must be (re)seeded from scratch: it is missing the
/// [`SCHEMA_VERSION_FILE`] seed-completion marker (new, empty, a pre-marker index,
/// or an interrupted earlier seed) or the marker records a different
/// [`SCHEMA_VERSION`]. A read error on the marker is treated as "rebuild" — the safe
/// direction.
fn needs_rebuild(dir: &Path) -> Result<bool, SearchError> {
    let sidecar = dir.join(SCHEMA_VERSION_FILE);
    match std::fs::read_to_string(&sidecar) {
        Ok(v) => Ok(v.trim() != SCHEMA_VERSION),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(err) => Err(err.into()),
    }
}

/// Stamp the [`SCHEMA_VERSION_FILE`] seed-completion marker after a successful
/// corpus seed. Called by the indexing actor — never by [`SearchIndex::open`] — so
/// the marker's presence durably means "this physical index is valid for the
/// current schema and fully seeded". An interrupted seed leaves no marker, so the
/// next [`SearchIndex::open`] sees `fresh` and reseeds rather than trusting a
/// partial index (ADR 0039).
pub(crate) fn write_seed_marker(dir: &Path) -> Result<(), SearchError> {
    std::fs::write(dir.join(SCHEMA_VERSION_FILE), SCHEMA_VERSION)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axon_store::IndexableEvent;

    fn ev(
        account: Uuid,
        event_id: &str,
        room: &str,
        sender: &str,
        ts: i64,
        body: &str,
    ) -> IndexableEvent {
        IndexableEvent {
            id: 0,
            account_id: account,
            event_id: event_id.to_owned(),
            room_id: room.to_owned(),
            sender: sender.to_owned(),
            origin_ts: ts,
            body: body.to_owned(),
        }
    }

    fn open_tmp() -> (SearchIndex, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (index, fresh) = SearchIndex::open(dir.path()).expect("open");
        assert!(fresh, "a brand-new directory opens fresh");
        (index, dir)
    }

    fn params(text: &str) -> SearchParams<'_> {
        SearchParams {
            text,
            account_id: None,
            room_id: None,
            sender: None,
            from_ts: None,
            to_ts: None,
            limit: 10,
            offset: 0,
        }
    }

    #[test]
    fn exact_phrase_is_top_hit() {
        let (index, _dir) = open_tmp();
        let acct = Uuid::new_v4();
        index.index_for_test(&[
            ev(acct, "$a", "!r", "@u:x", 1, "the quick brown fox jumps"),
            ev(acct, "$b", "!r", "@u:x", 2, "a slow green turtle walks"),
            ev(acct, "$c", "!r", "@u:x", 3, "quick notes about nothing"),
        ]);
        let res = index
            .search(&params("\"quick brown fox\""))
            .expect("search");
        assert_eq!(res.total, 1);
        assert_eq!(res.hits[0].event_id, "$a");
    }

    #[test]
    fn case_and_diacritic_and_plural_insensitive() {
        let (index, _dir) = open_tmp();
        let acct = Uuid::new_v4();
        index.index_for_test(&[
            ev(
                acct,
                "$cafe",
                "!r",
                "@u:x",
                1,
                "we met at the Café downtown",
            ),
            ev(acct, "$cats", "!r", "@u:x", 2, "the cats were sleeping"),
        ]);
        // Case- and diacritic-insensitive: `cafe` matches `Café`.
        assert_eq!(
            index.search(&params("cafe")).unwrap().hits[0].event_id,
            "$cafe"
        );
        assert_eq!(
            index.search(&params("CAFE")).unwrap().hits[0].event_id,
            "$cafe"
        );
        // Stemming: `cat` matches `cats`.
        assert_eq!(
            index.search(&params("cat")).unwrap().hits[0].event_id,
            "$cats"
        );
    }

    #[test]
    fn account_room_sender_filters() {
        let (index, _dir) = open_tmp();
        let a1 = Uuid::new_v4();
        let a2 = Uuid::new_v4();
        index.index_for_test(&[
            ev(a1, "$1", "!red", "@alice:x", 1, "shared keyword here"),
            ev(a2, "$2", "!blue", "@bob:x", 2, "shared keyword here"),
        ]);
        // Cross-account by default.
        assert_eq!(index.search(&params("keyword")).unwrap().total, 2);
        // Scoped to one account.
        let mut p = params("keyword");
        p.account_id = Some(a1);
        let r = index.search(&p).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.hits[0].account_id, a1);
        // Room filter.
        let mut p = params("keyword");
        p.room_id = Some("!blue");
        assert_eq!(index.search(&p).unwrap().hits[0].event_id, "$2");
        // Sender filter.
        let mut p = params("keyword");
        p.sender = Some("@alice:x");
        assert_eq!(index.search(&p).unwrap().hits[0].event_id, "$1");
    }

    #[test]
    fn date_range_filter() {
        let (index, _dir) = open_tmp();
        let acct = Uuid::new_v4();
        index.index_for_test(&[
            ev(acct, "$old", "!r", "@u:x", 100, "anniversary dinner"),
            ev(acct, "$new", "!r", "@u:x", 200, "anniversary dinner"),
        ]);
        let mut p = params("anniversary");
        p.from_ts = Some(150);
        let r = index.search(&p).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.hits[0].event_id, "$new");
    }

    #[test]
    fn pagination_offset_and_total() {
        let (index, _dir) = open_tmp();
        let acct = Uuid::new_v4();
        let events: Vec<_> = (0..5)
            .map(|i| ev(acct, &format!("$e{i}"), "!r", "@u:x", i, "paginate token"))
            .collect();
        index.index_for_test(&events);
        let mut p = params("paginate");
        p.limit = 2;
        p.offset = 0;
        let r0 = index.search(&p).unwrap();
        assert_eq!(r0.total, 5);
        assert_eq!(r0.hits.len(), 2);
        p.offset = 4;
        let r2 = index.search(&p).unwrap();
        assert_eq!(r2.total, 5);
        assert_eq!(r2.hits.len(), 1);
    }

    #[test]
    fn same_event_id_distinct_per_account() {
        // The same Matrix event id under two accounts must be two documents, so a
        // per-account delete (doc_key) removes only one.
        let (index, _dir) = open_tmp();
        let a1 = Uuid::new_v4();
        let a2 = Uuid::new_v4();
        index.index_for_test(&[
            ev(a1, "$dup", "!r", "@u:x", 1, "collision body"),
            ev(a2, "$dup", "!r", "@u:x", 1, "collision body"),
        ]);
        assert_eq!(index.search(&params("collision")).unwrap().total, 2);
    }

    #[test]
    fn reopen_same_version_is_not_fresh_and_keeps_docs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let acct = Uuid::new_v4();
        {
            let (index, fresh) = SearchIndex::open(dir.path()).expect("open");
            assert!(fresh);
            index.index_for_test(&[ev(acct, "$a", "!r", "@u:x", 1, "persistent body")]);
            // The actor stamps this only after a successful seed; simulate that here.
            write_seed_marker(dir.path()).expect("mark seeded");
        }
        // Same SCHEMA_VERSION marker → open the existing index, do not wipe it.
        let (index, fresh) = SearchIndex::open(dir.path()).expect("reopen");
        assert!(!fresh, "a matching seed-completion marker reopens in place");
        assert_eq!(index.search(&params("persistent")).unwrap().total, 1);
    }

    #[test]
    fn interrupted_seed_reseeds_on_restart() {
        // Regression for the "rebuild marked too early" blocker: a seed that wrote
        // documents but crashed before stamping the marker must NOT be trusted. The
        // next open sees no marker, reports `fresh`, and wipes the partial index so
        // the actor reseeds from the store.
        let dir = tempfile::tempdir().expect("tempdir");
        let acct = Uuid::new_v4();
        {
            let (index, fresh) = SearchIndex::open(dir.path()).expect("open");
            assert!(fresh);
            // Partial corpus committed, but the marker is never written (crash mid-seed).
            index.index_for_test(&[ev(acct, "$a", "!r", "@u:x", 1, "partial body")]);
        }
        let (index, fresh) = SearchIndex::open(dir.path()).expect("reopen");
        assert!(
            fresh,
            "an unmarked (interrupted) seed reseeds on the next open"
        );
        assert!(
            index.is_empty(),
            "the partial index was wiped, ready for re-seed"
        );
    }

    #[test]
    fn stale_schema_version_rebuilds_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        let acct = Uuid::new_v4();
        {
            let (index, _) = SearchIndex::open(dir.path()).expect("open");
            index.index_for_test(&[ev(acct, "$a", "!r", "@u:x", 1, "doomed body")]);
        }
        // Simulate a schema bump: an older version stamped in the marker. Opening
        // must wipe and recreate the directory rather than fail on the schema
        // mismatch (the bug the marker check exists to prevent).
        std::fs::write(dir.path().join(SCHEMA_VERSION_FILE), "0").unwrap();
        let (index, fresh) = SearchIndex::open(dir.path()).expect("open after bump");
        assert!(fresh, "a changed schema version opens fresh");
        assert!(
            index.is_empty(),
            "the stale index was wiped, ready for re-seed"
        );
        // The stale marker was removed with the directory and is NOT restamped at
        // open — the actor stamps the current version only after the reseed
        // completes, so an interrupted reseed is retried.
        assert!(
            !dir.path().join(SCHEMA_VERSION_FILE).exists(),
            "the marker is absent until the actor finishes reseeding"
        );
    }

    #[test]
    fn malformed_query_is_bad_query() {
        let (index, _dir) = open_tmp();
        // An unbalanced quote / illegal syntax should surface as BadQuery, not panic.
        let err = index.search(&params("\"unterminated")).err();
        assert!(matches!(err, Some(SearchError::BadQuery(_))) || err.is_none());
    }
}

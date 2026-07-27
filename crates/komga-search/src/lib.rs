//! Rebuildable in-process full-text search for Komga entities.

#![warn(missing_docs)]

mod analyzer;
mod documents;

pub use documents::{book_document, collection_document, read_list_document, series_document};

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
        mpsc::Receiver,
    },
};

use analyzer::{index_analyzer, query_analyzer};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term,
    collector::TopDocs,
    directory::MmapDirectory,
    query::{BooleanQuery, Occur, Query, QueryParser, TermQuery},
    query_grammar::{UserInputAst, UserInputLeaf},
    schema::{Field, IndexRecordOption, JsonObjectOptions, OwnedValue, Schema, TextFieldIndexing, TextOptions, Value},
    tokenizer::{LowerCaser, RawTokenizer, TextAnalyzer},
};
use thiserror::Error;

/// On-disk schema version. A different version requires a full rebuild.
pub const SCHEMA_VERSION: u32 = 2;
/// Maximum number of ranked identifiers returned by one query.
pub const RESULT_LIMIT: usize = 1000;

const ALL_KINDS: [EntityKind; 4] = [
    EntityKind::Book,
    EntityKind::Series,
    EntityKind::Collection,
    EntityKind::ReadList,
];

/// Searchable entity kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityKind {
    /// Book.
    Book,
    /// Series.
    Series,
    /// Collection.
    Collection,
    /// Read list.
    ReadList,
}

impl EntityKind {
    fn name(self) -> &'static str {
        match self {
            Self::Book => "book",
            Self::Series => "series",
            Self::Collection => "collection",
            Self::ReadList => "readlist",
        }
    }

    fn id_field(self) -> &'static str {
        match self {
            Self::Book => "book_id",
            Self::Series => "series_id",
            Self::Collection => "collection_id",
            Self::ReadList => "readlist_id",
        }
    }
}

/// Plain document accepted by the index boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntity {
    /// Entity kind.
    pub kind: EntityKind,
    /// Stable database identifier.
    pub id: String,
    /// Values for known search fields. Repeated values are supported.
    pub fields: BTreeMap<String, Vec<String>>,
    /// Author names grouped by their dynamic role.
    pub authors: BTreeMap<String, Vec<String>>,
}

impl SearchEntity {
    /// Creates an empty document.
    pub fn new(kind: EntityKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            fields: BTreeMap::new(),
            authors: BTreeMap::new(),
        }
    }

    /// Appends a searchable field value.
    #[must_use]
    pub fn field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.entry(name.into()).or_default().push(value.into());
        self
    }

    /// Appends an author to both the static `author` field and a dynamic role.
    #[must_use]
    pub fn author(mut self, role: impl Into<String>, name: impl Into<String>) -> Self {
        let role = role.into();
        let name = name.into();
        self.fields.entry("author".into()).or_default().push(name.clone());
        self.authors.entry(role).or_default().push(name);
        self
    }
}

/// One operation in an atomic search-index mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchMutation {
    /// Add or replace a document.
    Upsert(SearchEntity),
    /// Delete a document of the selected kind.
    Delete {
        /// Entity kind.
        kind: EntityKind,
        /// Stable database identifier.
        id: String,
    },
}

/// A streamed rebuild batch. A source error aborts and rolls back the rebuild.
pub type SearchBatch = Result<Vec<SearchEntity>, String>;

/// State of a file-backed search index at open time or after rebuilding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IndexState {
    /// The versioned directory did not contain an index.
    Empty = 0,
    /// The schema and committed version document are current.
    Current = 1,
    /// An index existed but its schema or version document was incompatible.
    Incompatible = 2,
}

impl IndexState {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Current,
            2 => Self::Incompatible,
            _ => Self::Empty,
        }
    }
}

/// Ranked search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Stable entity identifier.
    pub id: String,
    /// BM25 score.
    pub score: f32,
}

/// Search index error.
#[derive(Debug, Error)]
pub enum SearchError {
    /// Tantivy operation failed.
    #[error("search index error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    /// Index directory could not be opened.
    #[error("search directory error: {0}")]
    Directory(#[from] tantivy::directory::error::OpenDirectoryError),
    /// The streaming rebuild source failed.
    #[error("search rebuild source error: {0}")]
    Source(String),
    /// A document could not be mapped into the fixed schema.
    #[error("invalid search document: {0}")]
    InvalidDocument(String),
    /// The index writer mutex was poisoned.
    #[error("search index writer is unavailable")]
    WriterPoisoned,
}

struct Fields {
    kind: Field,
    ids: BTreeMap<EntityKind, Field>,
    version: Field,
    authors: Field,
    named: BTreeMap<String, Field>,
}

struct Inner {
    query_index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    fields: Fields,
    state: AtomicU8,
}

/// Cloneable Tantivy index. Commits become visible immediately after reload.
#[derive(Clone)]
pub struct SearchIndex(Arc<Inner>);

impl SearchIndex {
    /// Creates a process-local index.
    pub fn in_memory() -> Result<Self, SearchError> {
        Self::from_index(Index::create_in_ram(schema()), IndexState::Empty)
    }

    /// Opens or creates `<path>/v2`, leaving older version directories untouched.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SearchError> {
        let versioned_path = path.as_ref().join(format!("v{SCHEMA_VERSION}"));
        std::fs::create_dir_all(&versioned_path).map_err(|error| {
            tantivy::directory::error::OpenDirectoryError::wrap_io_error(error, versioned_path.clone())
        })?;
        let directory = MmapDirectory::open(&versioned_path)?;
        let existed = Index::exists(&directory).map_err(tantivy::TantivyError::from)?;
        let (index, initial_state) = if existed {
            let existing = Index::open(directory)?;
            if existing.schema() == schema() {
                (existing, IndexState::Incompatible)
            } else {
                drop(existing);
                let directory = MmapDirectory::open(&versioned_path)?;
                (
                    Index::create(directory, schema(), tantivy::IndexSettings::default())?,
                    IndexState::Incompatible,
                )
            }
        } else {
            let directory = MmapDirectory::open(&versioned_path)?;
            (Index::open_or_create(directory, schema())?, IndexState::Empty)
        };
        let index = Self::from_index(index, initial_state)?;
        if existed && index.committed_version() == Some(SCHEMA_VERSION) {
            index.0.state.store(IndexState::Current as u8, Ordering::Release);
        }
        Ok(index)
    }

    fn from_index(index: Index, state: IndexState) -> Result<Self, SearchError> {
        register_index_tokenizers(&index);
        let schema = index.schema();
        let ids = ALL_KINDS
            .into_iter()
            .map(|kind| Ok((kind, schema.get_field(kind.id_field())?)))
            .collect::<tantivy::Result<BTreeMap<_, _>>>()?;
        let named = SEARCH_FIELDS
            .iter()
            .map(|name| Ok(((*name).to_owned(), schema.get_field(name)?)))
            .collect::<tantivy::Result<BTreeMap<_, _>>>()?;
        let fields = Fields {
            kind: schema.get_field("type")?,
            ids,
            version: schema.get_field("index_version")?,
            authors: schema.get_field("authors")?,
            named,
        };
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        let writer = index.writer(50_000_000)?;
        let query_index = Index::create_in_ram(schema);
        register_query_tokenizers(&query_index);
        Ok(Self(Arc::new(Inner {
            query_index,
            reader,
            writer: Mutex::new(writer),
            fields,
            state: AtomicU8::new(state as u8),
        })))
    }

    /// Returns the current compatibility state.
    #[must_use]
    pub fn state(&self) -> IndexState {
        IndexState::from_u8(self.0.state.load(Ordering::Acquire))
    }

    /// Atomically applies a batch and makes it visible with one commit/reload.
    pub fn mutate(&self, mutations: impl IntoIterator<Item = SearchMutation>) -> Result<(), SearchError> {
        let mut writer = self.0.writer.lock().map_err(|_| SearchError::WriterPoisoned)?;
        for mutation in mutations {
            let result = match mutation {
                SearchMutation::Upsert(entity) => {
                    self.delete_kind_id(&mut writer, entity.kind, &entity.id);
                    self.document(entity)
                        .and_then(|document| writer.add_document(document).map(|_| ()).map_err(Into::into))
                }
                SearchMutation::Delete { kind, id } => {
                    self.delete_kind_id(&mut writer, kind, &id);
                    Ok(())
                }
            };
            if let Err(error) = result {
                writer.rollback()?;
                return Err(error);
            }
        }
        if let Err(error) = writer.commit() {
            writer.rollback()?;
            return Err(error.into());
        }
        self.0.reader.reload()?;
        Ok(())
    }

    /// Atomically replaces all documents of the selected kinds.
    pub fn replace(
        &self,
        kinds: &[EntityKind],
        entities: impl IntoIterator<Item = SearchEntity>,
    ) -> Result<(), SearchError> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        let batch = entities.into_iter().collect();
        sender.send(Ok(batch)).expect("new rebuild receiver must be connected");
        drop(sender);
        self.replace_stream(kinds, &receiver)
    }

    /// Atomically replaces selected kinds from a bounded streaming source.
    ///
    /// The caller should use a capacity-two channel. The writer remains
    /// uncommitted until the source closes, so searches continue to see the
    /// previously committed generation throughout the rebuild.
    pub fn replace_stream(&self, kinds: &[EntityKind], receiver: &Receiver<SearchBatch>) -> Result<(), SearchError> {
        let mut writer = self.0.writer.lock().map_err(|_| SearchError::WriterPoisoned)?;
        for kind in kinds {
            writer.delete_term(Term::from_field_text(self.0.fields.kind, kind.name()));
        }
        while let Ok(batch) = receiver.recv() {
            let batch = match batch {
                Ok(batch) => batch,
                Err(error) => {
                    writer.rollback()?;
                    return Err(SearchError::Source(error));
                }
            };
            for entity in batch {
                if !kinds.contains(&entity.kind) {
                    writer.rollback()?;
                    return Err(SearchError::InvalidDocument(format!(
                        "{} is outside the requested replacement kinds",
                        entity.kind.name()
                    )));
                }
                let document = match self.document(entity) {
                    Ok(document) => document,
                    Err(error) => {
                        writer.rollback()?;
                        return Err(error);
                    }
                };
                if let Err(error) = writer.add_document(document) {
                    writer.rollback()?;
                    return Err(error.into());
                }
            }
        }
        writer.delete_term(Term::from_field_text(self.0.fields.kind, "index_version"));
        if let Err(error) = writer.add_document(self.version_document()) {
            writer.rollback()?;
            return Err(error.into());
        }
        if let Err(error) = writer.commit() {
            writer.rollback()?;
            return Err(error.into());
        }
        self.0.reader.reload()?;
        self.0.state.store(IndexState::Current as u8, Ordering::Release);
        Ok(())
    }

    /// Adds or replaces one entity and commits it.
    pub fn upsert(&self, entity: SearchEntity) -> Result<(), SearchError> {
        self.mutate([SearchMutation::Upsert(entity)])
    }

    /// Deletes an identifier from every entity kind and commits once.
    pub fn delete(&self, id: &str) -> Result<(), SearchError> {
        self.mutate(ALL_KINDS.map(|kind| SearchMutation::Delete {
            kind,
            id: id.to_owned(),
        }))
    }

    /// Searches one entity kind. Invalid or unknown syntax returns no hits.
    #[must_use]
    pub fn search(&self, kind: EntityKind, input: &str) -> Vec<SearchHit> {
        if input.trim().is_empty() {
            return Vec::new();
        }
        let ast = match tantivy::query_grammar::parse_query(input) {
            Ok(mut ast) => {
                rewrite_dynamic_author_fields(&mut ast);
                normalize_negative_clauses(&mut ast);
                ast
            }
            Err(error) => {
                tracing::warn!(?error, query = input, "invalid search query");
                return Vec::new();
            }
        };
        let defaults = match kind {
            EntityKind::Book => vec![self.0.fields.named["title"], self.0.fields.named["isbn"]],
            EntityKind::Series => vec![self.0.fields.named["title"]],
            EntityKind::Collection | EntityKind::ReadList => vec![self.0.fields.named["name"]],
        };
        let mut parser = QueryParser::for_index(&self.0.query_index, defaults);
        parser.set_conjunction_by_default();
        let user_query = match parser.build_query_from_user_input_ast(ast) {
            Ok(query) => query,
            Err(error) => {
                tracing::warn!(%error, query = input, "invalid search query");
                return Vec::new();
            }
        };
        let type_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.0.fields.kind, kind.name()),
            IndexRecordOption::Basic,
        ));
        let query = BooleanQuery::new(vec![(Occur::Must, type_query), (Occur::Must, user_query)]);
        let searcher = self.0.reader.searcher();
        let top = match searcher.search(&query, &TopDocs::with_limit(RESULT_LIMIT).order_by_score()) {
            Ok(top) => top,
            Err(error) => {
                tracing::error!(%error, query = input, "search query execution failed");
                return Vec::new();
            }
        };
        top.into_iter()
            .filter_map(|(score, address)| {
                let document: TantivyDocument = searcher.doc(address).ok()?;
                let id = document.get_first(self.0.fields.ids[&kind])?.as_str()?.to_owned();
                Some(SearchHit { id, score })
            })
            .collect()
    }

    fn document(&self, entity: SearchEntity) -> Result<TantivyDocument, SearchError> {
        if entity.id.trim().is_empty() {
            return Err(SearchError::InvalidDocument("entity id must not be blank".into()));
        }
        let mut document = TantivyDocument::new();
        document.add_text(self.0.fields.kind, entity.kind.name());
        document.add_text(self.0.fields.ids[&entity.kind], entity.id);
        for (name, values) in entity.fields {
            let Some(field) = self.0.fields.named.get(&name).copied() else {
                return Err(SearchError::InvalidDocument(format!("unknown field `{name}`")));
            };
            for value in values {
                document.add_text(field, value);
            }
        }
        if !entity.authors.is_empty() {
            let authors = entity
                .authors
                .into_iter()
                .map(|(role, names)| {
                    (
                        role,
                        OwnedValue::Array(names.into_iter().map(OwnedValue::from).collect()),
                    )
                })
                .collect();
            document.add_object(self.0.fields.authors, authors);
        }
        Ok(document)
    }

    fn version_document(&self) -> TantivyDocument {
        let mut document = TantivyDocument::new();
        document.add_text(self.0.fields.kind, "index_version");
        document.add_text(self.0.fields.version, SCHEMA_VERSION.to_string());
        document
    }

    fn delete_kind_id(&self, writer: &mut IndexWriter, kind: EntityKind, id: &str) {
        writer.delete_term(Term::from_field_text(self.0.fields.ids[&kind], id));
    }

    fn committed_version(&self) -> Option<u32> {
        let query = TermQuery::new(
            Term::from_field_text(self.0.fields.kind, "index_version"),
            IndexRecordOption::Basic,
        );
        let searcher = self.0.reader.searcher();
        let (_, address) = searcher
            .search(&query, &TopDocs::with_limit(1).order_by_score())
            .ok()?
            .into_iter()
            .next()?;
        let document: TantivyDocument = searcher.doc(address).ok()?;
        document.get_first(self.0.fields.version)?.as_str()?.parse().ok()
    }
}

fn schema() -> Schema {
    let raw = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw_lower")
                .set_index_option(IndexRecordOption::Basic),
        )
        .set_stored();
    let text_indexing = TextFieldIndexing::default()
        .set_tokenizer("komga_text")
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let text = TextOptions::default().set_indexing_options(text_indexing.clone());
    let json = JsonObjectOptions::default().set_indexing_options(text_indexing);
    let mut builder = Schema::builder();
    builder.add_text_field("type", raw.clone());
    for kind in ALL_KINDS {
        builder.add_text_field(kind.id_field(), raw.clone());
    }
    builder.add_text_field("index_version", raw);
    for name in SEARCH_FIELDS {
        builder.add_text_field(name, text.clone());
    }
    builder.add_json_field("authors", json);
    builder.build()
}

const SEARCH_FIELDS: &[&str] = &[
    "title",
    "name",
    "isbn",
    "tag",
    "series_tag",
    "book_tag",
    "author",
    "release_date",
    "status",
    "deleted",
    "oneshot",
    "publisher",
    "reading_direction",
    "age_rating",
    "language",
    "genre",
    "sharing_label",
    "total_book_count",
    "book_count",
    "complete",
];

fn register_index_tokenizers(index: &Index) {
    let raw = TextAnalyzer::builder(RawTokenizer::default())
        .filter(LowerCaser)
        .build();
    index.tokenizers().register("raw_lower", raw);
    index.tokenizers().register("komga_text", index_analyzer());
}

fn register_query_tokenizers(index: &Index) {
    let raw = TextAnalyzer::builder(RawTokenizer::default())
        .filter(LowerCaser)
        .build();
    index.tokenizers().register("raw_lower", raw);
    index.tokenizers().register("komga_text", query_analyzer());
}

fn rewrite_dynamic_author_fields(ast: &mut UserInputAst) {
    match ast {
        UserInputAst::Clause(children) => {
            for (_, child) in children {
                rewrite_dynamic_author_fields(child);
            }
        }
        UserInputAst::Boost(child, _) => rewrite_dynamic_author_fields(child),
        UserInputAst::Leaf(leaf) => match leaf.as_mut() {
            UserInputLeaf::Literal(literal) => rewrite_field(&mut literal.field_name),
            UserInputLeaf::Range { field, .. }
            | UserInputLeaf::Set { field, .. }
            | UserInputLeaf::Regex { field, .. } => rewrite_field(field),
            UserInputLeaf::Exists { field } => {
                let mut value = Some(std::mem::take(field));
                rewrite_field(&mut value);
                *field = value.unwrap_or_default();
            }
            UserInputLeaf::All => {}
        },
    }
}

fn normalize_negative_clauses(ast: &mut UserInputAst) {
    // Lucene permits pure-negative clauses by implicitly matching every
    // document before applying the exclusions. Tantivy rejects those clauses.
    let UserInputAst::Clause(children) = ast else {
        if let UserInputAst::Boost(child, _) = ast {
            normalize_negative_clauses(child);
        }
        return;
    };

    for (_, child) in children.iter_mut() {
        normalize_negative_clauses(child);
    }
    if !children.is_empty() && children.iter().all(|(occur, _)| *occur == Some(Occur::MustNot)) {
        children.push((Some(Occur::Should), UserInputAst::Leaf(Box::new(UserInputLeaf::All))));
    }
}

fn rewrite_field(field: &mut Option<String>) {
    let Some(name) = field.as_mut() else {
        return;
    };
    if !known_query_fields().contains(name.as_str()) && !name.starts_with("authors.") {
        *name = format!("authors.{name}");
    }
}

fn known_query_fields() -> BTreeSet<&'static str> {
    SEARCH_FIELDS
        .iter()
        .copied()
        .chain(ALL_KINDS.into_iter().map(EntityKind::id_field))
        .chain(["type", "index_version", "authors"])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_fields_dynamic_roles_phrases_ranges_and_booleans() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .replace(
                &[EntityKind::Book, EntityKind::Series],
                [
                    SearchEntity::new(EntityKind::Book, "b1")
                        .field("title", "The Incredible Batman")
                        .field("isbn", "9782413016878")
                        .field("release_date", "2024")
                        .field("deleted", "false")
                        .author("writer", "Eric Dupont"),
                    SearchEntity::new(EntityKind::Book, "b2")
                        .field("title", "Batman Returns")
                        .field("release_date", "1992")
                        .field("deleted", "true")
                        .author("penciller", "Other Person"),
                    SearchEntity::new(EntityKind::Series, "s1").field("title", "Batman"),
                ],
            )
            .unwrap();
        assert_eq!(index.search(EntityKind::Book, "cred bat")[0].id, "b1");
        assert_eq!(index.search(EntityKind::Book, "\"incredible batman\"")[0].id, "b1");
        assert_eq!(index.search(EntityKind::Book, "writer:dup")[0].id, "b1");
        assert!(index.search(EntityKind::Book, "colorist:dup").is_empty());
        assert_eq!(
            index.search(EntityKind::Book, "release_date:[2020 TO 2025]")[0].id,
            "b1"
        );
        assert_eq!(index.search(EntityKind::Book, "deleted:false")[0].id, "b1");
        assert!(index.search(EntityKind::Book, "title:(").is_empty());
        assert_eq!(index.search(EntityKind::Series, "Batman").len(), 1);
    }

    #[test]
    fn kotlin_book_query_corpus_matches_fields_language_and_boolean_behavior() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .replace(
                &[EntityKind::Book],
                [
                    SearchEntity::new(EntityKind::Book, "batman")
                        .field("title", "Batman S.W.O.R.D X-Men")
                        .field("isbn", "9782413016878")
                        .field("tag", "tag1")
                        .field("release_date", "1999")
                        .field("status", "ERROR")
                        .field("deleted", "true")
                        .field("oneshot", "false")
                        .author("writer", "Bob"),
                    SearchEntity::new(EntityKind::Book, "batman-robin")
                        .field("title", "Batman and Robin")
                        .field("status", "READY")
                        .field("deleted", "false"),
                    SearchEntity::new(EntityKind::Book, "robin")
                        .field("title", "Robin")
                        .field("status", "READY")
                        .field("deleted", "false"),
                    SearchEntity::new(EntityKind::Book, "cjk")
                        .field("title", "不道德")
                        .field("deleted", "false"),
                ],
            )
            .unwrap();

        for (query, expected) in [
            ("9782413016878", &["batman"][..]),
            ("tag:tag1", &["batman"]),
            ("author:bob", &["batman"]),
            ("writer:bob", &["batman"]),
            ("release_date:1999", &["batman"]),
            ("release_date:[1990 TO 2010]", &["batman"]),
            ("status:error", &["batman"]),
            ("deleted:true", &["batman"]),
            ("s.w.o.r.d", &["batman"]),
            ("x-men", &["batman"]),
            ("不道德", &["cjk"]),
            ("batman robin", &["batman-robin"]),
            ("\"batman and robin\"", &["batman-robin"]),
            ("batman AND deleted:false", &["batman-robin"]),
            ("(batman OR robin) AND NOT deleted:true", &["batman-robin", "robin"]),
        ] {
            assert_eq!(hit_ids(&index, EntityKind::Book, query), expected, "query: {query}");
        }

        assert!(index.search(EntityKind::Book, "penciller:bob").is_empty());
        assert!(index.search(EntityKind::Book, "publisher:batman").is_empty());
        assert!(index.search(EntityKind::Book, "title:(").is_empty());
        assert!(index.search(EntityKind::Book, "   ").is_empty());
        assert_eq!(
            hit_id_set(&index, EntityKind::Book, "NOT deleted:true"),
            BTreeSet::from(["batman-robin".to_owned(), "cjk".to_owned(), "robin".to_owned()])
        );
        assert_eq!(
            hit_id_set(&index, EntityKind::Book, "robin OR NOT deleted:true"),
            BTreeSet::from(["batman-robin".to_owned(), "cjk".to_owned(), "robin".to_owned()])
        );
    }

    #[test]
    fn kotlin_series_query_corpus_matches_every_indexed_field() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .replace(
                &[EntityKind::Series],
                [
                    SearchEntity::new(EntityKind::Series, "batman")
                        .field("title", "Batman")
                        .field("publisher", "Vertigo")
                        .field("status", "HIATUS")
                        .field("reading_direction", "LEFT_TO_RIGHT")
                        .field("age_rating", "12")
                        .field("language", "en-us")
                        .field("series_tag", "seriestag")
                        .field("book_tag", "booktag")
                        .field("tag", "seriestag")
                        .field("tag", "booktag")
                        .field("genre", "action")
                        .field("sharing_label", "family")
                        .field("total_book_count", "5")
                        .field("book_count", "2")
                        .field("release_date", "1999")
                        .field("deleted", "true")
                        .field("oneshot", "false")
                        .field("complete", "false")
                        .author("penciller", "David"),
                    SearchEntity::new(EntityKind::Series, "robin")
                        .field("title", "Batman and Robin")
                        .field("deleted", "false"),
                ],
            )
            .unwrap();

        for query in [
            "publisher:vertigo",
            "status:hiatus",
            "reading_direction:left_to_right",
            "age_rating:12",
            "language:en-us",
            "book_tag:booktag",
            "series_tag:seriestag",
            "tag:booktag",
            "tag:seriestag",
            "genre:action",
            "sharing_label:family",
            "total_book_count:5",
            "book_count:2",
            "author:david",
            "penciller:david",
            "release_date:1999",
            "deleted:true",
            "oneshot:false",
            "complete:false",
        ] {
            assert_eq!(hit_ids(&index, EntityKind::Series, query), ["batman"], "query: {query}");
        }
        assert!(index.search(EntityKind::Series, "writer:david").is_empty());
        assert!(index.search(EntityKind::Series, "book_tag:seriestag").is_empty());
        assert!(index.search(EntityKind::Series, "series_tag:booktag").is_empty());
    }

    #[test]
    fn kotlin_relevance_order_and_entity_defaults_are_preserved() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .replace(
                &ALL_KINDS,
                [
                    SearchEntity::new(EntityKind::Book, "long").field(
                        "title",
                        "The incredible adventures of Batman, the man who is also a bat!",
                    ),
                    SearchEntity::new(EntityKind::Book, "robin").field("title", "Robin"),
                    SearchEntity::new(EntityKind::Book, "both").field("title", "Batman and Robin"),
                    SearchEntity::new(EntityKind::Book, "exact").field("title", "Batman"),
                    SearchEntity::new(EntityKind::Series, "series").field("title", "Batman"),
                    SearchEntity::new(EntityKind::Collection, "collection").field("name", "Batman"),
                    SearchEntity::new(EntityKind::ReadList, "readlist").field("name", "Batman"),
                ],
            )
            .unwrap();

        assert_eq!(hit_ids(&index, EntityKind::Book, "batman"), ["exact", "both", "long"]);
        assert_eq!(hit_ids(&index, EntityKind::Series, "batman"), ["series"]);
        assert_eq!(hit_ids(&index, EntityKind::Collection, "batman"), ["collection"]);
        assert_eq!(hit_ids(&index, EntityKind::ReadList, "batman"), ["readlist"]);
    }

    fn hit_ids(index: &SearchIndex, kind: EntityKind, query: &str) -> Vec<String> {
        index.search(kind, query).into_iter().map(|hit| hit.id).collect()
    }

    fn hit_id_set(index: &SearchIndex, kind: EntityKind, query: &str) -> BTreeSet<String> {
        hit_ids(index, kind, query).into_iter().collect()
    }

    #[test]
    fn mutations_are_atomic_and_failed_batches_roll_back() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .upsert(SearchEntity::new(EntityKind::Book, "one").field("title", "Old title"))
            .unwrap();
        let error = index.mutate([
            SearchMutation::Upsert(SearchEntity::new(EntityKind::Book, "one").field("title", "New title")),
            SearchMutation::Upsert(SearchEntity::new(EntityKind::Book, "").field("title", "Invalid")),
        ]);
        assert!(error.is_err());
        assert_eq!(index.search(EntityKind::Book, "old")[0].id, "one");
        assert!(index.search(EntityKind::Book, "new").is_empty());
    }

    #[test]
    fn streaming_rebuild_keeps_old_reader_until_commit_and_rolls_back_source_errors() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .replace(
                &[EntityKind::Book],
                [SearchEntity::new(EntityKind::Book, "old").field("title", "Old")],
            )
            .unwrap();
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        sender
            .send(Ok(vec![
                SearchEntity::new(EntityKind::Book, "new").field("title", "New"),
            ]))
            .unwrap();
        sender.send(Err("database failed".into())).unwrap();
        drop(sender);
        assert!(index.replace_stream(&[EntityKind::Book], &receiver).is_err());
        assert_eq!(index.search(EntityKind::Book, "old")[0].id, "old");
        assert!(index.search(EntityKind::Book, "new").is_empty());
    }

    #[test]
    fn file_index_uses_v2_and_tracks_version_document() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("v1")).unwrap();
        let index = SearchIndex::open(directory.path()).unwrap();
        assert_eq!(index.state(), IndexState::Empty);
        index.replace(&ALL_KINDS, []).unwrap();
        assert_eq!(index.state(), IndexState::Current);
        drop(index);
        assert_eq!(
            SearchIndex::open(directory.path()).unwrap().state(),
            IndexState::Current
        );
        assert!(directory.path().join("v1").exists());
    }

    #[test]
    fn result_count_is_capped() {
        let index = SearchIndex::in_memory().unwrap();
        let documents =
            (0..1_010).map(|id| SearchEntity::new(EntityKind::Book, id.to_string()).field("title", "Common"));
        index.replace(&[EntityKind::Book], documents).unwrap();
        assert_eq!(index.search(EntityKind::Book, "common").len(), RESULT_LIMIT);
    }

    #[test]
    fn bm25_hits_are_returned_in_descending_score_order() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .replace(
                &[EntityKind::Book],
                [
                    SearchEntity::new(EntityKind::Book, "one").field("title", "alpha beta"),
                    SearchEntity::new(EntityKind::Book, "two").field("title", "alpha alpha alpha beta gamma"),
                ],
            )
            .unwrap();
        let hits = index.search(EntityKind::Book, "alpha");
        assert_eq!(hits.len(), 2);
        assert!(hits[0].score >= hits[1].score);
        assert_ne!(hits[0].score, hits[1].score);
    }

    #[test]
    fn existing_index_without_version_document_is_incompatible() {
        let directory = tempfile::tempdir().unwrap();
        let index = SearchIndex::open(directory.path()).unwrap();
        index
            .upsert(SearchEntity::new(EntityKind::Book, "book").field("title", "Title"))
            .unwrap();
        drop(index);
        assert_eq!(
            SearchIndex::open(directory.path()).unwrap().state(),
            IndexState::Incompatible
        );
    }

    #[test]
    fn mismatched_v2_schema_is_recreated_without_touching_v1() {
        let directory = tempfile::tempdir().unwrap();
        let v1 = directory.path().join("v1");
        let v2 = directory.path().join("v2");
        std::fs::create_dir(&v1).unwrap();
        std::fs::write(v1.join("marker"), "legacy").unwrap();
        std::fs::create_dir(&v2).unwrap();

        let mut builder = Schema::builder();
        builder.add_text_field("legacy", TextOptions::default());
        let legacy = Index::create(
            MmapDirectory::open(&v2).unwrap(),
            builder.build(),
            tantivy::IndexSettings::default(),
        )
        .unwrap();
        drop(legacy);

        let index = SearchIndex::open(directory.path()).unwrap();
        assert_eq!(index.state(), IndexState::Incompatible);
        assert_eq!(std::fs::read_to_string(v1.join("marker")).unwrap(), "legacy");
        index.replace(&ALL_KINDS, []).unwrap();
        assert_eq!(index.state(), IndexState::Current);
    }
}

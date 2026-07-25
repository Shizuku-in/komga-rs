//! Rebuildable in-process full-text search for Komga entities.

#![warn(missing_docs)]

use std::{collections::BTreeMap, path::Path, sync::Arc};

use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term,
    collector::TopDocs,
    directory::MmapDirectory,
    query::QueryParser,
    schema::{Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value},
    tokenizer::{AsciiFoldingFilter, LowerCaser, NgramTokenizer, RawTokenizer, TextAnalyzer},
};
use thiserror::Error;

/// On-disk schema version. A different version requires a full rebuild.
pub const SCHEMA_VERSION: u32 = 1;
/// Maximum number of ranked identifiers returned by one query.
pub const RESULT_LIMIT: usize = 1000;

/// Searchable entity kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            Self::ReadList => "readList",
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
}

impl SearchEntity {
    /// Creates an empty document.
    pub fn new(kind: EntityKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            fields: BTreeMap::new(),
        }
    }

    /// Appends a searchable field value.
    #[must_use]
    pub fn field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.fields.entry(name.into()).or_default().push(value.into());
        self
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
}

struct Fields {
    kind: Field,
    id: Field,
    all: Field,
    title: Field,
    name: Field,
    isbn: Field,
    named: BTreeMap<String, Field>,
}

struct Inner {
    index: Index,
    reader: IndexReader,
    writer: std::sync::Mutex<IndexWriter>,
    fields: Fields,
}

/// Cloneable Tantivy index. Commits become visible immediately after reload.
#[derive(Clone)]
pub struct SearchIndex(Arc<Inner>);

impl SearchIndex {
    /// Creates a process-local index.
    pub fn in_memory() -> Result<Self, SearchError> {
        Self::from_index(Index::create_in_ram(schema()))
    }

    /// Opens or creates a file-backed index.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SearchError> {
        let versioned_path = path.as_ref().join(format!("v{SCHEMA_VERSION}"));
        std::fs::create_dir_all(&versioned_path).map_err(|error| {
            tantivy::directory::error::OpenDirectoryError::wrap_io_error(error, versioned_path.clone())
        })?;
        let directory = MmapDirectory::open(versioned_path)?;
        let index = Index::open_or_create(directory, schema())?;
        Self::from_index(index)
    }

    fn from_index(index: Index) -> Result<Self, SearchError> {
        register_tokenizers(&index);
        let schema = index.schema();
        let named = SEARCH_FIELDS
            .iter()
            .map(|name| Ok(((*name).to_owned(), schema.get_field(name)?)))
            .collect::<tantivy::Result<BTreeMap<_, _>>>()?;
        let fields = Fields {
            kind: schema.get_field("type")?,
            id: schema.get_field("id")?,
            all: schema.get_field("all")?,
            title: schema.get_field("title")?,
            name: schema.get_field("name")?,
            isbn: schema.get_field("isbn")?,
            named,
        };
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        let writer = index.writer(50_000_000)?;
        Ok(Self(Arc::new(Inner {
            index,
            reader,
            writer: std::sync::Mutex::new(writer),
            fields,
        })))
    }

    /// Atomically replaces all documents of the selected kinds.
    pub fn replace(
        &self,
        kinds: &[EntityKind],
        entities: impl IntoIterator<Item = SearchEntity>,
    ) -> Result<(), SearchError> {
        let mut writer = self.0.writer.lock().expect("search writer poisoned");
        for kind in kinds {
            writer.delete_term(Term::from_field_text(self.0.fields.kind, kind.name()));
        }
        for entity in entities {
            writer.add_document(self.document(entity))?;
        }
        writer.commit()?;
        self.0.reader.reload()?;
        Ok(())
    }

    /// Adds or replaces one entity and commits it.
    pub fn upsert(&self, entity: SearchEntity) -> Result<(), SearchError> {
        let mut writer = self.0.writer.lock().expect("search writer poisoned");
        writer.delete_term(Term::from_field_text(self.0.fields.id, &entity.id));
        writer.add_document(self.document(entity))?;
        writer.commit()?;
        self.0.reader.reload()?;
        Ok(())
    }

    /// Deletes one entity and commits it.
    pub fn delete(&self, id: &str) -> Result<(), SearchError> {
        let mut writer = self.0.writer.lock().expect("search writer poisoned");
        writer.delete_term(Term::from_field_text(self.0.fields.id, id));
        writer.commit()?;
        self.0.reader.reload()?;
        Ok(())
    }

    /// Searches one entity kind. Invalid or unknown syntax returns no hits.
    pub fn search(&self, kind: EntityKind, input: &str) -> Vec<SearchHit> {
        if input.trim().is_empty() {
            return Vec::new();
        }
        let defaults = match kind {
            EntityKind::Book => vec![self.0.fields.title, self.0.fields.isbn],
            EntityKind::Series => vec![self.0.fields.title],
            EntityKind::Collection | EntityKind::ReadList => vec![self.0.fields.name],
        };
        let mut parser = QueryParser::for_index(&self.0.index, defaults);
        parser.set_conjunction_by_default();
        let query_text = format!("type:{} AND ({input})", kind.name());
        let query = match parser.parse_query(&query_text) {
            Ok(query) => query,
            Err(error) => {
                tracing::warn!(%error, query = input, "invalid search query");
                return Vec::new();
            }
        };
        let searcher = self.0.reader.searcher();
        let collector = TopDocs::with_limit(RESULT_LIMIT).order_by_score();
        let top = match searcher.search(&query, &collector) {
            Ok(top) => top,
            Err(error) => {
                tracing::error!(%error, query = input, "search query execution failed");
                return Vec::new();
            }
        };
        top.into_iter()
            .filter_map(|(score, address)| {
                let doc: TantivyDocument = searcher.doc(address).ok()?;
                let id = doc.get_first(self.0.fields.id)?.as_str()?.to_owned();
                Some(SearchHit { id, score })
            })
            .collect()
    }

    fn document(&self, entity: SearchEntity) -> TantivyDocument {
        let mut doc = TantivyDocument::new();
        doc.add_text(self.0.fields.kind, entity.kind.name());
        doc.add_text(self.0.fields.id, &entity.id);
        for (name, values) in entity.fields {
            let target = self.0.fields.named.get(&name).copied().unwrap_or(self.0.fields.all);
            for value in values {
                let value = normalize_cjk(&value);
                doc.add_text(target, &value);
                doc.add_text(self.0.fields.all, value);
            }
        }
        doc
    }
}

fn schema() -> Schema {
    let raw = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw_lower")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("komga_ngram")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let mut builder = Schema::builder();
    builder.add_text_field("type", raw.clone());
    builder.add_text_field("id", raw);
    for name in SEARCH_FIELDS {
        builder.add_text_field(name, text.clone());
    }
    builder.build()
}

const SEARCH_FIELDS: &[&str] = &[
    "all",
    "title",
    "titleSort",
    "alternateTitle",
    "name",
    "isbn",
    "releaseYear",
    "deleted",
    "oneshot",
    "complete",
    "author",
    "tag",
    "publisher",
    "status",
    "readingDirection",
    "ageRating",
    "language",
    "genre",
    "sharingLabel",
    "bookCount",
    "mediaStatus",
];

fn register_tokenizers(index: &Index) {
    let raw = TextAnalyzer::builder(RawTokenizer::default())
        .filter(LowerCaser)
        .build();
    index.tokenizers().register("raw_lower", raw);
    let ngram = TextAnalyzer::builder(NgramTokenizer::new(1, 10, false).expect("valid ngram bounds"))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build();
    index.tokenizers().register("komga_ngram", ngram);
}

fn normalize_cjk(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    value.nfkc().collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_unicode_fields_and_replaces_documents() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .upsert(
                SearchEntity::new(EntityKind::Book, "b1")
                    .field("title", "L'ete 東京喰種")
                    .field("isbn", "978-1-23"),
            )
            .unwrap();
        assert_eq!(index.search(EntityKind::Book, "ete")[0].id, "b1");
        assert_eq!(index.search(EntityKind::Book, "東京")[0].id, "b1");
        assert_eq!(index.search(EntityKind::Book, "isbn:978-1-23")[0].id, "b1");
        index
            .upsert(SearchEntity::new(EntityKind::Book, "b1").field("title", "Changed"))
            .unwrap();
        assert!(index.search(EntityKind::Book, "ete").is_empty());
    }

    #[test]
    fn isolates_kinds_and_rejects_unknown_fields() {
        let index = SearchIndex::in_memory().unwrap();
        index
            .replace(
                &[EntityKind::Series, EntityKind::Collection],
                [
                    SearchEntity::new(EntityKind::Series, "s").field("title", "Alpha"),
                    SearchEntity::new(EntityKind::Collection, "c").field("name", "Alpha"),
                ],
            )
            .unwrap();
        assert_eq!(index.search(EntityKind::Series, "Alpha").len(), 1);
        assert_eq!(index.search(EntityKind::Collection, "Alpha").len(), 1);
        assert!(index.search(EntityKind::Series, "missing:value").is_empty());
    }
}

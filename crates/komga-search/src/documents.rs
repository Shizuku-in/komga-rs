use komga_core::{BookDto, CollectionDto, ReadListDto, SeriesDto};

use crate::{EntityKind, SearchEntity};

/// Builds the complete search document for a book.
///
/// A one-shot book inherits series-level metadata when its series projection is
/// supplied, matching Komga's Lucene document composition.
#[must_use]
pub fn book_document(book: &BookDto, series: Option<&SeriesDto>) -> SearchEntity {
    let mut document = SearchEntity::new(EntityKind::Book, &book.id)
        .field("title", &book.metadata.title)
        .field("isbn", &book.metadata.isbn)
        .field("status", &book.media.status)
        .field("deleted", book.deleted.to_string())
        .field("oneshot", book.oneshot.to_string());
    if let Some(year) = year(book.metadata.release_date.as_deref()) {
        document = document.field("release_date", year);
    }
    for tag in &book.metadata.tags {
        document = document.field("tag", tag);
    }
    for author in &book.metadata.authors {
        document = document.author(&author.role, &author.name);
    }
    if book.oneshot
        && let Some(series) = series
    {
        document = inherit_oneshot(document, series);
    }
    document
}

/// Builds the complete search document for a series.
#[must_use]
pub fn series_document(series: &SeriesDto) -> SearchEntity {
    let mut document = SearchEntity::new(EntityKind::Series, &series.id)
        .field("title", &series.metadata.title)
        .field("publisher", &series.metadata.publisher)
        .field("status", &series.metadata.status)
        .field("reading_direction", &series.metadata.reading_direction)
        .field("language", &series.metadata.language)
        .field("book_count", series.books_count.to_string())
        .field("deleted", series.deleted.to_string())
        .field("oneshot", series.oneshot.to_string());
    if series.metadata.title_sort != series.metadata.title {
        document = document.field("title", &series.metadata.title_sort);
    }
    for alternate in &series.metadata.alternate_titles {
        document = document.field("title", &alternate.title);
    }
    if let Some(value) = series.metadata.age_rating {
        document = document.field("age_rating", value.to_string());
    }
    if let Some(value) = series.metadata.total_book_count {
        document = document
            .field("total_book_count", value.to_string())
            .field("complete", (value == series.books_count).to_string());
    }
    for tag in &series.metadata.tags {
        document = document.field("series_tag", tag).field("tag", tag);
    }
    for tag in &series.books_metadata.tags {
        document = document.field("book_tag", tag).field("tag", tag);
    }
    for genre in &series.metadata.genres {
        document = document.field("genre", genre);
    }
    for label in &series.metadata.sharing_labels {
        document = document.field("sharing_label", label);
    }
    for author in &series.books_metadata.authors {
        document = document.author(&author.role, &author.name);
    }
    if let Some(year) = year(series.books_metadata.release_date.as_deref()) {
        document = document.field("release_date", year);
    }
    document
}

/// Builds a collection search document.
#[must_use]
pub fn collection_document(collection: &CollectionDto) -> SearchEntity {
    SearchEntity::new(EntityKind::Collection, &collection.id).field("name", &collection.name)
}

/// Builds a read-list search document.
#[must_use]
pub fn read_list_document(read_list: &ReadListDto) -> SearchEntity {
    SearchEntity::new(EntityKind::ReadList, &read_list.id).field("name", &read_list.name)
}

fn inherit_oneshot(mut document: SearchEntity, series: &SeriesDto) -> SearchEntity {
    document = document
        .field("publisher", &series.metadata.publisher)
        .field("status", &series.metadata.status)
        .field("reading_direction", &series.metadata.reading_direction)
        .field("language", &series.metadata.language)
        .field("complete", "true");
    if let Some(value) = series.metadata.age_rating {
        document = document.field("age_rating", value.to_string());
    }
    for genre in &series.metadata.genres {
        document = document.field("genre", genre);
    }
    for label in &series.metadata.sharing_labels {
        document = document.field("sharing_label", label);
    }
    document
}

fn year(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| value.get(..4))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use komga_core::{
        AlternateTitleDto, AuthorDto, BookMetadataAggregationDto, BookMetadataDto, MediaDto, SeriesMetadataDto,
    };

    use super::*;

    #[test]
    fn builders_cover_kotlin_fields_and_oneshot_inheritance() {
        let series = rich_series();
        let book = rich_book();
        let document = book_document(&book, Some(&series));
        assert_eq!(document.fields["title"], ["Book title"]);
        assert_eq!(document.fields["isbn"], ["9781234567890"]);
        assert_eq!(document.fields["release_date"], ["2024"]);
        assert_eq!(document.fields["tag"], ["book-tag"]);
        assert_eq!(document.fields["author"], ["Book Writer"]);
        assert_eq!(document.authors["writer"], ["Book Writer"]);
        assert_eq!(document.fields["status"], ["READY", "ONGOING"]);
        assert_eq!(document.fields["publisher"], ["Publisher"]);
        assert_eq!(document.fields["reading_direction"], ["LEFT_TO_RIGHT"]);
        assert_eq!(document.fields["age_rating"], ["16"]);
        assert_eq!(document.fields["language"], ["en"]);
        assert_eq!(document.fields["genre"], ["Adventure"]);
        assert_eq!(document.fields["sharing_label"], ["family"]);
        assert_eq!(document.fields["complete"], ["true"]);

        let document = series_document(&series);
        assert_eq!(document.fields["title"], ["Series title", "Series sort", "Alternate"]);
        assert_eq!(document.fields["series_tag"], ["series-tag"]);
        assert_eq!(document.fields["book_tag"], ["aggregate-tag"]);
        assert_eq!(document.fields["tag"], ["series-tag", "aggregate-tag"]);
        assert_eq!(document.fields["total_book_count"], ["1"]);
        assert_eq!(document.fields["book_count"], ["1"]);
        assert_eq!(document.fields["complete"], ["true"]);
        assert_eq!(document.fields["release_date"], ["2023"]);
        assert_eq!(document.authors["penciller"], ["Series Artist"]);
    }

    fn rich_book() -> BookDto {
        BookDto {
            id: "book".into(),
            series_id: "series".into(),
            series_title: "Series title".into(),
            library_id: "library".into(),
            name: "book.cbz".into(),
            url: "file:/book.cbz".into(),
            number: 1,
            created: String::new(),
            last_modified: String::new(),
            file_last_modified: String::new(),
            size_bytes: 1,
            size: "1 B".into(),
            media: MediaDto {
                status: "READY".into(),
                media_type: String::new(),
                pages_count: 1,
                comment: String::new(),
                media_profile: String::new(),
                epub_divina_compatible: false,
                epub_is_kepub: false,
            },
            metadata: BookMetadataDto {
                title: "Book title".into(),
                title_lock: false,
                summary: String::new(),
                summary_lock: false,
                number: "1".into(),
                number_lock: false,
                number_sort: 1.0,
                number_sort_lock: false,
                release_date: Some("2024-02-03".into()),
                release_date_lock: false,
                authors: vec![AuthorDto {
                    name: "Book Writer".into(),
                    role: "writer".into(),
                }],
                authors_lock: false,
                tags: BTreeSet::from(["book-tag".into()]),
                tags_lock: false,
                isbn: "9781234567890".into(),
                isbn_lock: false,
                links: Vec::new(),
                links_lock: false,
                created: String::new(),
                last_modified: String::new(),
            },
            read_progress: None,
            deleted: false,
            file_hash: String::new(),
            oneshot: true,
        }
    }

    fn rich_series() -> SeriesDto {
        SeriesDto {
            id: "series".into(),
            library_id: "library".into(),
            name: "Series".into(),
            url: "file:/series".into(),
            created: String::new(),
            last_modified: String::new(),
            file_last_modified: String::new(),
            books_count: 1,
            books_read_count: 0,
            books_unread_count: 1,
            books_in_progress_count: 0,
            metadata: SeriesMetadataDto {
                status: "ONGOING".into(),
                status_lock: false,
                title: "Series title".into(),
                title_lock: false,
                title_sort: "Series sort".into(),
                title_sort_lock: false,
                summary: String::new(),
                summary_lock: false,
                reading_direction: "LEFT_TO_RIGHT".into(),
                reading_direction_lock: false,
                publisher: "Publisher".into(),
                publisher_lock: false,
                age_rating: Some(16),
                age_rating_lock: false,
                language: "en".into(),
                language_lock: false,
                genres: BTreeSet::from(["Adventure".into()]),
                genres_lock: false,
                tags: BTreeSet::from(["series-tag".into()]),
                tags_lock: false,
                total_book_count: Some(1),
                total_book_count_lock: false,
                sharing_labels: BTreeSet::from(["family".into()]),
                sharing_labels_lock: false,
                links: Vec::new(),
                links_lock: false,
                alternate_titles: vec![AlternateTitleDto {
                    label: "alt".into(),
                    title: "Alternate".into(),
                }],
                alternate_titles_lock: false,
                created: String::new(),
                last_modified: String::new(),
            },
            books_metadata: BookMetadataAggregationDto {
                authors: vec![AuthorDto {
                    name: "Series Artist".into(),
                    role: "penciller".into(),
                }],
                tags: BTreeSet::from(["aggregate-tag".into()]),
                release_date: Some("2023-01-01".into()),
                summary: String::new(),
                summary_number: String::new(),
                created: String::new(),
                last_modified: String::new(),
            },
            deleted: false,
            oneshot: true,
        }
    }
}

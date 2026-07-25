//! Referential values used to populate content filter controls.

use std::collections::BTreeSet;

use komga_core::{AuthorDto, ContentPage, PageRequest, SortDirection, SortOrder, User};
use sqlx::{QueryBuilder, Sqlite};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::{
    Result, Storage, StorageError,
    query_support::{content_page, push_library_visibility, validate_page},
};

/// Scope selected by referential endpoint query parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferentialScope {
    /// All authorized libraries.
    Global,
    /// An explicit set of libraries, intersected with authorization.
    Libraries(Vec<String>),
    /// Series belonging to one collection.
    Collection(String),
    /// One series.
    Series(String),
    /// Series represented by books in one read list.
    ReadList(String),
}

impl Storage {
    /// Lists v1 authors, preserving its global Book-metadata source difference.
    ///
    /// # Errors
    ///
    /// Returns a database error when author relations cannot be queried.
    pub async fn referential_v1_authors(
        &self,
        scope: &ReferentialScope,
        search: &str,
        user: &User,
    ) -> Result<Vec<AuthorDto>> {
        let rows = self
            .author_rows(scope, user, matches!(scope, ReferentialScope::Global))
            .await?;
        Ok(filter_authors(rows, Some(search), None))
    }

    /// Lists and paginates v2 authors from Series author aggregation.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for invalid paging, or a database error
    /// when author relations cannot be queried.
    pub async fn referential_v2_authors(
        &self,
        scope: &ReferentialScope,
        search: Option<&str>,
        role: Option<&str>,
        page: &PageRequest,
        user: &User,
    ) -> Result<ContentPage<AuthorDto>> {
        validate_page(page)?;
        let rows = self.author_rows(scope, user, false).await?;
        let rows = filter_authors(rows, search, role);
        let total =
            u64::try_from(rows.len()).map_err(|_| StorageError::InvalidData("author count is too large".into()))?;
        let content = if page.unpaged {
            rows
        } else {
            let start = usize::try_from(page.page * page.size)
                .map_err(|_| StorageError::InvalidQuery("page offset is too large".into()))?;
            let size =
                usize::try_from(page.size).map_err(|_| StorageError::InvalidQuery("page size is too large".into()))?;
            rows.into_iter().skip(start).take(size).collect()
        };
        Ok(content_page(content, total, page))
    }

    async fn author_rows(
        &self,
        scope: &ReferentialScope,
        user: &User,
        direct_book_metadata: bool,
    ) -> Result<Vec<AuthorDto>> {
        let mut query = if direct_book_metadata {
            QueryBuilder::new(
                "SELECT DISTINCT authors.NAME, authors.ROLE FROM BOOK_METADATA_AUTHOR authors \
                 JOIN BOOK b ON b.ID = authors.BOOK_ID WHERE 1 = 1",
            )
        } else {
            let mut query = QueryBuilder::new(
                "SELECT DISTINCT authors.NAME, authors.ROLE FROM BOOK_METADATA_AGGREGATION_AUTHOR authors \
                 JOIN SERIES s ON s.ID = authors.SERIES_ID ",
            );
            match scope {
                ReferentialScope::Collection(_) => {
                    query.push("JOIN COLLECTION_SERIES cs ON cs.SERIES_ID = authors.SERIES_ID ");
                }
                ReferentialScope::ReadList(_) => {
                    query.push(
                        "JOIN BOOK b ON b.SERIES_ID = authors.SERIES_ID \
                         JOIN READLIST_BOOK rb ON rb.BOOK_ID = b.ID ",
                    );
                }
                _ => {}
            }
            query.push("WHERE 1 = 1");
            query
        };

        if direct_book_metadata {
            push_library_visibility(&mut query, "b.LIBRARY_ID", user);
        } else {
            push_library_visibility(&mut query, "s.LIBRARY_ID", user);
            push_series_scope(&mut query, scope, "s.LIBRARY_ID", "authors.SERIES_ID");
        }
        let rows = query.build_query_as::<(String, String)>().fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(name, role)| AuthorDto { name, role }).collect())
    }

    /// Lists v1 author names from direct Book metadata.
    ///
    /// # Errors
    ///
    /// Returns a database error when author relations cannot be queried.
    pub async fn referential_author_names(&self, search: &str, user: &User) -> Result<Vec<String>> {
        let authors = self.author_rows(&ReferentialScope::Global, user, true).await?;
        let mut names = authors
            .into_iter()
            .filter(|author| contains_normalized(&author.name, search))
            .map(|author| author.name)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        sort_strings(&mut names);
        Ok(names)
    }

    /// Lists v1 author roles from direct Book metadata.
    ///
    /// # Errors
    ///
    /// Returns a database error when author relations cannot be queried.
    pub async fn referential_author_roles(&self, user: &User) -> Result<Vec<String>> {
        let authors = self.author_rows(&ReferentialScope::Global, user, true).await?;
        let mut roles = authors
            .into_iter()
            .map(|author| author.role)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        roles.sort();
        Ok(roles)
    }

    /// Lists genres for a global, library, or collection scope.
    pub async fn referential_genres(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<String>> {
        self.series_string_values("SERIES_METADATA_GENRE", "GENRE", scope, user, false)
            .await
    }

    /// Lists sharing labels for a global, library, or collection scope.
    pub async fn referential_sharing_labels(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<String>> {
        self.series_string_values("SERIES_METADATA_SHARING", "LABEL", scope, user, false)
            .await
    }

    /// Lists Series metadata tags for a global, library, or collection scope.
    pub async fn referential_series_tags(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<String>> {
        self.series_string_values("SERIES_METADATA_TAG", "TAG", scope, user, false)
            .await
    }

    /// Lists non-empty Series metadata languages.
    pub async fn referential_languages(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<String>> {
        self.series_string_values("SERIES_METADATA", "LANGUAGE", scope, user, true)
            .await
    }

    /// Lists non-empty Series metadata publishers.
    pub async fn referential_publishers(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<String>> {
        self.series_string_values("SERIES_METADATA", "PUBLISHER", scope, user, true)
            .await
    }

    async fn series_string_values(
        &self,
        table: &str,
        column: &str,
        scope: &ReferentialScope,
        user: &User,
        exclude_empty: bool,
    ) -> Result<Vec<String>> {
        let mut query = QueryBuilder::new("SELECT DISTINCT values_table.");
        query
            .push(column)
            .push(" AS value FROM ")
            .push(table)
            .push(" values_table JOIN SERIES s ON s.ID = values_table.SERIES_ID ");
        if matches!(scope, ReferentialScope::Collection(_)) {
            query.push("JOIN COLLECTION_SERIES cs ON cs.SERIES_ID = values_table.SERIES_ID ");
        }
        query.push("WHERE 1 = 1");
        if exclude_empty {
            query.push(" AND values_table.").push(column).push(" <> ''");
        }
        push_library_visibility(&mut query, "s.LIBRARY_ID", user);
        push_series_scope(&mut query, scope, "s.LIBRARY_ID", "values_table.SERIES_ID");
        let mut values = query.build_query_scalar::<String>().fetch_all(&self.pool).await?;
        sort_strings(&mut values);
        Ok(values)
    }

    /// Lists direct Book tags for a global, library, series, or read-list scope.
    pub async fn referential_book_tags(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<String>> {
        let mut query = QueryBuilder::new(
            "SELECT DISTINCT tags.TAG FROM BOOK_METADATA_TAG tags JOIN BOOK b ON b.ID = tags.BOOK_ID ",
        );
        if matches!(scope, ReferentialScope::ReadList(_)) {
            query.push("JOIN READLIST_BOOK rb ON rb.BOOK_ID = tags.BOOK_ID ");
        }
        query.push("WHERE 1 = 1");
        push_library_visibility(&mut query, "b.LIBRARY_ID", user);
        match scope {
            ReferentialScope::Global => {}
            ReferentialScope::Libraries(ids) => push_values(&mut query, "b.LIBRARY_ID", ids),
            ReferentialScope::Series(id) => {
                query.push(" AND b.SERIES_ID = ").push_bind(id.clone());
            }
            ReferentialScope::ReadList(id) => {
                query.push(" AND rb.READLIST_ID = ").push_bind(id.clone());
            }
            ReferentialScope::Collection(_) => {
                query.push(" AND 0 = 1");
            }
        }
        let mut values = query.build_query_scalar::<String>().fetch_all(&self.pool).await?;
        sort_strings(&mut values);
        Ok(values)
    }

    /// Lists the union of Book and Series tags, using aggregation for collections.
    pub async fn referential_tags(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<String>> {
        let mut values = if matches!(scope, ReferentialScope::Collection(_)) {
            self.aggregated_book_tags(scope, user).await?
        } else {
            self.referential_book_tags(scope, user).await?
        };
        values.extend(self.referential_series_tags(scope, user).await?);
        values.sort();
        values.dedup();
        sort_strings(&mut values);
        Ok(values)
    }

    async fn aggregated_book_tags(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<String>> {
        let mut query = QueryBuilder::new(
            "SELECT DISTINCT tags.TAG FROM BOOK_METADATA_AGGREGATION_TAG tags \
             JOIN SERIES s ON s.ID = tags.SERIES_ID \
             JOIN COLLECTION_SERIES cs ON cs.SERIES_ID = tags.SERIES_ID WHERE 1 = 1",
        );
        push_library_visibility(&mut query, "s.LIBRARY_ID", user);
        push_series_scope(&mut query, scope, "s.LIBRARY_ID", "tags.SERIES_ID");
        Ok(query.build_query_scalar::<String>().fetch_all(&self.pool).await?)
    }

    /// Lists nullable age ratings in ascending order, with null first.
    pub async fn referential_age_ratings(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<Option<i32>>> {
        let mut query = scoped_series_metadata_query("sm.AGE_RATING", scope);
        push_library_visibility(&mut query, "s.LIBRARY_ID", user);
        push_series_scope(&mut query, scope, "s.LIBRARY_ID", "sm.SERIES_ID");
        query.push(" ORDER BY sm.AGE_RATING ASC");
        Ok(query.build_query_scalar::<Option<i32>>().fetch_all(&self.pool).await?)
    }

    /// Lists distinct aggregated release years in descending order.
    pub async fn referential_release_years(&self, scope: &ReferentialScope, user: &User) -> Result<Vec<i32>> {
        let mut query = QueryBuilder::new(
            "SELECT DISTINCT bma.RELEASE_DATE FROM BOOK_METADATA_AGGREGATION bma \
             JOIN SERIES s ON s.ID = bma.SERIES_ID ",
        );
        if matches!(scope, ReferentialScope::Collection(_)) {
            query.push("JOIN COLLECTION_SERIES cs ON cs.SERIES_ID = bma.SERIES_ID ");
        }
        query.push("WHERE bma.RELEASE_DATE IS NOT NULL");
        push_library_visibility(&mut query, "s.LIBRARY_ID", user);
        push_series_scope(&mut query, scope, "s.LIBRARY_ID", "bma.SERIES_ID");
        let dates = query.build_query_scalar::<String>().fetch_all(&self.pool).await?;
        let mut years = dates
            .into_iter()
            .filter_map(|date| date.get(..4)?.parse::<i32>().ok())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        years.sort_by(|left, right| right.cmp(left));
        Ok(years)
    }
}

fn scoped_series_metadata_query(column: &str, scope: &ReferentialScope) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new("SELECT DISTINCT ");
    query
        .push(column)
        .push(" FROM SERIES_METADATA sm JOIN SERIES s ON s.ID = sm.SERIES_ID ");
    if matches!(scope, ReferentialScope::Collection(_)) {
        query.push("JOIN COLLECTION_SERIES cs ON cs.SERIES_ID = sm.SERIES_ID ");
    }
    query.push("WHERE 1 = 1");
    query
}

fn push_series_scope(
    query: &mut QueryBuilder<'static, Sqlite>,
    scope: &ReferentialScope,
    library_column: &str,
    series_column: &str,
) {
    match scope {
        ReferentialScope::Global => {}
        ReferentialScope::Libraries(ids) => push_values(query, library_column, ids),
        ReferentialScope::Collection(id) => {
            query.push(" AND cs.COLLECTION_ID = ").push_bind(id.clone());
        }
        ReferentialScope::Series(id) => {
            query
                .push(" AND ")
                .push(series_column)
                .push(" = ")
                .push_bind(id.clone());
        }
        ReferentialScope::ReadList(_) => {
            if !series_column.starts_with("authors.") {
                query.push(" AND 0 = 1");
            }
        }
    }
    if let ReferentialScope::ReadList(id) = scope
        && series_column.starts_with("authors.")
    {
        query.push(" AND rb.READLIST_ID = ").push_bind(id.clone());
    }
}

fn push_values(query: &mut QueryBuilder<'static, Sqlite>, column: &str, values: &[String]) {
    if values.is_empty() {
        query.push(" AND 0 = 1");
        return;
    }
    query.push(" AND ").push(column).push(" IN (");
    let mut separated = query.separated(",");
    for value in values {
        separated.push_bind(value.clone());
    }
    query.push(")");
}

fn filter_authors(rows: Vec<AuthorDto>, search: Option<&str>, role: Option<&str>) -> Vec<AuthorDto> {
    let mut rows = rows
        .into_iter()
        .filter(|author| search.is_none_or(|search| contains_normalized(&author.name, search)))
        .filter(|author| role.is_none_or(|role| author.role == role))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        normalized(&left.name)
            .cmp(&normalized(&right.name))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.role.cmp(&right.role))
    });
    rows
}

fn contains_normalized(value: &str, search: &str) -> bool {
    normalized(value).contains(&normalized(search))
}

fn normalized(value: &str) -> String {
    value
        .nfkd()
        .filter(|character| !is_combining_mark(*character))
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfkc()
        .collect()
}

fn sort_strings(values: &mut [String]) {
    values.sort_by(|left, right| normalized(left).cmp(&normalized(right)).then_with(|| left.cmp(right)));
}

/// Returns the fixed sort metadata used by the Kotlin v2 author page.
pub fn author_page_request(mut page: PageRequest) -> PageRequest {
    page.sort = vec![SortOrder {
        property: "relevance".into(),
        direction: SortDirection::Ascending,
    }];
    page
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs};

    use chrono::NaiveDate;
    use komga_core::{
        AgeRestriction, AllowExclude, Author, CollectionCreation, ContentRestrictions, Library, ReadListCreation,
        scan_library_files,
    };
    use url::Url;

    use super::*;

    #[tokio::test]
    async fn referential_sources_scopes_search_and_authorization_match_kotlin() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let mut libraries = Vec::new();
        for (library_name, series_names) in [("Allowed", vec!["Primary", "Empty"]), ("Hidden", vec!["Hidden"])] {
            let root = temporary.path().join(library_name);
            for series_name in series_names {
                let directory = root.join(series_name);
                fs::create_dir_all(&directory).unwrap();
                fs::write(directory.join("Volume 1.cbz"), [1_u8]).unwrap();
            }
            let library = Library::new(library_name.into(), Url::from_directory_path(&root).unwrap());
            storage.create_library(&library).await.unwrap();
            let scan = scan_library_files(&library, || false).unwrap();
            storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
            libraries.push(library);
        }

        let mut allowed_series = storage.list_series_by_library_id(&libraries[0].id).await.unwrap();
        allowed_series.sort_by(|left, right| left.name.cmp(&right.name));
        let primary = allowed_series.iter().find(|series| series.name == "Primary").unwrap();
        let primary_book = storage.list_books_by_series_id(&primary.id).await.unwrap().remove(0);
        let hidden = storage
            .list_series_by_library_id(&libraries[1].id)
            .await
            .unwrap()
            .remove(0);
        let hidden_book = storage.list_books_by_series_id(&hidden.id).await.unwrap().remove(0);

        let mut series_metadata = storage.find_series_metadata(&primary.id).await.unwrap().unwrap();
        series_metadata.publisher = "Publisher".into();
        series_metadata.language = "en".into();
        series_metadata.age_rating = Some(18);
        series_metadata.genres = BTreeSet::from(["Fantasy".into()]);
        series_metadata.tags = BTreeSet::from(["SeriesTag".into()]);
        series_metadata.sharing_labels = BTreeSet::from(["adult".into()]);
        storage.save_series_metadata(&series_metadata).await.unwrap();

        let mut book_metadata = storage.find_book_metadata(&primary_book.id).await.unwrap().unwrap();
        book_metadata.authors = vec![Author {
            name: "José Alvarez".into(),
            role: "writer".into(),
        }];
        book_metadata.tags = BTreeSet::from(["BookTag".into()]);
        book_metadata.release_date = NaiveDate::from_ymd_opt(2024, 5, 1);
        storage.save_book_metadata(&book_metadata).await.unwrap();
        storage.rebuild_book_metadata_aggregation(&primary.id).await.unwrap();

        let mut hidden_metadata = storage.find_book_metadata(&hidden_book.id).await.unwrap().unwrap();
        hidden_metadata.authors = vec![Author {
            name: "Hidden Author".into(),
            role: "artist".into(),
        }];
        hidden_metadata.tags = BTreeSet::from(["HiddenTag".into()]);
        storage.save_book_metadata(&hidden_metadata).await.unwrap();
        storage.rebuild_book_metadata_aggregation(&hidden.id).await.unwrap();

        storage
            .create_collection(
                "collection",
                &CollectionCreation {
                    name: "Collection".into(),
                    ordered: false,
                    series_ids: vec![primary.id.clone()],
                },
            )
            .await
            .unwrap();
        storage
            .create_read_list(
                "readlist",
                &ReadListCreation {
                    name: "Read List".into(),
                    summary: String::new(),
                    ordered: true,
                    book_ids: vec![primary_book.id.clone()],
                },
            )
            .await
            .unwrap();

        let mut user = User::new("reader@example.org".into(), "hash".into());
        user.shared_all_libraries = false;
        user.shared_library_ids = BTreeSet::from([libraries[0].id.clone()]);
        user.restrictions = ContentRestrictions::new(
            Some(AgeRestriction {
                age: 10,
                restriction: AllowExclude::Exclude,
            }),
            BTreeSet::new(),
            BTreeSet::from(["adult".into()]),
        );

        let authors = storage
            .referential_v1_authors(&ReferentialScope::Global, "jose", &user)
            .await
            .unwrap();
        assert_eq!(authors[0].name, "José Alvarez");
        assert_eq!(
            storage
                .referential_v1_authors(&ReferentialScope::Series(hidden.id), "", &user)
                .await
                .unwrap(),
            []
        );
        assert_eq!(
            storage.referential_author_names("alvarez", &user).await.unwrap().len(),
            1
        );
        assert_eq!(storage.referential_author_roles(&user).await.unwrap(), ["writer"]);

        let author_page = storage
            .referential_v2_authors(
                &ReferentialScope::ReadList("readlist".into()),
                Some("JOSE"),
                Some("writer"),
                &author_page_request(PageRequest {
                    size: 1,
                    ..PageRequest::default()
                }),
                &user,
            )
            .await
            .unwrap();
        assert_eq!(author_page.total_elements, 1);
        assert_eq!(author_page.content[0].name, "José Alvarez");

        let collection = ReferentialScope::Collection("collection".into());
        assert_eq!(
            storage.referential_genres(&collection, &user).await.unwrap(),
            ["Fantasy"]
        );
        assert_eq!(
            storage.referential_sharing_labels(&collection, &user).await.unwrap(),
            ["adult"]
        );
        assert_eq!(storage.referential_languages(&collection, &user).await.unwrap(), ["en"]);
        assert_eq!(
            storage.referential_publishers(&collection, &user).await.unwrap(),
            ["Publisher"]
        );
        assert_eq!(
            storage.referential_tags(&collection, &user).await.unwrap(),
            ["BookTag", "SeriesTag"]
        );
        assert_eq!(
            storage
                .referential_book_tags(&ReferentialScope::ReadList("readlist".into()), &user)
                .await
                .unwrap(),
            ["BookTag"]
        );
        assert_eq!(
            storage
                .referential_age_ratings(&ReferentialScope::Global, &user)
                .await
                .unwrap(),
            [None, Some(18)]
        );
        assert_eq!(
            storage
                .referential_release_years(&ReferentialScope::Global, &user)
                .await
                .unwrap(),
            [2024]
        );
    }
}

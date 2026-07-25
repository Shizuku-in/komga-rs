//! Kotlin-compatible historical-event persistence.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDateTime;
use komga_core::{ContentPage, HistoricalEvent, PageRequest, SortDirection};
use sqlx::{FromRow, QueryBuilder, Sqlite, Transaction};

use crate::{Result, Storage, StorageError, query_support::validate_page};

#[derive(Debug, FromRow)]
struct HistoricalEventRow {
    id: String,
    event_type: String,
    timestamp: NaiveDateTime,
    book_id: Option<String>,
    series_id: Option<String>,
}

impl Storage {
    /// Lists historical events with their complete property maps.
    ///
    /// # Errors
    ///
    /// Returns an error when pagination, sorting, or persisted rows are invalid.
    pub async fn list_historical_events(&self, page: &PageRequest) -> Result<ContentPage<HistoricalEvent>> {
        validate_page(page)?;
        validate_sorts(page)?;
        let total = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM HISTORICAL_EVENT")
            .fetch_one(&self.pool)
            .await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT ID AS id, TYPE AS event_type, TIMESTAMP AS timestamp,
                      BOOK_ID AS book_id, SERIES_ID AS series_id
                 FROM HISTORICAL_EVENT"#,
        );
        push_order(&mut query, page);
        push_page(&mut query, page)?;
        let rows = query
            .build_query_as::<HistoricalEventRow>()
            .fetch_all(&self.pool)
            .await?;

        let mut properties = HashMap::<String, BTreeMap<String, String>>::new();
        if !rows.is_empty() {
            let mut property_query =
                QueryBuilder::<Sqlite>::new("SELECT ID, KEY, VALUE FROM HISTORICAL_EVENT_PROPERTIES WHERE ID IN (");
            let mut separated = property_query.separated(", ");
            for row in &rows {
                separated.push_bind(row.id.clone());
            }
            separated.push_unseparated(") ORDER BY ID, KEY");
            for (id, key, value) in property_query
                .build_query_as::<(String, String, String)>()
                .fetch_all(&self.pool)
                .await?
            {
                properties.entry(id).or_default().insert(key, value);
            }
        }

        let content = rows
            .into_iter()
            .map(|row| HistoricalEvent {
                properties: properties.remove(&row.id).unwrap_or_default(),
                id: row.id,
                event_type: row.event_type,
                timestamp: row.timestamp,
                book_id: row.book_id,
                series_id: row.series_id,
            })
            .collect();
        Ok(ContentPage {
            content,
            total_elements: u64::try_from(total).unwrap_or_default(),
            page: if page.unpaged { 0 } else { page.page },
            size: if page.unpaged {
                u64::try_from(total).unwrap_or(20).max(20)
            } else {
                page.size
            },
            sort: page.sort.clone(),
        })
    }

    /// Inserts one historical event and all properties atomically.
    ///
    /// # Errors
    ///
    /// Returns a database error when the event cannot be persisted.
    pub async fn insert_historical_event(&self, event: &HistoricalEvent) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        insert_historical_event(&mut tx, event).await?;
        tx.commit().await?;
        Ok(())
    }
}

pub(crate) async fn insert_historical_event(tx: &mut Transaction<'_, Sqlite>, event: &HistoricalEvent) -> Result<()> {
    sqlx::query("INSERT INTO HISTORICAL_EVENT (ID, TYPE, BOOK_ID, SERIES_ID, TIMESTAMP) VALUES (?, ?, ?, ?, ?)")
        .bind(&event.id)
        .bind(&event.event_type)
        .bind(&event.book_id)
        .bind(&event.series_id)
        .bind(event.timestamp)
        .execute(&mut **tx)
        .await?;
    for (key, value) in &event.properties {
        sqlx::query("INSERT INTO HISTORICAL_EVENT_PROPERTIES (ID, KEY, VALUE) VALUES (?, ?, ?)")
            .bind(&event.id)
            .bind(key)
            .bind(value)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

fn validate_sorts(page: &PageRequest) -> Result<()> {
    if let Some(sort) = page.sort.iter().find(|sort| sort_column(&sort.property).is_none()) {
        return Err(StorageError::InvalidQuery(format!(
            "unsupported historical-event sort property: {}",
            sort.property
        )));
    }
    Ok(())
}

fn push_order(query: &mut QueryBuilder<'_, Sqlite>, page: &PageRequest) {
    query.push(" ORDER BY ");
    for (index, sort) in page.sort.iter().enumerate() {
        if index > 0 {
            query.push(", ");
        }
        query.push(sort_column(&sort.property).expect("sorts validated"));
        query.push(match sort.direction {
            SortDirection::Ascending => " ASC",
            SortDirection::Descending => " DESC",
        });
    }
    if !page.sort.is_empty() {
        query.push(", ");
    }
    query.push("ID ASC");
}

fn push_page(query: &mut QueryBuilder<'_, Sqlite>, page: &PageRequest) -> Result<()> {
    if page.unpaged {
        return Ok(());
    }
    let size = i64::try_from(page.size).map_err(|_| StorageError::InvalidQuery("page size is too large".into()))?;
    let offset = page
        .page
        .checked_mul(page.size)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| StorageError::InvalidQuery("page offset is too large".into()))?;
    query.push(" LIMIT ").push_bind(size).push(" OFFSET ").push_bind(offset);
    Ok(())
}

fn sort_column(property: &str) -> Option<&'static str> {
    match property {
        "type" => Some("TYPE"),
        "bookId" => Some("BOOK_ID"),
        "seriesId" => Some("SERIES_ID"),
        "timestamp" => Some("TIMESTAMP"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use komga_core::{PageRequest, SortDirection, SortOrder};

    use super::*;

    fn event(id: &str, event_type: &str, day: u32) -> HistoricalEvent {
        HistoricalEvent {
            id: id.into(),
            event_type: event_type.into(),
            timestamp: NaiveDate::from_ymd_opt(2024, 1, day)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            book_id: Some(format!("book-{day}")),
            series_id: Some("series".into()),
            properties: BTreeMap::from([("reason".into(), format!("reason-{day}"))]),
        }
    }

    #[tokio::test]
    async fn historical_events_page_and_hydrate_properties() {
        let storage = Storage::connect_memory().await.unwrap();
        storage
            .insert_historical_event(&event("older", "BookFileDeleted", 1))
            .await
            .unwrap();
        storage
            .insert_historical_event(&event("newer", "DuplicatePageDeleted", 2))
            .await
            .unwrap();

        let page = storage
            .list_historical_events(&PageRequest {
                size: 1,
                sort: vec![SortOrder {
                    property: "timestamp".into(),
                    direction: SortDirection::Descending,
                }],
                ..PageRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(page.total_elements, 2);
        assert_eq!(page.content[0].id, "newer");
        assert_eq!(page.content[0].properties["reason"], "reason-2");
    }
}

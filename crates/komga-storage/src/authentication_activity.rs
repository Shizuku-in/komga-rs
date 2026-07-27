use chrono::NaiveDateTime;
use komga_core::{AuthenticationActivity, ContentPage, PageRequest, SortDirection, SortOrder, User};
use sqlx::{FromRow, QueryBuilder, Sqlite};

use crate::{
    Result, Storage, StorageError,
    query_support::{content_page, push_pagination, validate_page},
};

const SELECT_ACTIVITY: &str = r#"SELECT USER_ID AS user_id, EMAIL AS email,
    API_KEY_ID AS api_key_id, API_KEY_COMMENT AS api_key_comment, IP AS ip,
    USER_AGENT AS user_agent, SUCCESS AS success, ERROR AS error,
    DATE_TIME AS date_time, SOURCE AS source FROM AUTHENTICATION_ACTIVITY"#;

#[derive(Debug, FromRow)]
struct AuthenticationActivityRow {
    user_id: Option<String>,
    email: Option<String>,
    api_key_id: Option<String>,
    api_key_comment: Option<String>,
    ip: Option<String>,
    user_agent: Option<String>,
    success: bool,
    error: Option<String>,
    date_time: NaiveDateTime,
    source: Option<String>,
}

impl Storage {
    /// Persists one authentication attempt.
    ///
    /// Credential values must be removed by the caller before constructing the
    /// activity. Database failures are returned so HTTP authentication can log
    /// and otherwise ignore them.
    pub async fn insert_authentication_activity(&self, activity: &AuthenticationActivity) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO AUTHENTICATION_ACTIVITY
               (USER_ID, EMAIL, API_KEY_ID, API_KEY_COMMENT, IP, USER_AGENT,
                SUCCESS, ERROR, DATE_TIME, SOURCE)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&activity.user_id)
        .bind(&activity.email)
        .bind(&activity.api_key_id)
        .bind(&activity.api_key_comment)
        .bind(&activity.ip)
        .bind(&activity.user_agent)
        .bind(activity.success)
        .bind(&activity.error)
        .bind(activity.date_time)
        .bind(&activity.source)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Lists authentication activity globally or for one user.
    ///
    /// User-scoped history includes rows associated by the user's current email,
    /// matching the Kotlin implementation. Unsupported sort properties return
    /// [`StorageError::InvalidQuery`].
    pub async fn list_authentication_activity(
        &self,
        user: Option<&User>,
        page: &PageRequest,
    ) -> Result<ContentPage<AuthenticationActivity>> {
        validate_page(page)?;
        let sort = normalized_sort(page)?;
        let mut count = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM AUTHENTICATION_ACTIVITY");
        push_user_filter(&mut count, user);
        let total = count.build_query_scalar::<i64>().fetch_one(&self.pool).await?;

        let mut query = QueryBuilder::<Sqlite>::new(SELECT_ACTIVITY);
        push_user_filter(&mut query, user);
        push_sort(&mut query, &sort);
        push_pagination(&mut query, page)?;
        let rows = query
            .build_query_as::<AuthenticationActivityRow>()
            .fetch_all(&self.pool)
            .await?;
        let mut effective_page = page.clone();
        effective_page.sort = sort;
        Ok(content_page(
            rows.into_iter().map(AuthenticationActivityRow::into_domain).collect(),
            total.max(0) as u64,
            &effective_page,
        ))
    }

    /// Finds the newest activity associated with a user and optional API key.
    pub async fn latest_authentication_activity(
        &self,
        user: &User,
        api_key_id: Option<&str>,
    ) -> Result<Option<AuthenticationActivity>> {
        let mut query = QueryBuilder::<Sqlite>::new(SELECT_ACTIVITY);
        push_user_filter(&mut query, Some(user));
        if let Some(api_key_id) = api_key_id {
            query.push(" AND API_KEY_ID = ").push_bind(api_key_id.to_owned());
        }
        query.push(" ORDER BY DATE_TIME DESC LIMIT 1");
        Ok(query
            .build_query_as::<AuthenticationActivityRow>()
            .fetch_optional(&self.pool)
            .await?
            .map(AuthenticationActivityRow::into_domain))
    }

    /// Removes authentication activity older than the supplied UTC timestamp.
    pub async fn delete_authentication_activity_older_than(&self, date_time: NaiveDateTime) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM AUTHENTICATION_ACTIVITY WHERE DATE_TIME < ?")
            .bind(date_time)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }
}

fn normalized_sort(page: &PageRequest) -> Result<Vec<SortOrder>> {
    let sort = if page.sort.is_empty() {
        vec![SortOrder {
            property: "dateTime".into(),
            direction: SortDirection::Descending,
        }]
    } else {
        page.sort.clone()
    };
    for order in &sort {
        sort_column(&order.property)?;
    }
    Ok(sort)
}

fn sort_column(property: &str) -> Result<&'static str> {
    match property {
        "dateTime" => Ok("DATE_TIME"),
        "email" => Ok("EMAIL"),
        "success" => Ok("SUCCESS"),
        "ip" => Ok("IP"),
        "error" => Ok("ERROR"),
        "userId" => Ok("USER_ID"),
        "userAgent" => Ok("USER_AGENT"),
        _ => Err(StorageError::InvalidQuery(format!(
            "unsupported authentication activity sort property: {property}"
        ))),
    }
}

fn push_user_filter(query: &mut QueryBuilder<'static, Sqlite>, user: Option<&User>) {
    if let Some(user) = user {
        query
            .push(" WHERE (USER_ID = ")
            .push_bind(user.id.clone())
            .push(" OR EMAIL = ")
            .push_bind(user.email.clone())
            .push(" COLLATE KOMGA_UNICODE_NOCASE)");
    }
}

fn push_sort(query: &mut QueryBuilder<'static, Sqlite>, sort: &[SortOrder]) {
    query.push(" ORDER BY ");
    let mut separated = query.separated(", ");
    for order in sort {
        separated.push(sort_column(&order.property).expect("sort properties were validated"));
        separated.push_unseparated(match order.direction {
            SortDirection::Ascending => " ASC",
            SortDirection::Descending => " DESC",
        });
    }
}

impl AuthenticationActivityRow {
    fn into_domain(self) -> AuthenticationActivity {
        AuthenticationActivity {
            user_id: self.user_id,
            email: self.email,
            api_key_id: self.api_key_id,
            api_key_comment: self.api_key_comment,
            ip: self.ip,
            user_agent: self.user_agent,
            success: self.success,
            error: self.error,
            date_time: self.date_time,
            source: self.source,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use komga_core::{AuthenticationActivity, PageRequest, SortDirection, SortOrder, User, new_id};

    use crate::Storage;

    fn activity(user: &User, date: (i32, u32, u32), success: bool, api_key_id: Option<&str>) -> AuthenticationActivity {
        AuthenticationActivity {
            user_id: Some(user.id.clone()),
            email: Some(user.email.clone()),
            api_key_id: api_key_id.map(str::to_owned),
            api_key_comment: api_key_id.map(|_| "reader".to_owned()),
            ip: Some("192.0.2.10".into()),
            user_agent: Some("test".into()),
            success,
            error: (!success).then(|| "Authentication failed".into()),
            date_time: NaiveDate::from_ymd_opt(date.0, date.1, date.2)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            source: Some(if api_key_id.is_some() { "ApiKey" } else { "Password" }.into()),
        }
    }

    #[tokio::test]
    async fn activity_supports_paging_sorting_latest_filter_and_cleanup() {
        let storage = Storage::connect_memory().await.unwrap();
        let user = User::new("Reader@Example.org".into(), "hash".into());
        storage.create_user(&user).await.unwrap();
        let key_id = new_id();
        for item in [
            activity(&user, (2025, 1, 1), false, None),
            activity(&user, (2025, 2, 1), true, Some(&key_id)),
            activity(&user, (2025, 3, 1), true, None),
        ] {
            storage.insert_authentication_activity(&item).await.unwrap();
        }

        let page = storage
            .list_authentication_activity(
                Some(&User {
                    email: "reader@example.ORG".into(),
                    ..user.clone()
                }),
                &PageRequest {
                    page: 0,
                    size: 2,
                    unpaged: false,
                    sort: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(page.total_elements, 3);
        assert_eq!(page.content.len(), 2);
        assert_eq!(
            page.content[0].date_time.date(),
            NaiveDate::from_ymd_opt(2025, 3, 1).unwrap()
        );
        assert_eq!(page.sort[0].property, "dateTime");
        assert_eq!(page.sort[0].direction, SortDirection::Descending);

        let latest_key = storage
            .latest_authentication_activity(&user, Some(&key_id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(latest_key.api_key_id.as_deref(), Some(key_id.as_str()));
        assert_eq!(
            latest_key.date_time.date(),
            NaiveDate::from_ymd_opt(2025, 2, 1).unwrap()
        );

        let ascending = storage
            .list_authentication_activity(
                None,
                &PageRequest {
                    sort: vec![SortOrder {
                        property: "success".into(),
                        direction: SortDirection::Ascending,
                    }],
                    unpaged: true,
                    ..PageRequest::default()
                },
            )
            .await
            .unwrap();
        assert!(!ascending.content[0].success);
        assert!(
            storage
                .list_authentication_activity(
                    None,
                    &PageRequest {
                        sort: vec![SortOrder {
                            property: "source".into(),
                            direction: SortDirection::Ascending,
                        }],
                        ..PageRequest::default()
                    }
                )
                .await
                .is_err()
        );

        let deleted = storage
            .delete_authentication_activity_older_than(
                NaiveDate::from_ymd_opt(2025, 2, 15)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(
            storage
                .list_authentication_activity(None, &PageRequest::default())
                .await
                .unwrap()
                .total_elements,
            1
        );
    }

    #[tokio::test]
    async fn deleting_a_user_removes_activity_linked_by_id_or_email() {
        let storage = Storage::connect_memory().await.unwrap();
        let user = User::new("reader@example.org".into(), "hash".into());
        storage.create_user(&user).await.unwrap();
        let mut by_id = activity(&user, (2025, 1, 1), true, None);
        by_id.email = None;
        let mut by_email = activity(&user, (2025, 1, 2), false, None);
        by_email.user_id = None;
        storage.insert_authentication_activity(&by_id).await.unwrap();
        storage.insert_authentication_activity(&by_email).await.unwrap();

        assert!(storage.delete_user(&user.id).await.unwrap());
        assert_eq!(
            storage
                .list_authentication_activity(None, &PageRequest::default())
                .await
                .unwrap()
                .total_elements,
            0
        );
    }
}

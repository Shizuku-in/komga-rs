use std::collections::BTreeMap;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::NaiveDate;
use komga_core::HistoricalEvent;
use komga_storage::Storage;
use tower::ServiceExt;

use crate::support::{app_with_storage, basic, body_json, claim};

#[tokio::test]
async fn history_is_admin_only_and_uses_kotlin_page_contract() {
    let storage = Storage::connect_memory().await.unwrap();
    for (id, day, event_type) in [("older", 1, "BookFileDeleted"), ("newer", 2, "DuplicatePageDeleted")] {
        storage
            .insert_historical_event(&HistoricalEvent {
                id: id.into(),
                event_type: event_type.into(),
                timestamp: NaiveDate::from_ymd_opt(2024, 2, day)
                    .unwrap()
                    .and_hms_opt(3, 4, 5)
                    .unwrap(),
                book_id: Some(format!("book-{day}")),
                series_id: Some("series".into()),
                properties: BTreeMap::from([("key".into(), format!("value-{day}"))]),
            })
            .await
            .unwrap();
    }
    let app = app_with_storage(storage).await;
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v1/history").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v2/users")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"email":"reader@example.org","password":"reader-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let forbidden = app
        .clone()
        .oneshot(
            Request::get("/api/v1/history")
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(
            Request::get("/api/v1/history?page=bad&size=9999&unknown=x")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page = body_json(response).await;
    assert_eq!(page["size"], 2_000);
    assert_eq!(page["number"], 0);
    assert_eq!(page["sort"]["sorted"], true);
    assert_eq!(page["content"][0]["id"], "newer");
    assert_eq!(page["content"][0]["type"], "DuplicatePageDeleted");
    assert_eq!(page["content"][0]["timestamp"], "2024-02-02T03:04:05");
    assert_eq!(page["content"][0]["bookId"], "book-2");
    assert_eq!(page["content"][0]["seriesId"], "series");
    assert_eq!(page["content"][0]["properties"]["key"], "value-2");

    let unknown_sort = app
        .clone()
        .oneshot(
            Request::get("/api/v1/history?sort=unknown,asc")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(unknown_sort).await["sort"]["sorted"], false);
}

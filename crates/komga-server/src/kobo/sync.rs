use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, HeaderValue, Method, Uri},
    response::{IntoResponse, Response},
};
use komga_core::SyncBookChange;
use komga_core::SyncChangeKind;

use crate::{AppState, error::ApiError};

use super::{
    auth::KoboAuthContext,
    dto::{
        BookEntitlementContainerDto, SyncResultDto, WrappedReadingStateDto, entitlement, metadata, reading_state,
        removed_metadata, should_convert_kepub, wrapped_tag,
    },
    resources::download_url,
    token,
};

const X_KOBO_SYNC_TOKEN: &str = "x-kobo-synctoken";
const X_KOBO_SYNC: &str = "x-kobo-sync";

pub(super) async fn library_sync(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let received = token::decode(headers.get(X_KOBO_SYNC_TOKEN).and_then(|value| value.to_str().ok()));
    let ongoing = verified_point(&state, received.ongoing_sync_point_id.as_deref(), &auth.user.id).await?;
    let to_point = match ongoing {
        Some(point) => point,
        None => {
            state
                .storage
                .create_sync_point(&auth.user, Some(&auth.api_key_id))
                .await?
        }
    };
    let from_point = verified_point(&state, received.last_successful_sync_point_id.as_deref(), &auth.user.id).await?;
    let batch = state
        .storage
        .take_sync_batch(
            from_point.as_ref().map(|point| point.id.as_str()),
            &to_point.id,
            &auth.user.id,
            state.kobo_sync_item_limit,
        )
        .await?;
    let conversion_available = state.kobo_kepub.is_available();
    let mut local_results = if from_point.is_some() {
        incremental_results(&headers, &uri, &auth, &batch.books, conversion_available)?
    } else {
        initial_results(&headers, &uri, &auth, &batch.books, conversion_available)?
    };
    append_tag_results(&mut local_results, &batch.read_lists);
    let mut results = local_results
        .into_iter()
        .map(|result| {
            serde_json::to_value(result).map_err(|error| {
                tracing::error!(%error, "could not serialize local Kobo sync result");
                ApiError::internal("Could not serialize Kobo sync result")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut merged_token = received.clone();
    let mut should_continue = batch.has_more;
    if !batch.has_more && state.kobo_proxy.is_enabled() {
        match state
            .kobo_proxy
            .forward(&Method::GET, &uri, &headers, None, Some(&received.raw_kobo_sync_token))
            .await
        {
            Ok(response) => match response.json_value().ok().and_then(|value| value.as_array().cloned()) {
                Some(mut official_results) => {
                    results.append(&mut official_results);
                    if let Some(raw_token) = response.header(X_KOBO_SYNC_TOKEN).and_then(|value| value.to_str().ok()) {
                        merged_token.raw_kobo_sync_token = raw_token.to_owned();
                    }
                    should_continue = response
                        .header(X_KOBO_SYNC)
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|value| value.eq_ignore_ascii_case("continue"));
                }
                None => tracing::error!("Kobo Store sync returned an invalid result array; completing local sync"),
            },
            Err(error) => tracing::error!(%error, "Kobo Store sync failed; completing local sync"),
        }
    }

    let updated = if should_continue {
        merged_token.ongoing_sync_point_id = Some(to_point.id.clone());
        merged_token
    } else {
        if let Some(point) = from_point.as_ref()
            && point.id != to_point.id
        {
            state.storage.delete_sync_point(&point.id).await?;
        }
        merged_token.ongoing_sync_point_id = None;
        merged_token.last_successful_sync_point_id = Some(to_point.id);
        merged_token
    };
    let mut response = Json(results).into_response();
    response.headers_mut().insert(
        X_KOBO_SYNC_TOKEN,
        HeaderValue::from_str(&token::encode(&updated))
            .map_err(|_| ApiError::internal("Could not encode Kobo sync token"))?,
    );
    if should_continue {
        response
            .headers_mut()
            .insert(X_KOBO_SYNC, HeaderValue::from_static("continue"));
    }
    Ok(response)
}

async fn verified_point(
    state: &AppState,
    id: Option<&str>,
    user_id: &str,
) -> Result<Option<komga_core::SyncPoint>, ApiError> {
    match id {
        Some(id) => Ok(state.storage.find_sync_point_for_user(id, user_id).await?),
        None => Ok(None),
    }
}

fn initial_results(
    headers: &HeaderMap,
    uri: &Uri,
    auth: &KoboAuthContext,
    books: &[SyncBookChange],
    conversion_available: bool,
) -> Result<Vec<SyncResultDto>, ApiError> {
    books
        .iter()
        .map(|change| {
            let projection = projection(change)?;
            Ok(SyncResultDto::NewEntitlement {
                value: BookEntitlementContainerDto {
                    book_entitlement: entitlement(change, false),
                    book_metadata: metadata(
                        projection,
                        change.snapshot.thumbnail_id.clone(),
                        change.snapshot.file_size,
                        download_url(
                            headers,
                            uri,
                            &auth.auth_token,
                            &change.snapshot.book_id,
                            should_convert_kepub(projection, conversion_available),
                        )?,
                        conversion_available,
                    ),
                    reading_state: Some(reading_state(
                        change.read_progress.as_ref(),
                        &change.snapshot.book_id,
                        change.snapshot.created_date,
                    )),
                },
            })
        })
        .collect()
}

fn incremental_results(
    headers: &HeaderMap,
    uri: &Uri,
    auth: &KoboAuthContext,
    books: &[SyncBookChange],
    conversion_available: bool,
) -> Result<Vec<SyncResultDto>, ApiError> {
    let mut output = Vec::new();
    for change in books.iter().filter(|book| book.kind == SyncChangeKind::Added) {
        let projection = projection(change)?;
        output.push(SyncResultDto::NewEntitlement {
            value: BookEntitlementContainerDto {
                book_entitlement: entitlement(change, false),
                book_metadata: metadata(
                    projection,
                    change.snapshot.thumbnail_id.clone(),
                    change.snapshot.file_size,
                    download_url(
                        headers,
                        uri,
                        &auth.auth_token,
                        &change.snapshot.book_id,
                        should_convert_kepub(projection, conversion_available),
                    )?,
                    conversion_available,
                ),
                reading_state: Some(reading_state(
                    change.read_progress.as_ref(),
                    &change.snapshot.book_id,
                    change.snapshot.created_date,
                )),
            },
        });
    }
    for change in books.iter().filter(|book| book.kind == SyncChangeKind::Changed) {
        let projection = projection(change)?;
        output.push(SyncResultDto::NewEntitlement {
            value: BookEntitlementContainerDto {
                book_entitlement: entitlement(change, false),
                book_metadata: metadata(
                    projection,
                    change.snapshot.thumbnail_id.clone(),
                    change.snapshot.file_size,
                    download_url(
                        headers,
                        uri,
                        &auth.auth_token,
                        &change.snapshot.book_id,
                        should_convert_kepub(projection, conversion_available),
                    )?,
                    conversion_available,
                ),
                reading_state: Some(reading_state(
                    change.read_progress.as_ref(),
                    &change.snapshot.book_id,
                    change.snapshot.created_date,
                )),
            },
        });
    }
    for change in books.iter().filter(|book| book.kind == SyncChangeKind::Changed) {
        output.push(SyncResultDto::ChangedProductMetadata {
            value: metadata(
                projection(change)?,
                change.snapshot.thumbnail_id.clone(),
                change.snapshot.file_size,
                download_url(
                    headers,
                    uri,
                    &auth.auth_token,
                    &change.snapshot.book_id,
                    should_convert_kepub(projection(change)?, conversion_available),
                )?,
                conversion_available,
            ),
        });
    }
    for change in books.iter().filter(|book| book.kind == SyncChangeKind::Removed) {
        output.push(SyncResultDto::ChangedEntitlement {
            value: BookEntitlementContainerDto {
                book_entitlement: entitlement(change, true),
                book_metadata: removed_metadata(&change.snapshot.book_id),
                reading_state: None,
            },
        });
    }
    for change in books.iter().filter(|book| {
        matches!(book.kind, SyncChangeKind::Changed | SyncChangeKind::ProgressChanged) && book.read_progress.is_some()
    }) {
        output.push(SyncResultDto::ChangedReadingState {
            value: WrappedReadingStateDto {
                reading_state: reading_state(
                    change.read_progress.as_ref(),
                    &change.snapshot.book_id,
                    change.snapshot.created_date,
                ),
            },
        });
    }
    Ok(output)
}

fn append_tag_results(output: &mut Vec<SyncResultDto>, tags: &[komga_core::SyncReadListChange]) {
    for change in tags {
        let value = wrapped_tag(
            &change.snapshot,
            (change.kind != SyncChangeKind::Removed).then_some(change.book_ids.as_slice()),
        );
        output.push(match change.kind {
            SyncChangeKind::Added => SyncResultDto::NewTag { value },
            SyncChangeKind::Changed => SyncResultDto::ChangedTag { value },
            SyncChangeKind::Removed => SyncResultDto::DeletedTag { value },
            SyncChangeKind::ProgressChanged => unreachable!("read lists do not have progress changes"),
        });
    }
}

fn projection(change: &SyncBookChange) -> Result<&komga_core::SyncBookProjection, ApiError> {
    change
        .projection
        .as_ref()
        .ok_or_else(|| ApiError::internal("Kobo book projection is missing"))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Json, Router,
        body::Body,
        extract::State as AxumState,
        http::{Request, StatusCode},
        routing::get,
    };
    use http_body_util::BodyExt;
    use komga_core::{User, UserRole};
    use komga_storage::{Storage, TaskStorage};
    use serde_json::json;
    use sha2::{Digest, Sha512};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    use super::*;
    use crate::{build_app, kobo::proxy::KoboProxy};

    async fn official_sync(AxumState(calls): AxumState<Arc<AtomicUsize>>, headers: HeaderMap) -> Response {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        let expected = if call == 0 { "official.old" } else { "official.next" };
        if headers.get(X_KOBO_SYNC_TOKEN).and_then(|value| value.to_str().ok()) != Some(expected) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let mut response = Json(json!([{"StoreResult": call + 1}])).into_response();
        response.headers_mut().insert(
            X_KOBO_SYNC_TOKEN,
            HeaderValue::from_static(if call == 0 { "official.next" } else { "official.done" }),
        );
        if call == 0 {
            response
                .headers_mut()
                .insert(X_KOBO_SYNC, HeaderValue::from_static("continue"));
        }
        response
    }

    #[tokio::test]
    async fn official_sync_pages_are_merged_before_local_snapshot_finalization() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let upstream_calls = calls.clone();
        let upstream = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/library/sync", get(official_sync))
                    .with_state(upstream_calls),
            )
            .await
            .unwrap();
        });
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let mut user = User::new("sync-proxy@example.org".into(), "unused".into());
        user.roles.insert(UserRole::KoboSync);
        storage.create_user(&user).await.unwrap();
        let key = "sync-proxy-key";
        storage
            .create_api_key(
                "sync-proxy-key-id",
                &user.id,
                &hex::encode(Sha512::digest(key.as_bytes())),
                "Kobo",
            )
            .await
            .unwrap();
        let mut state = AppState::new(storage.clone(), tasks).await.unwrap();
        state.kobo_proxy = KoboProxy::with_base_url(true, &base_url);
        let app = build_app(state);

        let first = app
            .clone()
            .oneshot(
                Request::get(format!("/kobo/{key}/v1/library/sync"))
                    .header(X_KOBO_SYNC_TOKEN, "official.old")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(first.headers()[X_KOBO_SYNC], "continue");
        let first_token = first.headers()[X_KOBO_SYNC_TOKEN].to_str().unwrap().to_owned();
        let decoded = token::decode(Some(&first_token));
        assert_eq!(decoded.raw_kobo_sync_token, "official.next");
        assert!(decoded.ongoing_sync_point_id.is_some());
        assert!(decoded.last_successful_sync_point_id.is_none());
        let first_body = first.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&first_body).unwrap()[0]["StoreResult"],
            1
        );

        let second = app
            .clone()
            .oneshot(
                Request::get(format!("/kobo/{key}/v1/library/sync"))
                    .header(X_KOBO_SYNC_TOKEN, first_token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        assert!(!second.headers().contains_key(X_KOBO_SYNC));
        let decoded = token::decode(second.headers()[X_KOBO_SYNC_TOKEN].to_str().ok());
        assert_eq!(decoded.raw_kobo_sync_token, "official.done");
        assert!(decoded.ongoing_sync_point_id.is_none());
        let successful = decoded.last_successful_sync_point_id.unwrap();
        assert!(
            storage
                .find_sync_point_for_user(&successful, &user.id)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let local_only = app
            .oneshot(
                Request::get(format!("/kobo/{key}/v1/library/sync"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(local_only.status(), StatusCode::OK);
        assert!(!local_only.headers().contains_key(X_KOBO_SYNC));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        upstream.abort();
    }
}

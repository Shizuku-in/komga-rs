use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, HeaderValue, Uri},
    response::{IntoResponse, Response},
};
use komga_core::{KomgaSyncToken, SyncBookChange, SyncChangeKind};

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
    let mut results = if from_point.is_some() {
        incremental_results(&headers, &uri, &auth, &batch.books, conversion_available)?
    } else {
        initial_results(&headers, &uri, &auth, &batch.books, conversion_available)?
    };
    append_tag_results(&mut results, &batch.read_lists);

    let updated = if batch.has_more {
        KomgaSyncToken {
            ongoing_sync_point_id: Some(to_point.id.clone()),
            ..received
        }
    } else {
        if let Some(point) = from_point.as_ref()
            && point.id != to_point.id
        {
            state.storage.delete_sync_point(&point.id).await?;
        }
        KomgaSyncToken {
            ongoing_sync_point_id: None,
            last_successful_sync_point_id: Some(to_point.id),
            ..received
        }
    };
    let mut response = Json(results).into_response();
    response.headers_mut().insert(
        X_KOBO_SYNC_TOKEN,
        HeaderValue::from_str(&token::encode(&updated))
            .map_err(|_| ApiError::internal("Could not encode Kobo sync token"))?,
    );
    if batch.has_more {
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

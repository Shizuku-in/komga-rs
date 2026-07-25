use std::path::Path as FilePath;

use komga_core::{BookDto, SeriesDto, User};

use crate::{AppState, error::ApiError};

/// Loads a series and applies library, age, and sharing-label restrictions.
pub(crate) async fn load_visible_series(state: &AppState, user: &User, series_id: &str) -> Result<SeriesDto, ApiError> {
    let series = state
        .storage
        .find_series_dto_by_id(series_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    ensure_series_visible(user, &series)?;
    Ok(series)
}

/// Loads a book and rejects users who cannot access its containing series.
pub(crate) async fn load_visible_book(state: &AppState, user: &User, book_id: &str) -> Result<BookDto, ApiError> {
    load_visible_book_context(state, user, book_id)
        .await
        .map(|(book, _)| book)
}

/// Loads a book with its series and applies library, age, and label restrictions.
///
/// Missing rows remain `404`, while an existing but restricted series returns
/// `403`; callers should use this helper before exposing media-derived details.
pub(crate) async fn load_visible_book_context(
    state: &AppState,
    user: &User,
    book_id: &str,
) -> Result<(BookDto, SeriesDto), ApiError> {
    let book = state
        .storage
        .find_book_dto_by_id(book_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let series = state
        .storage
        .find_series_dto_by_id(&book.series_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    ensure_series_visible(user, &series)?;
    Ok((book, series))
}

fn ensure_series_visible(user: &User, series: &SeriesDto) -> Result<(), ApiError> {
    let age_rating = series.metadata.age_rating.and_then(|value| u32::try_from(value).ok());
    if !user.can_access_library(&series.library_id)
        || !user.is_content_allowed(age_rating, &series.metadata.sharing_labels)
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

/// Redacts the persisted book path before exposing it to a non-admin user.
pub(crate) fn redact_book_source_for_user(book: &mut BookDto, user: &User) {
    if user.is_admin() {
        return;
    }
    book.url = FilePath::new(&book.url)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
}

/// Redacts the persisted series path before exposing it to a non-admin user.
pub(crate) fn redact_series_source_for_user(series: &mut SeriesDto, user: &User) {
    if !user.is_admin() {
        series.url.clear();
    }
}

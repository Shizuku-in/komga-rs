use komga_search::EntityKind;

use crate::AppState;

pub(crate) fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(crate) async fn search_hits(state: &AppState, kind: EntityKind, query: &str) -> Vec<String> {
    let index = state.search.clone();
    let query = query.to_owned();
    tokio::task::spawn_blocking(move || index.search(kind, &query).into_iter().map(|hit| hit.id).collect())
        .await
        .unwrap_or_default()
}

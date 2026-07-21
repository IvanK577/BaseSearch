use super::state::AppTab;
use super::stored_queries::{StoredQuery, encode_stored_queries_v2};
use super::ui_text::query_summary;
use super::{App, RECENT_QUERIES_V2_META, RECENT_QUERY_LIMIT, SAVED_QUERIES_V2_META};
use crate::db::Query;

pub(super) fn persist_recent_queries(app: &App) {
    app.persist(
        RECENT_QUERIES_V2_META,
        &encode_stored_queries_v2(&app.recent_queries),
    );
}

pub(super) fn persist_saved_queries(app: &App) {
    app.persist(
        SAVED_QUERIES_V2_META,
        &encode_stored_queries_v2(&app.saved_queries),
    );
}

pub(super) fn remember_recent_query(app: &mut App, query: &Query) {
    if query.is_empty() {
        return;
    }
    app.recent_queries.retain(|item| item.query != *query);
    app.recent_queries.insert(
        0,
        StoredQuery {
            name: query_summary(query, app.t()),
            query: query.clone(),
        },
    );
    app.recent_queries.truncate(RECENT_QUERY_LIMIT);
    persist_recent_queries(app);
}

pub(super) fn save_current_query(app: &mut App) {
    let query = Query {
        text: app.query_text.clone(),
        filters: app.filters.clone(),
        advanced: app.advanced_query.clone(),
        record_scope: Default::default(),
    };
    if query.is_empty() {
        return;
    }
    app.saved_queries.retain(|item| item.query != query);
    app.saved_queries.insert(
        0,
        StoredQuery {
            name: query_summary(&query, app.t()),
            query,
        },
    );
    persist_saved_queries(app);
}

pub(super) fn clear_recent_queries(app: &mut App) {
    app.recent_queries.clear();
    persist_recent_queries(app);
}

pub(super) fn remove_saved_query(app: &mut App, index: usize) {
    if index < app.saved_queries.len() {
        app.saved_queries.remove(index);
        persist_saved_queries(app);
    }
}

pub(super) fn apply_stored_query(app: &mut App, query: Query) {
    app.query_text = query.text;
    app.filters = query.filters;
    app.advanced_query = query.advanced;
    app.show_filters = !app.filters.is_empty();
    app.active_tab = AppTab::Results;
    app.start_search(true);
}

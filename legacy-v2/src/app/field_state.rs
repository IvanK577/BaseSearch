use std::collections::HashSet;

use super::App;
use crate::search::{FieldInfo, default_field_catalog, result_field_catalog};

pub(super) fn persist_hidden_cols(app: &App) {
    let hidden: Vec<&str> = app
        .result_fields
        .iter()
        .zip(&app.visible_cols)
        .filter(|(_, v)| !**v)
        .map(|(field, _)| field.id.as_str())
        .collect();
    app.persist("hidden_cols", &hidden.join(","));
}

pub(super) fn set_result_fields(app: &mut App, fields: Vec<FieldInfo>) {
    let hidden = hidden_result_ids(app);
    app.visible_cols = fields
        .iter()
        .map(|field| !hidden.contains(&field.id))
        .collect();
    app.result_fields = fields;
}

pub(super) fn refresh_result_fields(app: &mut App) {
    let fields = app
        .lite_db
        .as_ref()
        .map(|db| db.result_fields_cached())
        .unwrap_or_else(|| result_field_catalog(Vec::<String>::new()));
    set_result_fields(app, fields);
}

pub(super) fn refresh_search_fields(app: &mut App) {
    app.search_fields = app
        .lite_db
        .as_ref()
        .map(|db| db.field_catalog_cached())
        .unwrap_or_else(default_field_catalog);
}

fn hidden_result_ids(app: &App) -> HashSet<String> {
    app.result_fields
        .iter()
        .zip(&app.visible_cols)
        .filter(|(_, visible)| !**visible)
        .map(|(field, _)| field.id.clone())
        .collect()
}

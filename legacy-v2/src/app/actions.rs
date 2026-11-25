use super::App;
use super::results_table::RowMenuAction;
use super::state::{AnalyticsView, AppTab, StatusLine};
use super::ui_text::{GuidedQuestionAction, guided_questions_empty};
use crate::db::{AnalyticsFilterAction, AnalyticsFilterField, Filters, Query};
use crate::workers::WorkerReq;

pub(super) fn open_card(app: &mut App, row_index: usize) {
    let Some(id) = app.row_ids.get(row_index).copied() else {
        return;
    };
    open_card_by_id(app, id);
}

pub(super) fn open_card_by_id(app: &mut App, id: i64) {
    if let Some(db) = &app.lite_db
        && let Ok(card) = db.record_card(id)
    {
        app.card = Some(card);
        app.card_open = true;
    }
}

pub(super) fn open_profile(app: &mut App, edrpou: String) {
    let edrpou = edrpou.trim().to_string();
    if edrpou.is_empty() {
        return;
    }
    app.profile = None;
    app.profile_loading = true;
    app.profile_gen += 1;
    let _ = app.analytics_tx.send(WorkerReq::Profile {
        edrpou,
        generation: app.profile_gen,
    });
}

pub(super) fn close_profile(app: &mut App) {
    app.profile = None;
    app.profile_loading = false;
    app.profile_gen += 1;
}

pub(super) fn run_guided_question(app: &mut App, action: GuidedQuestionAction) {
    let current = Query {
        text: app.query_text.clone(),
        filters: app.filters.clone(),
        advanced: app.advanced_query.clone(),
        record_scope: Default::default(),
    };
    if current.is_empty() && !matches!(action, GuidedQuestionAction::Profile(_)) {
        app.status = StatusLine {
            text: guided_questions_empty(app.lang).to_string(),
            is_error: false,
        };
        return;
    }
    let query_changed = current != app.active_query;
    match action {
        GuidedQuestionAction::Analytics(view) => {
            app.active_tab = AppTab::Analytics;
            app.analytics_view = view;
            if query_changed {
                app.start_search(true);
            } else {
                app.request_analytics();
            }
        }
        GuidedQuestionAction::Explore(kind) => {
            app.active_tab = AppTab::Analytics;
            if query_changed {
                app.start_search(true);
            }
            app.open_group_explorer(kind);
        }
        GuidedQuestionAction::Pivot(row_dim, col_dim, metric) => {
            app.active_tab = AppTab::Analytics;
            app.analytics_view = AnalyticsView::Pivot;
            app.pivot_row_dim = row_dim;
            app.pivot_col_dim = col_dim;
            app.pivot_metric = metric;
            app.pivot = None;
            app.analytics_loaded[AnalyticsView::Pivot.index()] = false;
            if query_changed {
                app.start_search(true);
            } else {
                app.request_analytics();
            }
        }
        GuidedQuestionAction::Profile(edrpou) => open_profile(app, edrpou),
    }
}

pub(super) fn handle_row_click(app: &mut App, i: usize, modifiers: egui::Modifiers) {
    if modifiers.ctrl || modifiers.command {
        if !app.selected.insert(i) {
            app.selected.remove(&i);
        }
        app.select_anchor = Some(i);
    } else if modifiers.shift && app.select_anchor.is_some() {
        let anchor = app.select_anchor.unwrap();
        let (lo, hi) = (anchor.min(i), anchor.max(i));
        app.selected = (lo..=hi).collect();
    } else {
        app.selected.clear();
        app.selected.insert(i);
        app.select_anchor = Some(i);
    }
}

pub(super) fn copy_selected_rows(app: &App, ctx: &egui::Context) {
    let mut indices: Vec<usize> = app.selected.iter().copied().collect();
    indices.sort_unstable();
    let lines: Vec<String> = indices
        .iter()
        .filter_map(|i| app.rows.get(*i))
        .map(|row| {
            row.iter()
                .zip(&app.visible_cols)
                .filter(|(_, v)| **v)
                .map(|(value, _)| value.as_str())
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect();
    if !lines.is_empty() {
        ctx.copy_text(lines.join("\n"));
    }
}

pub(super) fn apply_menu_action(app: &mut App, ctx: &egui::Context, action: RowMenuAction) {
    let quick_filter = |app: &mut App, set: &dyn Fn(&mut Filters, String), value: String| {
        app.query_text.clear();
        app.filters.clear();
        set(&mut app.filters, value);
        app.show_filters = true;
        app.start_search(true);
    };
    match action {
        RowMenuAction::CopyCell(value) => ctx.copy_text(value),
        RowMenuAction::CopyRow(i) => {
            if let Some(row) = app.rows.get(i) {
                ctx.copy_text(row.join("\t"));
            }
        }
        RowMenuAction::CopySelected => copy_selected_rows(app, ctx),
        RowMenuAction::FilterSender(v) => {
            quick_filter(app, &|f, v| f.sender = v, v);
        }
        RowMenuAction::FilterRecipient(v) => {
            quick_filter(app, &|f, v| f.recipient = v, v);
        }
        RowMenuAction::FilterCode(v) => {
            quick_filter(app, &|f, v| f.product_code = v, v);
        }
        RowMenuAction::FilterEdrpou(v) => {
            quick_filter(app, &|f, v| f.edrpou = v, v);
        }
        RowMenuAction::OpenProfile(v) => open_profile(app, v),
    }
}

pub(super) fn apply_analytics_filter(app: &mut App, action: AnalyticsFilterAction) {
    match action.field {
        AnalyticsFilterField::Recipient => app.filters.recipient = action.value,
        AnalyticsFilterField::Sender => app.filters.sender = action.value,
        AnalyticsFilterField::Edrpou => app.filters.edrpou = action.value,
        AnalyticsFilterField::ProductCode => app.filters.product_code = action.value,
        AnalyticsFilterField::OriginCountry => app.filters.origin_country = action.value,
        AnalyticsFilterField::DispatchCountry => app.filters.dispatch_country = action.value,
        AnalyticsFilterField::TradeCountry => app.filters.trade_country = action.value,
        AnalyticsFilterField::Trademark => app.filters.trademark = action.value,
        AnalyticsFilterField::Description => app.filters.description = action.value,
    }
    app.show_filters = true;
    app.active_tab = AppTab::Results;
    app.start_search(true);
}

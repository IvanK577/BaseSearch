use super::App;
use super::analytics_groups::{GroupExplorerState, GroupSort};
use super::state::{AnalyticsView, AppTab, invalidate_underpricing_generation};
use crate::db::{AnalyticsScope, AnalyticsSectionKind, Query};
use crate::workers::WorkerReq;

pub(super) fn start_search(app: &mut App, reset_page: bool) {
    if reset_page {
        app.page = 0;
    }
    app.active_query = Query {
        text: app.query_text.clone(),
        filters: app.filters.clone(),
        advanced: app.advanced_query.clone(),
        record_scope: Default::default(),
    };
    let query_to_remember = app.active_query.clone();
    if reset_page {
        app.remember_recent_query(&query_to_remember);
    }
    app.search_gen += 1;
    app.search_in_flight = true;
    app.count_in_flight = true;
    app.page_has_next = false;
    app.total = None;
    app.last_search_ms = None;
    app.analytics = None;
    app.analytics_loaded = [false; AnalyticsView::COUNT];
    app.analytics_loading = false;
    app.group_explorer = None;
    app.pivot = None;
    app.compare_analytics = None;
    app.compare_query = None;
    app.compare_loading = false;
    app.underpricing = None;
    app.underpricing_loading = false;
    invalidate_underpricing_generation(&mut app.underpricing_gen);
    let _ = app.search_tx.send(WorkerReq::Search {
        q: Box::new(app.active_query.clone()),
        page: app.page,
        generation: app.search_gen,
    });
    if app.active_tab == AppTab::Analytics {
        request_analytics(app);
    }
}

pub(super) fn goto_page(app: &mut App, page: u64) {
    app.page = page;
    app.search_gen += 1;
    app.search_in_flight = true;
    app.count_in_flight = true;
    app.page_has_next = false;
    app.total = None;
    app.last_search_ms = None;
    let _ = app.search_tx.send(WorkerReq::Search {
        q: Box::new(app.active_query.clone()),
        page,
        generation: app.search_gen,
    });
}

pub(super) fn request_analytics(app: &mut App) {
    if app.active_query.is_empty() {
        return;
    }
    if app.analytics_view == AnalyticsView::Report {
        request_report_data(app);
        return;
    }
    if app.analytics_view == AnalyticsView::Compare {
        if app.analytics.is_none() || app.analytics_gen != app.search_gen {
            app.analytics_loading = true;
            let _ = app.analytics_tx.send(WorkerReq::Analytics {
                q: Box::new(app.active_query.clone()),
                limit: app.analytics_limit,
                scope: None,
                hs_level: app.hs_level,
                generation: app.search_gen,
            });
        }
        return;
    }
    if app.analytics_view == AnalyticsView::Pivot {
        if app.analytics.is_none() || app.analytics_gen != app.search_gen {
            app.analytics_loading = true;
            let _ = app.analytics_tx.send(WorkerReq::Analytics {
                q: Box::new(app.active_query.clone()),
                limit: app.analytics_limit,
                scope: None,
                hs_level: app.hs_level,
                generation: app.search_gen,
            });
        }
        request_pivot(app);
        return;
    }
    if app.analytics_gen == app.search_gen && app.analytics_loaded[app.analytics_view.index()] {
        return;
    }
    app.analytics_loading = true;
    let _ = app.analytics_tx.send(WorkerReq::Analytics {
        q: Box::new(app.active_query.clone()),
        limit: app.analytics_limit,
        scope: app.analytics_view.scope(),
        hs_level: app.hs_level,
        generation: app.search_gen,
    });
}

pub(super) fn request_report_data(app: &mut App) {
    let base_needed = app.analytics.is_none() || app.analytics_gen != app.search_gen;
    if base_needed {
        app.analytics_loading = true;
        let _ = app.analytics_tx.send(WorkerReq::Analytics {
            q: Box::new(app.active_query.clone()),
            limit: app.analytics_limit,
            scope: None,
            hs_level: app.hs_level,
            generation: app.search_gen,
        });
    }
    for scope in AnalyticsScope::ALL {
        let view = AnalyticsView::from_scope(Some(scope));
        if app.analytics_gen == app.search_gen && app.analytics_loaded[view.index()] {
            continue;
        }
        app.analytics_loading = true;
        let _ = app.analytics_tx.send(WorkerReq::Analytics {
            q: Box::new(app.active_query.clone()),
            limit: app.analytics_limit,
            scope: Some(scope),
            hs_level: app.hs_level,
            generation: app.search_gen,
        });
    }
}

pub(super) fn report_ready(app: &App) -> bool {
    app.analytics_gen == app.search_gen
        && app.analytics.is_some()
        && app.analytics_loaded[AnalyticsView::Companies.index()]
        && app.analytics_loaded[AnalyticsView::Products.index()]
        && app.analytics_loaded[AnalyticsView::Countries.index()]
        && app.analytics_loaded[AnalyticsView::Prices.index()]
}

pub(super) fn comparison_query(app: &App) -> Query {
    let mut q = app.active_query.clone();
    let text = app.compare_text.trim();
    if !text.is_empty() {
        q.text = text.to_string();
    }
    let year = app.compare_year.trim();
    if !year.is_empty() {
        q.filters.year = year.to_string();
    }
    q
}

pub(super) fn request_compare(app: &mut App) {
    let q = comparison_query(app);
    if q.is_empty() {
        return;
    }
    app.compare_gen = app.compare_gen.wrapping_add(1);
    app.compare_loading = true;
    app.compare_query = Some(q.clone());
    app.compare_analytics = None;
    let _ = app.analytics_tx.send(WorkerReq::Compare {
        q: Box::new(q),
        generation: app.compare_gen,
    });
}

pub(super) fn open_group_explorer(app: &mut App, kind: AnalyticsSectionKind) {
    if app.active_query.is_empty() {
        return;
    }
    app.group_explorer = Some(GroupExplorerState {
        kind,
        generation: app.search_gen,
        loading: true,
        rows: Vec::new(),
        label_filter: String::new(),
        sort: GroupSort::Value,
        descending: true,
    });
    let _ = app.analytics_tx.send(WorkerReq::AnalyticsSection {
        q: Box::new(app.active_query.clone()),
        kind,
        limit: super::FULL_SECTION_LIMIT,
        hs_level: app.hs_level,
        generation: app.search_gen,
    });
}

pub(super) fn request_underpricing(app: &mut App) {
    if app.active_query.is_empty() {
        return;
    }
    app.underpricing = None;
    app.underpricing_loading = true;
    invalidate_underpricing_generation(&mut app.underpricing_gen);
    let _ = app.analytics_tx.send(WorkerReq::Underpricing {
        q: Box::new(app.active_query.clone()),
        threshold: 0.5,
        generation: app.underpricing_gen,
    });
}

pub(super) fn request_pivot(app: &mut App) {
    if app.active_query.is_empty() {
        return;
    }
    app.pivot = None;
    app.analytics_loaded[AnalyticsView::Pivot.index()] = false;
    app.analytics_loading = true;
    let others = app.t().others;
    let _ = app.analytics_tx.send(WorkerReq::Pivot {
        q: Box::new(app.active_query.clone()),
        row_dim: app.pivot_row_dim,
        col_dim: app.pivot_col_dim,
        metric: app.pivot_metric,
        others_label: others.to_string(),
        generation: app.search_gen,
    });
}

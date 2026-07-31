//! Graphical interface: search bar, filters, paginated table, record card,
//! import/export progress, and settings.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use crate::db::{
    Analytics, AnalyticsFilterAction, AnalyticsSectionKind, CompanyProfile, Db, Filters, PivotDim,
    PivotMetric, PivotResult, Query, RecordCard, Undervaluation,
};
use crate::i18n::{Lang, Tr, tr};
use crate::import::FileSummary;
use crate::search::{FieldInfo, QueryExpr, default_field_catalog, result_field_catalog};
use crate::workers::{self, Msg, PAGE_SIZE, WorkerReq};

mod actions;
mod advanced_query;
mod analytics_groups;
mod analytics_view;
mod columns;
mod commands;
mod compare_view;
mod dialogs;
mod field_state;
mod format;
mod import_report_view;
mod messages;
mod month_chart;
mod overview_view;
mod pivot_view;
mod platform;
mod price_view;
mod profile_view;
mod query_history;
mod reports;
mod requests;
mod results_table;
mod search_controls;
mod state;
mod status_bar;
mod stored_queries;
mod theme;
mod toolbar;
mod ui_text;
mod underpricing_view;
mod widgets;

use analytics_groups::{GroupExplorerAction, GroupExplorerState, group_explorer_window};
use analytics_view::{AnalyticsViewInput, analytics_view_panel};
use dialogs::{
    ResultsEmptyAction, SettingsAction, SettingsWindowInput, card_window, confirm_clear_window,
    help_window, results_empty_state, settings_window, startup_state,
};
use import_report_view::import_report_window;
use month_chart::MonthMetric;
use pivot_view::pivot_tsv;
pub use platform::default_db_path;
use platform::open_parent_folder;
use profile_view::{ProfileViewAction, profile_view};
use reports::{report_html, report_markdown};
use results_table::{ResultsTableInput, RowMenuAction, results_table};
use state::{AnalyticsView, AppTab, OpState, StatusLine};
use status_bar::{StatusBarInput, status_bar_panel};
use stored_queries::StoredQuery;
use theme::{setup_fonts, setup_style};
use toolbar::{ToolbarInput, toolbar_panel};
use ui_text::GuidedQuestionAction;

/// Interface accent color.
const ACCENT: egui::Color32 = egui::Color32::from_rgb(37, 99, 235);
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const FULL_SECTION_LIMIT: u64 = 20_000;
const RECENT_QUERIES_V2_META: &str = "recent_queries_v2";
const SAVED_QUERIES_V2_META: &str = "saved_queries_v2";
const RECENT_QUERY_LIMIT: usize = 12;

pub struct App {
    lang: Lang,
    db_path: PathBuf,
    /// Lightweight connection for instant operations, such as cards and settings.
    lite_db: Option<Db>,

    query_text: String,
    filters: Filters,
    advanced_query: Option<QueryExpr>,
    search_fields: Vec<FieldInfo>,
    recent_queries: Vec<StoredQuery>,
    saved_queries: Vec<StoredQuery>,
    show_filters: bool,
    show_advanced_search: bool,
    active_query: Query,
    page: u64,
    total: Option<u64>,
    rows: Vec<Vec<String>>,
    row_ids: Vec<i64>,
    page_has_next: bool,
    /// Per result row: Some(first file) when the row is a kept duplicate.
    result_dups: Vec<Option<String>>,
    analytics: Option<Analytics>,
    active_tab: AppTab,
    analytics_limit: u64,
    /// Generation of the query the loaded analytics belong to.
    analytics_gen: u64,
    /// Active sub-tab on the Analytics view.
    analytics_view: AnalyticsView,
    /// Which sub-tabs are loaded for `analytics_gen` (indexed by view).
    analytics_loaded: [bool; AnalyticsView::COUNT],
    analytics_loading: bool,
    /// Product code grouping level: 2/4/6 digits or 10 for full codes.
    hs_level: u8,
    group_explorer: Option<GroupExplorerState>,
    month_metric: MonthMetric,
    /// Pivot (cross-tab) state.
    pivot: Option<PivotResult>,
    pivot_row_dim: PivotDim,
    pivot_col_dim: PivotDim,
    pivot_metric: PivotMetric,
    compare_text: String,
    compare_year: String,
    compare_query: Option<Query>,
    compare_analytics: Option<Analytics>,
    compare_loading: bool,
    compare_gen: u64,
    /// Undervaluation scan (in the Prices sub-tab).
    underpricing: Option<Undervaluation>,
    underpricing_loading: bool,
    underpricing_gen: u64,
    selected: HashSet<usize>,
    select_anchor: Option<usize>,
    result_fields: Vec<FieldInfo>,
    visible_cols: Vec<bool>,
    search_gen: u64,
    search_in_flight: bool,
    count_in_flight: bool,
    last_search_ms: Option<u64>,

    db_total_rows: Option<u64>,
    db_ready: bool,
    startup_started: Instant,
    status: StatusLine,

    op: Option<OpState>,
    import_report: Option<Vec<FileSummary>>,

    card: Option<RecordCard>,
    card_open: bool,
    show_settings: bool,
    show_help: bool,
    /// Whether "help seen" has already been recorded in this session, so the
    /// flag is written once instead of on every repaint.
    help_seen_saved: bool,
    confirm_clear: bool,
    columns_filter: String,
    add_filter_search: String,
    advanced_field_search: String,

    /// Open company dossier; `None` means the normal Results/Analytics view.
    profile: Option<CompanyProfile>,
    profile_loading: bool,
    profile_gen: u64,

    msg_rx: Receiver<Msg>,
    msg_tx: Sender<Msg>,
    search_tx: Sender<WorkerReq>,
    analytics_tx: Sender<WorkerReq>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx, ACCENT);

        let db_path = default_db_path();
        let result_fields = result_field_catalog(Vec::<String>::new());
        let visible_cols = vec![true; result_fields.len()];
        let search_fields = default_field_catalog();

        let (msg_tx, msg_rx) = channel::<Msg>();
        let (search_tx, search_rx) = channel::<WorkerReq>();
        let (analytics_tx, analytics_rx) = channel::<WorkerReq>();
        workers::spawn_startup(db_path.clone(), msg_tx.clone(), cc.egui_ctx.clone());
        workers::spawn_search_worker(
            db_path.clone(),
            search_rx,
            msg_tx.clone(),
            cc.egui_ctx.clone(),
        );
        workers::spawn_analytics_worker(
            db_path.clone(),
            analytics_rx,
            msg_tx.clone(),
            cc.egui_ctx.clone(),
        );

        App {
            lang: Lang::default(),
            db_path,
            lite_db: None,
            query_text: String::new(),
            filters: Filters::default(),
            advanced_query: None,
            search_fields,
            recent_queries: Vec::new(),
            saved_queries: Vec::new(),
            show_filters: false,
            show_advanced_search: false,
            active_query: Query::default(),
            page: 0,
            total: None,
            rows: Vec::new(),
            row_ids: Vec::new(),
            page_has_next: false,
            result_dups: Vec::new(),
            analytics: None,
            active_tab: AppTab::Results,
            analytics_limit: 10,
            analytics_gen: 0,
            analytics_view: AnalyticsView::default(),
            analytics_loaded: [false; AnalyticsView::COUNT],
            analytics_loading: false,
            hs_level: 10,
            group_explorer: None,
            month_metric: MonthMetric::default(),
            pivot: None,
            pivot_row_dim: PivotDim::Recipient,
            pivot_col_dim: PivotDim::Month,
            pivot_metric: PivotMetric::Value,
            compare_text: String::new(),
            compare_year: String::new(),
            compare_query: None,
            compare_analytics: None,
            compare_loading: false,
            compare_gen: 0,
            underpricing: None,
            underpricing_loading: false,
            underpricing_gen: 0,
            selected: HashSet::new(),
            select_anchor: None,
            result_fields,
            visible_cols,
            search_gen: 0,
            search_in_flight: false,
            count_in_flight: false,
            last_search_ms: None,
            db_total_rows: None,
            db_ready: false,
            startup_started: Instant::now(),
            status: StatusLine::default(),
            op: None,
            import_report: None,
            card: None,
            card_open: false,
            show_settings: false,
            show_help: false,
            help_seen_saved: false,
            confirm_clear: false,
            columns_filter: String::new(),
            add_filter_search: String::new(),
            advanced_field_search: String::new(),
            profile: None,
            profile_loading: false,
            profile_gen: 0,
            msg_rx,
            msg_tx,
            search_tx,
            analytics_tx,
        }
    }

    fn t(&self) -> &'static Tr {
        tr(self.lang)
    }

    fn persist(&self, key: &str, value: &str) {
        if let Some(db) = &self.lite_db {
            db.meta_set(key, value);
        }
    }

    fn persist_hidden_cols(&self) {
        field_state::persist_hidden_cols(self);
    }

    fn set_result_fields(&mut self, fields: Vec<FieldInfo>) {
        field_state::set_result_fields(self, fields);
    }

    fn refresh_result_fields(&mut self) {
        field_state::refresh_result_fields(self);
    }

    fn refresh_search_fields(&mut self) {
        field_state::refresh_search_fields(self);
    }

    fn remember_recent_query(&mut self, query: &Query) {
        query_history::remember_recent_query(self, query);
    }

    fn save_current_query(&mut self) {
        query_history::save_current_query(self);
    }

    fn clear_recent_queries(&mut self) {
        query_history::clear_recent_queries(self);
    }

    fn remove_saved_query(&mut self, index: usize) {
        query_history::remove_saved_query(self, index);
    }

    fn apply_stored_query(&mut self, query: Query) {
        query_history::apply_stored_query(self, query);
    }

    fn start_search(&mut self, reset_page: bool) {
        requests::start_search(self, reset_page);
    }

    fn goto_page(&mut self, page: u64) {
        requests::goto_page(self, page);
    }

    fn request_analytics(&mut self) {
        requests::request_analytics(self);
    }

    fn report_ready(&self) -> bool {
        requests::report_ready(self)
    }

    fn request_compare(&mut self) {
        requests::request_compare(self);
    }

    fn open_group_explorer(&mut self, kind: AnalyticsSectionKind) {
        requests::open_group_explorer(self, kind);
    }

    fn request_underpricing(&mut self) {
        requests::request_underpricing(self);
    }

    fn request_pivot(&mut self) {
        requests::request_pivot(self);
    }

    fn page_count(&self) -> u64 {
        match self.total {
            Some(total) => total.div_ceil(PAGE_SIZE).max(1),
            None if !self.search_in_flight && self.page_has_next => self.page + 2,
            None => self.page + 1,
        }
    }

    fn drain_messages(&mut self, ctx: &egui::Context) {
        messages::drain_messages(self, ctx);
    }

    fn pick_and_import(&mut self, ctx: &egui::Context) {
        commands::pick_and_import(self, ctx);
    }

    fn pick_and_export(&mut self, ctx: &egui::Context) {
        commands::pick_and_export(self, ctx);
    }

    fn save_report_html(&mut self, html: String) {
        commands::save_report_html(self, html);
    }

    fn start_clear_db(&mut self, ctx: &egui::Context) {
        commands::start_clear_db(self, ctx);
    }

    fn start_optimize_db(&mut self, ctx: &egui::Context) {
        commands::start_optimize_db(self, ctx);
    }

    fn open_card(&mut self, row_index: usize) {
        actions::open_card(self, row_index);
    }

    fn open_card_by_id(&mut self, id: i64) {
        actions::open_card_by_id(self, id);
    }

    fn close_profile(&mut self) {
        actions::close_profile(self);
    }

    fn run_guided_question(&mut self, action: GuidedQuestionAction) {
        actions::run_guided_question(self, action);
    }

    fn handle_row_click(&mut self, i: usize, modifiers: egui::Modifiers) {
        actions::handle_row_click(self, i, modifiers);
    }

    fn copy_selected_rows(&self, ctx: &egui::Context) {
        actions::copy_selected_rows(self, ctx);
    }

    fn apply_menu_action(&mut self, ctx: &egui::Context, action: RowMenuAction) {
        actions::apply_menu_action(self, ctx, action);
    }

    fn apply_analytics_filter(&mut self, action: AnalyticsFilterAction) {
        actions::apply_analytics_filter(self, action);
    }

    // ---------- panels ----------

    fn ui_toolbar(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let t = self.t();
        let active_query_text = self.active_query.text.clone();
        let action = toolbar_panel(
            root,
            ToolbarInput {
                lang: self.lang,
                t,
                db_ready: self.db_ready,
                busy: self.op.is_some() || !self.db_ready,
                db_total_rows: self.db_total_rows,
                total: self.total,
                rows_empty: self.rows.is_empty(),
                active_query_text: &active_query_text,
                active_tab: &mut self.active_tab,
                query_text: &mut self.query_text,
                filters: &mut self.filters,
                advanced_query: &mut self.advanced_query,
                search_fields: &self.search_fields,
                result_fields: &self.result_fields,
                visible_cols: &mut self.visible_cols,
                recent_queries: &self.recent_queries,
                saved_queries: &self.saved_queries,
                show_filters: &mut self.show_filters,
                show_advanced_search: &mut self.show_advanced_search,
                show_settings: &mut self.show_settings,
                show_help: &mut self.show_help,
                columns_filter: &mut self.columns_filter,
                add_filter_search: &mut self.add_filter_search,
                advanced_field_search: &mut self.advanced_field_search,
            },
        );
        if action.columns_changed {
            self.persist_hidden_cols();
        }
        if action.save_current_query {
            self.save_current_query();
        }
        if let Some(index) = action.remove_saved_query {
            self.remove_saved_query(index);
        }
        if action.clear_recent_queries {
            self.clear_recent_queries();
        }
        if let Some(query) = action.apply_stored_query {
            self.apply_stored_query(query);
        } else if let Some(action) = action.guided_action {
            self.run_guided_question(action);
        } else if action.search && self.db_ready {
            self.start_search(true);
        } else if action.request_analytics && self.db_ready {
            self.request_analytics();
        }
        if action.import && self.db_ready {
            self.pick_and_import(&ctx);
        }
        if action.export && self.db_ready {
            self.pick_and_export(&ctx);
        }
    }

    fn ui_status_bar(&mut self, root: &mut egui::Ui) {
        let action = status_bar_panel(
            root,
            StatusBarInput {
                op: self.op.as_ref(),
                search_in_flight: self.search_in_flight,
                count_in_flight: self.count_in_flight,
                status: &self.status,
                last_search_ms: self.last_search_ms,
                page: self.page,
                page_count: self.page_count(),
                total: self.total,
                rows_len: self.rows.len(),
                page_has_next: self.page_has_next,
                selected_len: self.selected.len(),
                t: self.t(),
            },
        );
        if action.cancel_operation
            && let Some(op) = &self.op
        {
            op.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(p) = action.goto_page {
            self.goto_page(p);
        }
    }

    fn ui_analytics_view(&mut self, root: &mut egui::Ui) {
        if !self.db_ready {
            egui::CentralPanel::default().show_inside(root, |ui| self.ui_startup_state(ui));
            return;
        }

        let actions = analytics_view_panel(
            root,
            AnalyticsViewInput {
                active_query: &self.active_query,
                analytics: self.analytics.as_ref(),
                analytics_loading: self.analytics_loading,
                search_in_flight: self.search_in_flight,
                analytics_view: self.analytics_view,
                analytics_loaded: &self.analytics_loaded,
                analytics_limit: self.analytics_limit,
                month_metric: self.month_metric,
                hs_level: self.hs_level,
                pivot: self.pivot.as_ref(),
                pivot_row_dim: self.pivot_row_dim,
                pivot_col_dim: self.pivot_col_dim,
                pivot_metric: self.pivot_metric,
                underpricing: self.underpricing.as_ref(),
                underpricing_loading: self.underpricing_loading,
                compare_text: &self.compare_text,
                compare_year: &self.compare_year,
                compare_analytics: self.compare_analytics.as_ref(),
                compare_query: self.compare_query.as_ref(),
                compare_loading: self.compare_loading,
                report_ready: self.report_ready(),
                lang: self.lang,
                t: self.t(),
            },
        );

        if let Some(metric) = actions.new_metric {
            self.month_metric = metric;
        }
        if let Some(v) = actions.new_view {
            self.analytics_view = v;
            self.request_analytics();
        }
        if let Some(level) = actions.new_hs {
            self.hs_level = level;
            self.analytics_loaded[AnalyticsView::Products.index()] = false;
            self.request_analytics();
        }
        if actions.copy_pivot
            && let Some(pivot) = &self.pivot
        {
            let tsv = pivot_tsv(pivot, self.pivot_row_dim, self.pivot_col_dim, self.lang);
            root.ctx().copy_text(tsv);
        }
        if actions.copy_report
            && let Some(analytics) = &self.analytics
        {
            root.ctx()
                .copy_text(report_markdown(analytics, &self.active_query, self.lang));
        }
        if actions.export_report
            && let Some(analytics) = &self.analytics
        {
            let html = report_html(analytics, &self.active_query, self.lang);
            self.save_report_html(html);
        }

        let mut repivot = false;
        if let Some(d) = actions.new_pivot_row {
            self.pivot_row_dim = d;
            repivot = true;
        }
        if let Some(d) = actions.new_pivot_col {
            self.pivot_col_dim = d;
            repivot = true;
        }
        if let Some(m) = actions.new_pivot_metric {
            self.pivot_metric = m;
            repivot = true;
        }
        if repivot {
            self.request_pivot();
        }

        if let Some(text) = actions.compare_text {
            self.compare_text = text;
        }
        if let Some(year) = actions.compare_year {
            self.compare_year = year;
        }
        if actions.run_compare {
            self.request_compare();
        }
        if actions.scan_underpricing {
            self.request_underpricing();
        }
        if let Some(id) = actions.open_card_id {
            self.open_card_by_id(id);
        }
        if actions.show_more {
            self.analytics_limit = 50;
            self.analytics_loaded = [false; AnalyticsView::COUNT];
            self.request_analytics();
        }
        if let Some(action) = actions.filter_action {
            self.apply_analytics_filter(action);
        }
        if let Some(kind) = actions.explore_kind {
            self.open_group_explorer(kind);
        }
        if actions.request_analytics {
            self.request_analytics();
        }
    }

    fn ui_table(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(root, |ui| {
            if !self.db_ready {
                self.ui_startup_state(ui);
                return;
            }
            if self.rows.is_empty() {
                self.ui_results_empty_state(ui);
                return;
            }
            let actions = results_table(
                ui,
                ResultsTableInput {
                    result_fields: &self.result_fields,
                    visible_cols: &self.visible_cols,
                    rows: &self.rows,
                    result_dups: &self.result_dups,
                    selected: &self.selected,
                    t: self.t(),
                },
            );
            if let Some((i, modifiers)) = actions.clicked_row {
                self.handle_row_click(i, modifiers);
            }
            if let Some(i) = actions.open_card {
                self.selected.clear();
                self.selected.insert(i);
                self.select_anchor = Some(i);
                self.open_card(i);
            }
            if let Some(action) = actions.menu_action {
                let ctx = ui.ctx().clone();
                self.apply_menu_action(&ctx, action);
            }
        });
    }

    fn ui_startup_state(&self, ui: &mut egui::Ui) {
        startup_state(ui, &self.db_path, &self.startup_started, &self.status);
    }

    fn ui_results_empty_state(&mut self, ui: &mut egui::Ui) {
        let action = results_empty_state(
            ui,
            self.total,
            self.active_query.is_empty(),
            self.search_in_flight,
            self.count_in_flight,
            self.t(),
        );
        if matches!(action, Some(ResultsEmptyAction::Import)) {
            self.pick_and_import(ui.ctx());
        }
    }

    fn ui_card_window(&mut self, ctx: &egui::Context) {
        let t = self.t();
        card_window(ctx, &mut self.card_open, &mut self.card, t);
    }

    fn ui_profile_view(&mut self, root: &mut egui::Ui) {
        let action = profile_view(
            root,
            self.profile.as_ref(),
            self.profile_loading,
            self.t(),
            self.lang,
        );
        match action {
            Some(ProfileViewAction::Close) => self.close_profile(),
            Some(ProfileViewAction::Filter(filter)) => {
                self.close_profile();
                self.apply_analytics_filter(filter);
            }
            None => {}
        }
    }

    fn ui_group_explorer_window(&mut self, ctx: &egui::Context) {
        let lang = self.lang;
        let action = {
            let Some(explorer) = self.group_explorer.as_mut() else {
                return;
            };
            group_explorer_window(ctx, explorer, lang, FULL_SECTION_LIMIT)
        };
        match action {
            Some(GroupExplorerAction::Close) => self.group_explorer = None,
            Some(GroupExplorerAction::Filter(action)) => {
                self.group_explorer = None;
                self.apply_analytics_filter(action);
            }
            None => {}
        }
    }

    fn ui_import_report(&mut self, ctx: &egui::Context) {
        let Some(report) = &self.import_report else {
            return;
        };
        if import_report_window(ctx, report, self.t()) {
            self.import_report = None;
        }
    }

    fn ui_help_window(&mut self, ctx: &egui::Context) {
        if !self.show_help {
            return;
        }
        // Recorded once per session, not once per repaint. As a per-frame write
        // it hit SQLite on every frame the window was open; during an import,
        // which holds a write transaction, each attempt waited on the busy
        // handler for up to the 30 second timeout and froze the window.
        if !self.help_seen_saved {
            self.persist("help_seen", "1");
            self.help_seen_saved = true;
        }
        let t = self.t();
        help_window(ctx, &mut self.show_help, self.lang, t);
    }

    fn ui_settings_window(&mut self, ctx: &egui::Context) {
        let t = self.t();
        let busy = self.op.is_some() || !self.db_ready;
        let action = settings_window(
            ctx,
            SettingsWindowInput {
                show_settings: &mut self.show_settings,
                lang: &mut self.lang,
                db_path: &self.db_path,
                busy,
                db_ready: self.db_ready,
                t,
                app_version: APP_VERSION,
            },
        );
        match action {
            Some(SettingsAction::Persist { key, value }) => self.persist(key, &value),
            Some(SettingsAction::CopyDbPath) => {
                ctx.copy_text(self.db_path.display().to_string());
                self.status = StatusLine {
                    text: "Database path copied.".to_string(),
                    is_error: false,
                };
            }
            Some(SettingsAction::OpenDbFolder) => match open_parent_folder(&self.db_path) {
                Ok(()) => {
                    self.status = StatusLine {
                        text: "Database folder opened.".to_string(),
                        is_error: false,
                    };
                }
                Err(err) => {
                    self.status = StatusLine {
                        text: format!("{}: {err}", self.t().error),
                        is_error: true,
                    };
                }
            },
            Some(SettingsAction::OptimizeDatabase) => self.start_optimize_db(ctx),
            Some(SettingsAction::ClearDatabase) => self.confirm_clear = true,
            None => {}
        }
    }

    fn ui_confirm_clear(&mut self, ctx: &egui::Context) {
        let t = self.t();
        if confirm_clear_window(ctx, &mut self.confirm_clear, t) {
            self.show_settings = false;
            self.start_clear_db(ctx);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.drain_messages(&ctx);
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.confirm_clear {
                self.confirm_clear = false;
            } else if self.show_help {
                self.show_help = false;
            } else if self.show_settings {
                self.show_settings = false;
            } else if self.card_open {
                self.card_open = false;
            } else {
                self.group_explorer = None;
            }
        }
        // Ctrl+C copies selected rows when focus is not inside a text field.
        let copy_requested = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        if copy_requested && !self.selected.is_empty() && !self.card_open {
            self.copy_selected_rows(&ctx);
        }
        self.ui_toolbar(root);
        self.ui_status_bar(root);
        if self.profile.is_some() || self.profile_loading {
            self.ui_profile_view(root);
        } else {
            match self.active_tab {
                AppTab::Results => self.ui_table(root),
                AppTab::Analytics => self.ui_analytics_view(root),
            }
        }
        self.ui_card_window(&ctx);
        self.ui_group_explorer_window(&ctx);
        self.ui_import_report(&ctx);
        self.ui_settings_window(&ctx);
        self.ui_help_window(&ctx);
        self.ui_confirm_clear(&ctx);
        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.show_help = true;
            self.show_settings = false;
        }
        // Safety repaint: refresh regularly while a background operation runs.
        if self.op.is_some()
            || self.search_in_flight
            || self.count_in_flight
            || self.analytics_loading
            || self.profile_loading
            || self.underpricing_loading
            || self
                .group_explorer
                .as_ref()
                .map(|explorer| explorer.loading)
                .unwrap_or(false)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::state::invalidate_underpricing_generation;
    use super::ui_text::{
        GuidedQuestionKind, condition_op_label, exact_edrpou_candidate, guided_question_title,
        guided_questions_for,
    };
    use crate::db::Filters;
    use crate::i18n::{Lang, tr};
    use crate::search::ConditionOp;

    #[test]
    fn invalidating_underpricing_generation_rejects_stale_results() {
        let mut generation = 7;
        let stale_generation = generation;

        invalidate_underpricing_generation(&mut generation);

        assert_ne!(generation, stale_generation);
    }

    #[test]
    fn guided_questions_cover_all_languages() {
        let kinds = [
            GuidedQuestionKind::ProductCompanies,
            GuidedQuestionKind::ProductAllCompanies,
            GuidedQuestionKind::ProductGoods,
            GuidedQuestionKind::ProductCountries,
            GuidedQuestionKind::ProductPrices,
            GuidedQuestionKind::ProductTimeline,
            GuidedQuestionKind::ProductCompaniesByMonth,
            GuidedQuestionKind::CompanyProfile,
            GuidedQuestionKind::CompanyGoods,
            GuidedQuestionKind::CompanySuppliers,
            GuidedQuestionKind::CompanyCountries,
            GuidedQuestionKind::CompanyTimeline,
            GuidedQuestionKind::CompanyGoodsByMonth,
            GuidedQuestionKind::MarketCompanies,
            GuidedQuestionKind::MarketGoods,
            GuidedQuestionKind::MarketCountries,
            GuidedQuestionKind::MarketPrices,
        ];
        for lang in Lang::ALL {
            for kind in kinds {
                assert!(!guided_question_title(kind, lang).trim().is_empty());
            }
        }
    }

    #[test]
    fn v2_search_translations_cover_all_languages() {
        let ops = [
            ConditionOp::Contains,
            ConditionOp::Equals,
            ConditionOp::StartsWith,
            ConditionOp::IsAnyOf,
            ConditionOp::Range,
            ConditionOp::IsEmpty,
            ConditionOp::IsNotEmpty,
        ];
        for lang in Lang::ALL {
            let t = tr(lang);
            for value in [
                t.v2_query_summary,
                t.v2_add_filter,
                t.v2_advanced,
                t.v2_clear_advanced,
                t.v2_advanced_search,
                t.v2_logic_hint,
                t.v2_match,
                t.v2_match_all,
                t.v2_match_any,
                t.v2_exclude_group,
                t.v2_exclude_rule,
                t.v2_excluding,
                t.v2_add_group,
                t.v2_edit_in_filters,
                t.v2_edit,
                t.v2_duplicate,
                t.v2_toggle_not,
                t.v2_remove,
                t.v2_more,
                t.v2_add_condition,
                t.v2_add_and_group,
                t.v2_add_or_group,
                t.v2_clear_group,
                t.v2_group,
                t.v2_and_group,
                t.v2_or_group,
                t.v2_no_value,
                t.v2_value_hint,
                t.v2_list_hint,
                t.v2_from_hint,
                t.v2_to_hint,
            ] {
                assert!(
                    !value.trim().is_empty(),
                    "missing V2 translation for {lang:?}"
                );
            }
            for op in ops {
                assert!(
                    !condition_op_label(op, t).trim().is_empty(),
                    "missing V2 operator translation for {lang:?}"
                );
            }
        }
    }

    #[test]
    fn guided_questions_match_input_context() {
        let product = guided_questions_for("Widget", &Filters::default());
        assert!(
            product
                .iter()
                .any(|(_, kind)| *kind == GuidedQuestionKind::ProductCompanies)
        );
        assert!(
            product
                .iter()
                .any(|(_, kind)| *kind == GuidedQuestionKind::ProductPrices)
        );

        let filters = Filters {
            edrpou: "12345678".into(),
            ..Filters::default()
        };
        let company = guided_questions_for("", &filters);
        assert!(
            company
                .iter()
                .any(|(_, kind)| *kind == GuidedQuestionKind::CompanyProfile)
        );
        assert_eq!(
            exact_edrpou_candidate("", &filters),
            Some("12345678".to_string())
        );

        let filters = Filters {
            year: "2024".into(),
            origin_country: "CN".into(),
            ..Filters::default()
        };
        let market = guided_questions_for("", &filters);
        assert!(
            market
                .iter()
                .any(|(_, kind)| *kind == GuidedQuestionKind::MarketCompanies)
        );
        assert!(
            market
                .iter()
                .any(|(_, kind)| *kind == GuidedQuestionKind::MarketPrices)
        );
    }
}

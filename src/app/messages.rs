use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::App;
use super::state::{AnalyticsView, OpKind, OpState, StatusLine};
use super::stored_queries::decode_stored_queries_with_fallback;
use crate::db::AnalyticsScope;
use crate::export::ExportError;
use crate::i18n::{Lang, fmt, group_digits};
use crate::workers::{self, Msg, WorkerReq};

pub(super) fn drain_messages(app: &mut App, ctx: &egui::Context) {
    while let Ok(msg) = app.msg_rx.try_recv() {
        match msg {
            Msg::StartupProgress(phase) => app.startup_phase = Some(phase),
            Msg::StartupDone(result) => handle_startup_done(app, ctx, result),
            Msg::SearchPage {
                generation,
                fields,
                ids,
                rows,
                dups,
                has_next,
                ms,
            } => {
                if generation == app.search_gen {
                    app.set_result_fields(fields);
                    app.row_ids = ids;
                    app.rows = rows;
                    app.result_dups = dups;
                    app.page_has_next = has_next;
                    if app.page == 0 && app.rows.is_empty() {
                        app.total = Some(0);
                        app.count_in_flight = false;
                    }
                    app.last_search_ms = Some(ms);
                    app.search_in_flight = false;
                    app.selected.clear();
                    app.select_anchor = None;
                }
            }
            Msg::SearchCount { generation, total } => {
                if generation == app.search_gen {
                    app.total = Some(total);
                    app.count_in_flight = false;
                }
            }
            Msg::AnalyticsDone {
                generation,
                scope,
                analytics,
            } => handle_analytics_done(app, generation, scope, *analytics),
            Msg::AnalyticsSectionDone {
                generation,
                section,
            } => {
                if let Some(explorer) = &mut app.group_explorer
                    && explorer.generation == generation
                    && explorer.kind == section.kind
                {
                    explorer.rows = section.rows;
                    explorer.loading = false;
                }
            }
            Msg::SearchError {
                generation,
                message,
            } => handle_search_error(app, generation, message),
            Msg::ProfileDone {
                generation,
                profile,
            } => {
                if generation == app.profile_gen {
                    app.profile = Some(*profile);
                    app.profile_loading = false;
                }
            }
            Msg::CompareDone {
                generation,
                query,
                analytics,
            } => {
                if generation == app.compare_gen {
                    app.compare_query = Some(*query);
                    app.compare_analytics = Some(*analytics);
                    app.compare_loading = false;
                }
            }
            Msg::CompareError {
                generation,
                message,
            } => {
                if generation == app.compare_gen {
                    app.compare_loading = false;
                    app.status = StatusLine {
                        text: format!("{}: {message}", app.t().error),
                        is_error: true,
                    };
                }
            }
            Msg::PivotDone { generation, pivot } => {
                if generation == app.search_gen {
                    app.pivot = Some(*pivot);
                    app.analytics_gen = generation;
                    app.analytics_loaded[AnalyticsView::Pivot.index()] = true;
                    app.analytics_loading = false;
                }
            }
            Msg::UnderpricingDone { generation, result } => {
                if generation == app.underpricing_gen {
                    app.underpricing = Some(*result);
                    app.underpricing_loading = false;
                }
            }
            Msg::Stats(total) => app.db_total_rows = Some(total),
            Msg::Import(ev) => {
                if let Some(op) = &mut app.op {
                    op.last_event = Some(ev);
                }
            }
            Msg::ImportDone(summaries, total_rows) => {
                app.op = None;
                app.db_total_rows = Some(total_rows);
                app.refresh_search_fields();
                app.refresh_result_fields();
                if !summaries.is_empty() {
                    let imported: u64 = summaries.iter().map(|s| s.imported).sum();
                    let dups: u64 = summaries.iter().map(|s| s.duplicates).sum();
                    let errors = summaries.iter().filter(|s| s.error.is_some()).count();
                    app.status = StatusLine {
                        text: fmt(
                            app.t().import_done,
                            &[
                                &group_digits(imported),
                                &group_digits(dups),
                                &errors.to_string(),
                            ],
                        ),
                        is_error: errors > 0,
                    };
                    app.import_report = Some(summaries);
                }
                let _ = app.search_tx.send(WorkerReq::Stats);
                app.start_search(true);
            }
            Msg::ExportProgress(done, total) => {
                if let Some(op) = &mut app.op {
                    op.export_progress = (done, total);
                }
            }
            Msg::ExportDone(result) => {
                app.op = None;
                app.status = export_status(app, result);
            }
            Msg::DbCleared(result) => {
                app.op = None;
                if result.is_ok() {
                    app.refresh_search_fields();
                }
                app.status = match result {
                    Ok(()) => StatusLine {
                        text: app.t().db_cleared.to_string(),
                        is_error: false,
                    },
                    Err(e) => StatusLine {
                        text: format!("{}: {e}", app.t().error),
                        is_error: true,
                    },
                };
                let _ = app.search_tx.send(WorkerReq::Stats);
                app.start_search(true);
            }
            Msg::MaintenanceDone(result) => {
                app.op = None;
                app.status = match result {
                    Ok(message) => StatusLine {
                        text: message,
                        is_error: false,
                    },
                    Err(message) => StatusLine {
                        text: format!("{}: {message}", app.t().error),
                        is_error: true,
                    },
                };
            }
            Msg::Fatal(message) => {
                // A worker that died still has to release the UI. Without this
                // the operation stayed "in progress" forever: `busy` gates the
                // whole toolbar, and Cancel only flips a flag nobody reads any
                // more, so the window was stuck until the app was restarted.
                app.op = None;
                app.status = StatusLine {
                    text: format!("{}: {message}", app.t().error),
                    is_error: true,
                };
            }
        }
    }
}

fn handle_startup_done(
    app: &mut App,
    ctx: &egui::Context,
    result: Result<workers::StartupData, String>,
) {
    match result {
        Ok(data) => {
            let workers::StartupData {
                db,
                lang_code,
                theme,
                zoom,
                hidden_cols,
                recent_queries_v1,
                saved_queries_v1,
                recent_queries_v2,
                saved_queries_v2,
                first_run,
                result_fields,
                search_fields,
                total_rows,
                unindexed_rows,
            } = data;
            app.lang = lang_code
                .as_deref()
                .map(Lang::from_code)
                .unwrap_or_default();
            ctx.set_theme(match theme.as_deref() {
                Some("dark") => egui::Theme::Dark,
                _ => egui::Theme::Light,
            });
            if let Some(zoom) = zoom.and_then(|z| z.parse::<f32>().ok()) {
                ctx.set_zoom_factor(zoom.clamp(0.6, 2.0));
            }
            let hidden: HashSet<String> = hidden_cols
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect();
            app.visible_cols = result_fields
                .iter()
                .map(|field| !hidden.contains(&field.id))
                .collect();
            app.result_fields = result_fields;
            app.search_fields = search_fields;
            app.recent_queries =
                decode_stored_queries_with_fallback(recent_queries_v2, recent_queries_v1);
            app.saved_queries =
                decode_stored_queries_with_fallback(saved_queries_v2, saved_queries_v1);
            app.db_total_rows = Some(total_rows);
            app.db_ready = true;
            app.lite_db = Some(*db);
            app.show_help = first_run;
            app.status = StatusLine::default();

            if total_rows == 0 {
                app.total = Some(0);
                app.rows.clear();
                app.row_ids.clear();
                app.result_dups.clear();
            } else {
                let _ = app.search_tx.send(WorkerReq::Stats);
                app.start_search(true);
            }

            if unindexed_rows > 0 {
                let cancel = Arc::new(AtomicBool::new(false));
                app.op = Some(OpState {
                    kind: OpKind::Import,
                    cancel: cancel.clone(),
                    last_event: None,
                    export_progress: (0, 0),
                });
                workers::spawn_index_repair(
                    app.db_path.clone(),
                    cancel,
                    app.msg_tx.clone(),
                    ctx.clone(),
                );
            }
        }
        Err(message) => {
            app.db_ready = false;
            app.search_in_flight = false;
            app.count_in_flight = false;
            app.status = StatusLine {
                text: format!("{}: {message}", app.t().error),
                is_error: true,
            };
        }
    }
}

fn handle_analytics_done(
    app: &mut App,
    generation: u64,
    scope: Option<AnalyticsScope>,
    analytics: crate::db::Analytics,
) {
    if generation != app.search_gen {
        return;
    }
    match app.analytics.as_mut() {
        Some(existing) if app.analytics_gen == generation => {
            existing.overview = analytics.overview;
            existing.months = analytics.months;
            match scope {
                None => {}
                Some(AnalyticsScope::Companies) => {
                    existing.company_sections = analytics.company_sections;
                }
                Some(AnalyticsScope::Products) => {
                    existing.product_sections = analytics.product_sections;
                }
                Some(AnalyticsScope::Countries) => {
                    existing.country_sections = analytics.country_sections;
                }
                Some(AnalyticsScope::Prices) => {
                    existing.price_sections = analytics.price_sections;
                }
            }
        }
        _ => {
            app.analytics = Some(analytics);
            app.analytics_loaded = [false; AnalyticsView::COUNT];
        }
    }
    app.analytics_gen = generation;
    app.analytics_loaded[AnalyticsView::from_scope(scope).index()] = true;
    app.analytics_loading = false;
}

fn handle_search_error(app: &mut App, generation: u64, message: String) {
    if generation == app.search_gen {
        app.search_in_flight = false;
        app.count_in_flight = false;
        app.analytics_loading = false;
        app.status = StatusLine {
            text: format!("{}: {message}", app.t().error),
            is_error: true,
        };
    }
    if let Some(explorer) = &mut app.group_explorer
        && explorer.generation == generation
    {
        explorer.loading = false;
    }
}

fn export_status(app: &App, result: Result<(u64, std::path::PathBuf), ExportError>) -> StatusLine {
    match result {
        Ok((written, path)) => StatusLine {
            text: format!(
                "{} \u{2192} {}",
                fmt(app.t().export_done, &[&group_digits(written)]),
                path.display()
            ),
            is_error: false,
        },
        Err(ExportError::TooManyRowsForXlsx(_)) => StatusLine {
            text: app.t().xlsx_limit.to_string(),
            is_error: true,
        },
        Err(ExportError::Cancelled) => StatusLine {
            text: app.t().cancelled.to_string(),
            is_error: false,
        },
        Err(ExportError::UnsupportedExtension(ext)) => StatusLine {
            text: if ext.is_empty() {
                "Unsupported export extension. Use .csv or .xlsx.".to_string()
            } else {
                format!("Unsupported export extension: .{ext}. Use .csv or .xlsx.")
            },
            is_error: true,
        },
        Err(ExportError::Other(e)) => StatusLine {
            text: format!("{}: {e}", app.t().error),
            is_error: true,
        },
    }
}

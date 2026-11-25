//! Background threads for search, import, and export. The GUI never blocks.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use crate::db::{Db, PivotLimits, Query, analytics_should_run};

mod jobs;
mod protocol;
mod startup;

pub use jobs::{spawn_clear_db, spawn_export, spawn_import, spawn_index_repair, spawn_optimize_db};
pub use protocol::{ImportEvent, Msg, PAGE_SIZE, StartupData, WorkerReq};
pub use startup::spawn_startup;

enum SearchCountReq {
    Count { q: Box<Query>, generation: u64 },
    ClearCache,
}

/// Handles the analytics-family requests shared by the search and analytics
/// workers, so the dispatch logic lives in exactly one place.
///
/// Returns `None` once the request has been handled (or skipped because the
/// query is too broad to run). Requests that are not part of this family
/// (`Search`, `Stats`) are returned unchanged as `Some(req)` so the calling
/// worker can handle them itself.
fn handle_analytics_req(
    db: &Db,
    req: WorkerReq,
    tx: &Sender<Msg>,
    ctx: &egui::Context,
) -> Option<WorkerReq> {
    match req {
        WorkerReq::Analytics {
            q,
            limit,
            scope,
            hs_level,
            generation,
        } => {
            if !analytics_should_run(&q) {
                return None;
            }
            let msg = match db.analytics_scoped(&q, limit, scope, hs_level) {
                Ok(analytics) => Msg::AnalyticsDone {
                    generation,
                    scope,
                    analytics: Box::new(analytics),
                },
                Err(e) => Msg::SearchError {
                    generation,
                    message: e.to_string(),
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
        WorkerReq::AnalyticsSection {
            q,
            kind,
            limit,
            hs_level,
            generation,
        } => {
            if !analytics_should_run(&q) {
                return None;
            }
            let msg = match db.analytics_section(&q, kind, hs_level, limit) {
                Ok(section) => Msg::AnalyticsSectionDone {
                    generation,
                    section: Box::new(section),
                },
                Err(e) => Msg::SearchError {
                    generation,
                    message: e.to_string(),
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
        WorkerReq::Profile { edrpou, generation } => {
            let msg = match db.company_profile(&edrpou, 10) {
                Ok(profile) => Msg::ProfileDone {
                    generation,
                    profile: Box::new(profile),
                },
                Err(e) => Msg::SearchError {
                    generation,
                    message: e.to_string(),
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
        WorkerReq::Pivot {
            q,
            row_dim,
            col_dim,
            metric,
            others_label,
            generation,
        } => {
            if !analytics_should_run(&q) {
                return None;
            }
            let msg = match db.pivot(
                &q,
                row_dim,
                col_dim,
                metric,
                PivotLimits { rows: 25, cols: 18 },
                &others_label,
            ) {
                Ok(pivot) => Msg::PivotDone {
                    generation,
                    pivot: Box::new(pivot),
                },
                Err(e) => Msg::SearchError {
                    generation,
                    message: e.to_string(),
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
        WorkerReq::Compare { q, generation } => {
            if !analytics_should_run(&q) {
                return None;
            }
            let msg = match db.analytics(&q, 10) {
                Ok(analytics) => Msg::CompareDone {
                    generation,
                    query: q,
                    analytics: Box::new(analytics),
                },
                Err(e) => Msg::CompareError {
                    generation,
                    message: e.to_string(),
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
        WorkerReq::Underpricing {
            q,
            threshold,
            generation,
        } => {
            if !analytics_should_run(&q) {
                return None;
            }
            let msg = match db.undervaluation(&q, threshold, 5, 200) {
                Ok(result) => Msg::UnderpricingDone {
                    generation,
                    result: Box::new(result),
                },
                Err(e) => Msg::SearchError {
                    generation,
                    message: e.to_string(),
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
        // Not an analytics-family request: hand it back to the caller.
        other => return Some(other),
    }
    None
}

/// Persistent search thread with its own connection and COUNT cache.
pub fn spawn_search_worker(
    db_path: PathBuf,
    rx: Receiver<WorkerReq>,
    tx: Sender<Msg>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let mut db: Option<Db> = None;
        let mut count_tx: Option<Sender<SearchCountReq>> = None;
        while let Ok(req) = rx.recv() {
            if db.is_none() {
                match Db::open(&db_path) {
                    Ok(opened) => {
                        let (tx_count, count_rx) = channel::<SearchCountReq>();
                        spawn_search_count_worker(
                            db_path.clone(),
                            count_rx,
                            tx.clone(),
                            ctx.clone(),
                        );
                        count_tx = Some(tx_count);
                        db = Some(opened);
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Fatal(e));
                        ctx.request_repaint();
                        return;
                    }
                }
            }
            let db = db.as_ref().expect("database opened above");
            let count_tx = count_tx.as_ref().expect("count worker opened above");
            // Analytics-family requests are handled by the shared dispatcher;
            // it returns Some(req) only for Search and Stats, which this worker
            // owns.
            let req = match handle_analytics_req(db, req, &tx, &ctx) {
                None => continue,
                Some(req) => req,
            };
            match req {
                WorkerReq::Search {
                    q,
                    page,
                    generation,
                } => {
                    let started = Instant::now();
                    let result = db.search_page_dynamic(&q, PAGE_SIZE + 1, page * PAGE_SIZE);
                    match result {
                        Ok((fields, mut ids, mut rows, mut dups)) => {
                            let empty_first_page = page == 0 && ids.is_empty();
                            let has_next = rows.len() as u64 > PAGE_SIZE;
                            if has_next {
                                ids.truncate(PAGE_SIZE as usize);
                                rows.truncate(PAGE_SIZE as usize);
                                dups.truncate(PAGE_SIZE as usize);
                            }
                            let msg = Msg::SearchPage {
                                generation,
                                fields,
                                ids,
                                rows,
                                dups,
                                has_next,
                                ms: started.elapsed().as_millis() as u64,
                            };
                            let _ = tx.send(msg);
                            ctx.request_repaint();

                            if empty_first_page {
                                let _ = tx.send(Msg::SearchCount {
                                    generation,
                                    total: 0,
                                });
                                ctx.request_repaint();
                            } else {
                                let _ = count_tx.send(SearchCountReq::Count {
                                    q: Box::new((*q).clone()),
                                    generation,
                                });
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::SearchError {
                                generation,
                                message: e.to_string(),
                            });
                            ctx.request_repaint();
                        }
                    }
                }
                WorkerReq::Stats => {
                    let _ = count_tx.send(SearchCountReq::ClearCache);
                    let _ = tx.send(Msg::Stats(db.total_rows()));
                    ctx.request_repaint();
                }
                // Already handled by handle_analytics_req.
                _ => {}
            }
        }
    });
}

/// Persistent analytics thread with its own connection, kept separate from
/// search paging so expensive reports cannot block interactive results.
pub fn spawn_analytics_worker(
    db_path: PathBuf,
    rx: Receiver<WorkerReq>,
    tx: Sender<Msg>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let mut db: Option<Db> = None;
        while let Ok(req) = rx.recv() {
            if db.is_none() {
                match Db::open(&db_path) {
                    Ok(opened) => db = Some(opened),
                    Err(e) => {
                        let _ = tx.send(Msg::Fatal(e));
                        ctx.request_repaint();
                        return;
                    }
                }
            }
            let db = db.as_ref().expect("database opened above");
            // This worker only serves analytics-family requests. Search and Stats
            // are not routed here; if one arrives, the shared dispatcher returns
            // it unchanged and we simply drop it.
            let _ = handle_analytics_req(db, req, &tx, &ctx);
        }
    });
}

fn spawn_search_count_worker(
    db_path: PathBuf,
    rx: Receiver<SearchCountReq>,
    tx: Sender<Msg>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let db = match Db::open(&db_path) {
            Ok(db) => db,
            Err(e) => {
                let _ = tx.send(Msg::Fatal(e));
                ctx.request_repaint();
                return;
            }
        };
        let mut count_cache: Option<(Query, u64)> = None;
        while let Ok(req) = rx.recv() {
            let mut count_req = match req {
                SearchCountReq::Count { q, generation } => Some((*q, generation)),
                SearchCountReq::ClearCache => {
                    count_cache = None;
                    None
                }
            };
            while let Ok(req) = rx.try_recv() {
                match req {
                    SearchCountReq::Count { q, generation } => {
                        count_req = Some((*q, generation));
                    }
                    SearchCountReq::ClearCache => {
                        count_cache = None;
                        count_req = None;
                    }
                }
            }

            let Some((q, generation)) = count_req else {
                continue;
            };
            let msg = match count_cache.as_ref().filter(|(cq, _)| cq == &q) {
                Some((_, total)) => Msg::SearchCount {
                    generation,
                    total: *total,
                },
                None => match db.count(&q) {
                    Ok(total) => {
                        count_cache = Some((q, total));
                        Msg::SearchCount { generation, total }
                    }
                    Err(e) => Msg::SearchError {
                        generation,
                        message: e.to_string(),
                    },
                },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        }
    });
}

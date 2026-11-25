use std::path::PathBuf;
use std::sync::mpsc::Sender;

use super::{Msg, StartupData};
use crate::db::Db;

pub fn spawn_startup(db_path: PathBuf, tx: Sender<Msg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let result = (|| -> Result<StartupData, String> {
            // Opening rebuilds a database carried over from an older version,
            // which is minutes of work on a large one. Forward each phase so
            // the startup screen can say what is happening instead of showing
            // a spinner that is indistinguishable from a hang.
            let phase_tx = tx.clone();
            let phase_ctx = ctx.clone();
            let db = Db::open_with_progress(&db_path, &mut |phase| {
                let _ = phase_tx.send(Msg::StartupProgress(phase));
                phase_ctx.request_repaint();
            })?;
            let lang_code = db.meta_get("lang");
            let theme = db.meta_get("theme");
            let zoom = db.meta_get("zoom");
            let hidden_cols = db.meta_get("hidden_cols");
            let recent_queries_v1 = db.meta_get("recent_queries_v1");
            let saved_queries_v1 = db.meta_get("saved_queries_v1");
            let recent_queries_v2 = db.meta_get("recent_queries_v2");
            let saved_queries_v2 = db.meta_get("saved_queries_v2");
            let first_run = db.meta_get("help_seen").is_none();
            let result_fields = db.result_fields_cached();
            let search_fields = db.field_catalog_cached();
            let total_rows = db.total_rows();
            let unindexed_rows = db.unindexed_rows();
            Ok(StartupData {
                db: Box::new(db),
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
            })
        })();
        let _ = tx.send(Msg::StartupDone(result));
        ctx.request_repaint();
    });
}

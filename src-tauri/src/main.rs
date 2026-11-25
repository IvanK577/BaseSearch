#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod jobs;
mod smoke;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = commands::DesktopState::new(app)?;
            app.manage(state);
            Ok(())
        })
        .on_page_load(|webview, payload| smoke::on_page_load(webview, payload.event()))
        .invoke_handler(tauri::generate_handler![
            commands::workspace_info,
            commands::preview_file,
            commands::search,
            commands::record_detail,
            commands::summary,
            commands::start_import,
            commands::start_migration,
            commands::start_export,
            commands::job_status,
            commands::cancel_job,
            commands::package_smoke_result,
        ])
        .run(tauri::generate_context!())
        .expect("Base Search could not open its main window");
}

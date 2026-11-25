use std::{fs, path::PathBuf};

use tauri::{Webview, webview::PageLoadEvent};

fn fixture_directory() -> Option<PathBuf> {
    let directory = PathBuf::from(std::env::var_os("BASE_SEARCH_SMOKE_DIR")?);
    directory.join("smoke.csv").is_file().then_some(directory)
}

pub fn record_process(file: &str) {
    if let Some(directory) = fixture_directory() {
        let _ = fs::write(directory.join(file), std::process::id().to_string());
    }
}

pub fn on_page_load(webview: &Webview, event: PageLoadEvent) {
    if event != PageLoadEvent::Finished {
        return;
    }
    let Some(test_path) = fixture_directory() else {
        return;
    };
    let Ok(directory) = serde_json::to_string(&test_path) else {
        return;
    };
    let script = format!("void ({})({directory});", include_str!("package-smoke.js"));
    if let Err(error) = webview.eval(script) {
        eprintln!("Could not run native package check: {error}");
    }
}

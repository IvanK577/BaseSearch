use tauri::{Webview, webview::PageLoadEvent};

pub fn on_page_load(webview: &Webview, event: PageLoadEvent) {
    if event != PageLoadEvent::Finished {
        return;
    }
    let Some(test_path) = std::env::var_os("BASE_SEARCH_SMOKE_DIR") else {
        return;
    };
    let Some(test_path) = test_path.to_str() else {
        return;
    };
    if !std::path::Path::new(test_path).join("smoke.csv").is_file() {
        return;
    }
    let Ok(directory) = serde_json::to_string(test_path) else {
        return;
    };
    let script = format!("void ({})({directory});", include_str!("package-smoke.js"));
    if let Err(error) = webview.eval(script) {
        eprintln!("Could not run native package check: {error}");
    }
}

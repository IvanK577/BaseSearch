//! Serves the compiled React workspace. Assets are read from `web-ui/dist`:
//! embedded into the binary for release builds (single-file desktop app) and
//! read live from disk in debug builds for fast frontend iteration.
//!
//! Unknown paths without a file extension fall back to `index.html` so the
//! client-side router can handle deep links.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web-ui/dist"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let path = if raw.is_empty() { "index.html" } else { raw };

    if let Some(response) = serve(path) {
        return response;
    }

    // SPA fallback for client-side routes such as `/analytics` — but never for
    // a request that looks like a missing asset (has a file extension).
    if !path.contains('.')
        && let Some(response) = serve("index.html")
    {
        return response;
    }

    (StatusCode::NOT_FOUND, "Not found").into_response()
}

fn serve(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Vite emits content-hashed filenames under `/assets`, so those can be
    // cached aggressively; everything else stays revalidated.
    let cache_control = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Some(
        (
            [
                (header::CONTENT_TYPE, mime.as_ref()),
                (header::CACHE_CONTROL, cache_control),
            ],
            file.data.into_owned(),
        )
            .into_response(),
    )
}

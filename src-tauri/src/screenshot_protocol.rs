//! The `screenshot:` URI scheme the Transcript view loads a finished Meeting's Screenshots through:
//! `screenshot://localhost/<meeting id>/<file name>`. Only files the core listing returns for that Meeting are
//! served, so nothing outside its `screenshots/` can be read. Tested by hand.

use std::sync::Arc;

use tauri::http::{header, Request, Response, StatusCode};
use tiro_core::store::MeetingStore;

pub const SCHEME: &str = "screenshot";

/// Answers one request: the WebP, or an empty 404 for anything that is not a Screenshot of a Meeting.
pub fn serve(store: &Arc<MeetingStore>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let found = request
        .uri()
        .path()
        .trim_start_matches('/')
        .split_once('/')
        .and_then(|(id, file)| store.screenshot(id, file).ok().flatten())
        .and_then(|shot| std::fs::read(&shot.path).ok());
    match found {
        Some(webp) => Response::builder()
            .header(header::CONTENT_TYPE, "image/webp")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(webp),
        None => Response::builder().status(StatusCode::NOT_FOUND).body(Vec::new()),
    }
    .expect("a valid response")
}

//! Indexing Meetings' Passages for Search in the background at startup, with its progress for the search box, then
//! loading their embeddings for meaning search.

use tiro_core::search::{index_stale, IndexProgress};
use tauri::{AppHandle, Emitter, Manager};

use crate::embedding;
use crate::recorder::AppState;

/// Carries an `IndexProgress`. Matches `EVENTS.indexing` in src/lib/api.ts.
pub const INDEXING_EVENT: &str = "indexing";

/// Indexes, oldest first, the Meetings whose Passages are missing or stale (a new index, or files changed while
/// the app wasn't running), reporting progress as it goes, then loads the stored embeddings and queues the Meetings
/// they don't cover. Search works on whatever is indexed so far.
pub fn index_in_background(app: AppHandle) {
    let spawned = std::thread::Builder::new().name("index-passages".into()).spawn(move || {
        let state = app.state::<AppState>();
        let result = index_stale(&state.store, |progress| {
            *state.indexing.lock().unwrap() = progress;
            let _ = app.emit(INDEXING_EVENT, progress);
        });
        if let Err(e) = result {
            tracing::warn!("could not index Meetings for Search: {e}");
            let done = IndexProgress::default();
            *state.indexing.lock().unwrap() = done;
            let _ = app.emit(INDEXING_EVENT, done);
        }
        embedding::load_all(&app);
    });
    if let Err(e) = spawned {
        tracing::warn!("could not start indexing Meetings for Search: {e}");
    }
}

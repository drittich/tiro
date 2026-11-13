//! Making sure Notes still waiting on the editor's autosave reach `notes.md` before something reads them or the
//! app quits.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::recorder::AppState;

/// Asks the UI to save pending Notes; it answers through the `notes_flushed` command.
pub const FLUSH_NOTES_EVENT: &str = "flush-notes";

/// Long enough for a local save, short enough not to hold up Quit when the UI is gone.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Asks the UI to save any Notes waiting on autosave and waits, briefly, until it has.
pub async fn flush(app: &AppHandle) {
    let state = app.state::<AppState>();
    let request = state.acks.request();
    if app.emit(FLUSH_NOTES_EVENT, request.id()).is_err() {
        return;
    }
    if !state.acks.wait(request, FLUSH_TIMEOUT).await {
        tracing::warn!("the UI did not confirm saving pending Notes");
    }
}

//! Commands the Svelte UI invokes.

use std::path::PathBuf;

use tiro_core::ask::{Answer, AskEvent};
use tiro_core::autostart::{self, default_autostart_path};
use tiro_core::export::{self, ScreenshotsMode};
use tiro_core::keyfile::{default_key_path, read_key, write_key, KeyError};
use tiro_core::retry::RetryReport;
use tiro_core::search::{self, IndexProgress, Meaning, SearchResults};
use tiro_core::spend::{self, MeetingSpend, SpendSummary};
use tiro_core::store::{Meeting, MeetingStatus};
use tiro_core::summary::{Summary, DEFAULT_PROMPT_TEMPLATE};
use tiro_core::transcript::{self, Segment};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::asking;
use crate::capture::{self, Microphone};
use crate::embedding::{self, EmbeddingProgress};
use crate::recorder::{self, AppState};
use crate::settings::{self, Settings, Theme};
use crate::jobs;
use crate::updates::{self, UpdateStatus};

/// The Meeting `meeting_id`, with the error as the UI shows it.
fn meeting(state: &AppState, meeting_id: &str) -> Result<Meeting, String> {
    state.store.get(meeting_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_recording(app: AppHandle, state: State<'_, AppState>) -> Result<Meeting, String> {
    recorder::start(&app, &state, None)
}

#[tauri::command]
pub fn stop_recording(app: AppHandle, state: State<'_, AppState>) {
    recorder::stop(&app, &state);
}

#[tauri::command]
pub fn active_meeting(state: State<'_, AppState>) -> Option<Meeting> {
    state.active.lock().unwrap().as_ref().map(|a| a.meeting.clone())
}

/// A Meeting as the Meetings list shows it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingListing {
    #[serde(flatten)]
    meeting: Meeting,
    /// The file name of the Screenshot its card shows, if it has any.
    thumbnail: Option<String>,
    screenshot_count: usize,
}

/// Every Meeting, newest first, with its card's Screenshot. A Meeting whose Screenshots can't be listed shows none.
#[tauri::command]
pub fn list_meetings(state: State<'_, AppState>) -> Result<Vec<MeetingListing>, String> {
    let meetings = state.store.list().map_err(|e| e.to_string())?;
    Ok(meetings
        .into_iter()
        .map(|meeting| {
            let screenshots = state.store.screenshots(&meeting).unwrap_or_default();
            let thumbnail = tiro_core::screenshots::card_screenshot(&screenshots).map(|s| s.file_name());
            MeetingListing { meeting, thumbnail, screenshot_count: screenshots.len() }
        })
        .collect())
}

#[tauri::command]
pub fn transcript(state: State<'_, AppState>, meeting_id: String) -> Result<Vec<Segment>, String> {
    let meeting = meeting(&state, &meeting_id)?;
    // Shown without fillers and audio-event tags; transcript.jsonl keeps them (ADR-0005).
    state.store.transcript(&meeting).map(|s| transcript::cleaned(&s)).map_err(|e| e.to_string())
}

/// A Screenshot as the Transcript view places it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotView {
    /// Served at `screenshot://localhost/<meeting id>/<file>`.
    file: String,
    /// Meeting time in ms.
    ms: u64,
    /// The index of the shown Segment it goes before (the Segment count when it goes last).
    position: usize,
}

/// The Meeting's Screenshots by time, each placed in the Transcript the `transcript` command returns.
#[tauri::command]
pub fn meeting_screenshots(state: State<'_, AppState>, meeting_id: String) -> Result<Vec<ScreenshotView>, String> {
    let meeting = meeting(&state, &meeting_id)?;
    let shown = state.store.transcript(&meeting).map(|s| transcript::cleaned(&s)).map_err(|e| e.to_string())?;
    let screenshots = state.store.screenshots(&meeting).map_err(|e| e.to_string())?;
    Ok(screenshots
        .iter()
        .map(|s| ScreenshotView { file: s.file_name(), ms: s.ms, position: tiro_core::screenshots::position(&shown, s.ms) })
        .collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    key_path: String,
    /// `None` when a usable key is present; otherwise why it is not.
    key_problem: Option<String>,
    /// The built-in summary prompt, for "Reset to default".
    default_summary_prompt: &'static str,
    /// Whether the login autostart entry exists; the entry itself is the setting.
    autostart: bool,
}

#[tauri::command]
pub fn get_settings() -> SettingsView {
    let path = default_key_path();
    let key_problem = match read_key(&path) {
        Ok(_) => None,
        Err(KeyError::Missing(_)) => Some("No API key set yet.".into()),
        Err(e) => Some(e.to_string()),
    };
    SettingsView {
        settings: settings::load(),
        key_path: path.display().to_string(),
        key_problem,
        default_summary_prompt: DEFAULT_PROMPT_TEMPLATE,
        autostart: autostart::is_enabled(&default_autostart_path()),
    }
}

#[tauri::command]
pub fn list_microphones() -> Result<Vec<Microphone>, String> {
    capture::list_microphones().map_err(|e| e.to_string())
}

/// Saves the STT model, the microphone (`None` for the system default) and, when one is given, a new API key
/// (written with mode 0600).
#[tauri::command]
pub fn save_settings(app: AppHandle, stt_model: String, mic: Option<String>, api_key: Option<String>) -> Result<SettingsView, String> {
    let stt_model = stt_model.trim();
    if stt_model.is_empty() {
        return Err("Choose an STT model.".into());
    }
    if let Some(key) = api_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        write_key(&default_key_path(), key).map_err(|e| e.to_string())?;
        // A first key, or one replacing a rejected key, lets the Meetings be embedded.
        embedding::reload_in_background(app);
    }
    let mic = mic.filter(|m| !m.is_empty());
    settings::save(&Settings { stt_model: stt_model.into(), mic, ..settings::load() })
        .map_err(|e| format!("could not save settings: {e}"))?;
    Ok(get_settings())
}

/// Saves the summary model, prompt template (blank or unchanged from the built-in one follows the built-in
/// one), display name, Embedding model (blank turns meaning search off) and Ask model (blank turns asking off). A
/// new Embedding model embeds every Meeting again, in the background.
#[tauri::command]
pub fn save_summary_settings(
    app: AppHandle,
    summary_model: String,
    summary_prompt: String,
    display_name: String,
    embedding_model: String,
    ask_model: String,
) -> Result<SettingsView, String> {
    let summary_model = summary_model.trim();
    if summary_model.is_empty() {
        return Err("Choose a summary model.".into());
    }
    let prompt = summary_prompt.trim();
    let summary_prompt = (!prompt.is_empty() && prompt != DEFAULT_PROMPT_TEMPLATE.trim()).then(|| summary_prompt.clone());
    let previous = settings::load();
    let embedding_model = embedding_model.trim().to_owned();
    let model_changed = embedding_model != previous.embedding_model;
    let settings = Settings {
        summary_model: summary_model.into(),
        summary_prompt,
        display_name: display_name.trim().into(),
        embedding_model,
        ask_model: ask_model.trim().into(),
        ..previous
    };
    settings::save(&settings).map_err(|e| format!("could not save settings: {e}"))?;
    if model_changed {
        embedding::reload_in_background(app);
    }
    Ok(get_settings())
}

/// Installed font families for the font pickers.
#[tauri::command]
pub fn list_fonts() -> Vec<String> {
    settings::installed_fonts()
}

/// Saves the fonts and sizes of the app's text and the Notes editor, and the theme; a blank font restores the default
/// one.
#[tauri::command]
pub fn save_appearance(
    text_font: String,
    text_size: u32,
    notes_font: String,
    notes_size: u32,
    theme: Theme,
) -> Result<SettingsView, String> {
    for size in [text_size, notes_size] {
        if !settings::FONT_SIZES.contains(&size) {
            return Err(format!(
                "Choose a font size from {} to {} px.",
                settings::FONT_SIZES.start(),
                settings::FONT_SIZES.end()
            ));
        }
    }
    let font = |name: &str, default: &str| if name.trim().is_empty() { default.to_string() } else { name.trim().to_string() };
    let settings = Settings {
        text_font: font(&text_font, settings::DEFAULT_TEXT_FONT),
        text_size,
        notes_font: font(&notes_font, settings::DEFAULT_NOTES_FONT),
        notes_size,
        theme,
        ..settings::load()
    };
    settings::save(&settings).map_err(|e| format!("could not save settings: {e}"))?;
    Ok(get_settings())
}

/// Turns Meeting screenshots on or off. The Settings view asks for consent before turning them on.
#[tauri::command]
pub fn set_meeting_screenshots(enabled: bool) -> Result<SettingsView, String> {
    settings::save(&Settings { meeting_screenshots: enabled, ..settings::load() })
        .map_err(|e| format!("could not save settings: {e}"))?;
    Ok(get_settings())
}

/// The running version, any newer release, and the command to install it by hand if the privilege prompt could not.
#[tauri::command]
pub fn update_status(app: AppHandle) -> UpdateStatus {
    updates::status(&app)
}

/// Turns the update check on (checking now) or off (no more requests to GitHub).
#[tauri::command]
pub fn set_check_for_updates(app: AppHandle, enabled: bool) -> Result<SettingsView, String> {
    updates::set_enabled(&app, enabled).map_err(|e| format!("could not save settings: {e}"))?;
    Ok(get_settings())
}

/// Downloads the newer release and offers to install it, as the tray's "Update to …" does.
#[tauri::command]
pub fn install_update(app: AppHandle) {
    updates::install(&app);
}

/// Writes or removes `~/.config/autostart/tiro.desktop`.
#[tauri::command]
pub fn set_autostart(enabled: bool) -> Result<SettingsView, String> {
    let path = default_autostart_path();
    autostart::set_enabled(&path, enabled).map_err(|e| format!("could not update {}: {e}", path.display()))?;
    Ok(get_settings())
}

/// The Meeting's last Summary, if it has one.
#[tauri::command]
pub fn meeting_summary(state: State<'_, AppState>, meeting_id: String) -> Result<Option<Summary>, String> {
    let meeting = meeting(&state, &meeting_id)?;
    state.store.summary(&meeting).map_err(|e| e.to_string())
}

/// What the Meeting has cost, from the spend ledger.
#[tauri::command]
pub fn meeting_spend(state: State<'_, AppState>, meeting_id: String) -> MeetingSpend {
    spend::meeting_spend(&state.spend.entries(), &meeting_id)
}

/// Spend per month in local time and to date, deleted Meetings included.
#[tauri::command]
pub fn spend_summary(state: State<'_, AppState>) -> SpendSummary {
    spend::spend_summary(&state.spend.entries(), &chrono::Local)
}

/// Re-summarize.
#[tauri::command]
pub async fn summarize(app: AppHandle, meeting_id: String) -> Result<Summary, String> {
    jobs::resummarize(&app, &meeting_id).await
}

#[tauri::command]
pub async fn retry_failed_chunks(app: AppHandle, meeting_id: String) -> Result<RetryReport, String> {
    jobs::retry_failed(&app, &meeting_id).await
}

#[tauri::command]
pub fn set_title(state: State<'_, AppState>, meeting_id: String, title: String) -> Result<Meeting, String> {
    state.store.set_title(&meeting_id, &title).map_err(|e| e.to_string())?;
    meeting(&state, &meeting_id)
}

/// Deletes the Meeting's folder. Refused while it is recording, transcribing, or being retried or summarized.
#[tauri::command]
pub fn delete_meeting(state: State<'_, AppState>, meeting_id: String) -> Result<(), String> {
    // Held throughout, so no retry or summary can start on the Meeting while it is deleted.
    let jobs = state.jobs.lock().unwrap();
    let recording = state.active.lock().unwrap().as_ref().is_some_and(|a| a.meeting.id == meeting_id);
    let status = meeting(&state, &meeting_id)?.status;
    if recording || matches!(status, MeetingStatus::Recording | MeetingStatus::Transcribing) {
        return Err("Wait until the Meeting has stopped and its Transcript is finished.".into());
    }
    if jobs.contains(&meeting_id) {
        return Err("Wait until the Meeting has finished being retried or summarized.".into());
    }
    state.store.delete(&meeting_id).map_err(|e| format!("could not delete the Meeting: {e}"))?;
    state.meaning.remove(&meeting_id);
    Ok(())
}

/// The Meeting's Notes (`notes.md`); empty when there are none.
#[tauri::command]
pub fn notes(state: State<'_, AppState>, meeting_id: String) -> Result<String, String> {
    let meeting = meeting(&state, &meeting_id)?;
    state.store.notes(&meeting).map_err(|e| e.to_string())
}

/// Autosave for the Notes editor.
#[tauri::command]
pub fn save_notes(state: State<'_, AppState>, meeting_id: String, notes: String) -> Result<(), String> {
    let meeting = meeting(&state, &meeting_id)?;
    state.store.write_notes(&meeting, &notes).map_err(|e| format!("could not save Notes: {e}"))
}

/// Indexes the Meeting's Passages again; the Notes editor asks once editing stops or the Meeting is closed, not on
/// every autosave.
#[tauri::command]
pub fn index_meeting(state: State<'_, AppState>, meeting_id: String) -> Result<(), String> {
    state.store.index_passages(&meeting_id).map_err(|e| format!("could not index the Meeting for Search: {e}"))
}

/// What the search box finds, by keywords and, when there is a key and an Embedding model, by meaning. Malformed
/// input finds less, never fails.
#[tauri::command]
pub async fn search(state: State<'_, AppState>, query: String) -> Result<SearchResults, String> {
    let results = match embedding::embedder() {
        Some(embedder) => {
            let meaning = Meaning { index: &state.meaning, embedder: &embedder, spend: &state.spend };
            search::search_by_meaning(&state.store, &query, &meaning).await
        }
        None => search::search(&state.store, &query),
    };
    results.map_err(|e| format!("could not search: {e}"))
}

/// Why asking questions of the Meetings is off, or `None` when it is on.
#[tauri::command]
pub fn ask_off() -> Option<String> {
    asking::answerer().err()
}

/// Answers a question as the next turn of the conversation, streaming the Passages it may cite and then the answer
/// so far to `on_event`.
#[tauri::command]
pub async fn ask(state: State<'_, AppState>, question: String, on_event: Channel<AskEvent>) -> Result<Answer, String> {
    asking::ask(&state, &question, &on_event).await
}

/// Forgets the conversation, so the next question starts a new one.
#[tauri::command]
pub fn new_conversation(state: State<'_, AppState>) {
    state.conversation.clear();
}

/// How far the startup indexing has got; done equals total when it has finished.
#[tauri::command]
pub fn indexing_progress(state: State<'_, AppState>) -> IndexProgress {
    *state.indexing.lock().unwrap()
}

/// How far embedding Meetings for meaning search has got, and why it stopped if it did.
#[tauri::command]
pub fn embedding_progress(state: State<'_, AppState>) -> EmbeddingProgress {
    state.embedding.progress()
}

/// The UI has saved the Notes that were pending when `notes::flush` asked.
#[tauri::command]
pub fn notes_flushed(state: State<'_, AppState>, request: u64) {
    state.acks.ack(request);
}

/// Where the export's Save dialog starts: `<title>.md` in the last export's folder, or in Downloads.
#[tauri::command]
pub fn export_default_path(state: State<'_, AppState>, meeting_id: String) -> Result<String, String> {
    let title = meeting(&state, &meeting_id)?.title;
    let last_dir = settings::load().last_export_dir;
    Ok(export::default_path(&title, last_dir.as_deref(), &settings::downloads_dir()).display().to_string())
}

/// Renders the Markdown export, writes it to `path` (chosen in the Save dialog) and remembers its folder; returns the path.
/// `screenshots` is the choice offered when the Meeting has Screenshots, remembered for next time; `None` when
/// none was offered, which exports without them. `summary_only` leaves out the Transcript and its Screenshots.
#[tauri::command]
pub fn export_meeting(
    state: State<'_, AppState>,
    meeting_id: String,
    path: String,
    screenshots: Option<ScreenshotsMode>,
    summary_only: bool,
) -> Result<String, String> {
    let mut settings = settings::load();
    let path = export::markdown_path(&PathBuf::from(path));
    let mode = screenshots.unwrap_or(ScreenshotsMode::None);
    if summary_only {
        export::export_summary(&state.store, &meeting_id, &settings.display_name, &path)
    } else {
        export::export_meeting(&state.store, &meeting_id, &settings.display_name, mode, &path)
    }
    .map_err(|e| format!("could not export to {}: {e}", path.display()))?;
    settings.remember_export(&path);
    if let Some(mode) = screenshots {
        settings.export_screenshots = mode;
    }
    if let Err(e) = settings::save(&settings) {
        tracing::warn!("could not remember the export folder: {e}");
    }
    Ok(path.display().to_string())
}

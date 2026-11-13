//! Starting and stopping a manual recording: key check, Meeting folder, capture processes and the pipeline.

use std::collections::HashSet;
use std::io::Read;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::Local;
use tiro_core::acks::Acks;
use tiro_core::detection::MeetingApp;
use tiro_core::embeddings::MeaningIndex;
use tiro_core::keyfile::{default_key_path, read_key};
use tiro_core::openrouter::{OpenRouterTranscriber, DEFAULT_BASE_URL};
use tiro_core::pipeline::{Pipeline, PipelineConfig, PipelineEvent};
use tiro_core::search::IndexProgress;
use tiro_core::spend::SpendLedger;
use tiro_core::store::{Meeting, MeetingStatus, MeetingStore};
use tiro_core::transcript::{self, Track};
use tauri::{AppHandle, Emitter, Manager};

use crate::embedding::EmbeddingQueue;
use crate::screenshots::Capture;
use crate::{capture, jobs, settings, tray};

/// The Meeting being recorded right now.
pub struct ActiveMeeting {
    pub meeting: Meeting,
    capture: Vec<Child>,
    pipeline: Pipeline,
    /// Running only when Meeting screenshots are turned on in Settings.
    screenshots: Option<Capture>,
}

pub struct AppState {
    pub store: Arc<MeetingStore>,
    /// Every OpenRouter request's cost (`spend.jsonl`).
    pub spend: Arc<SpendLedger>,
    pub active: Mutex<Option<ActiveMeeting>>,
    /// Meetings with a retry or summary running (see `jobs`).
    pub jobs: Mutex<HashSet<String>>,
    /// Requests waiting on the UI (see `notes::flush`).
    pub acks: Acks,
    /// How far the startup indexing for Search has got (see `search`).
    pub indexing: Mutex<IndexProgress>,
    /// Every embedded Passage, for meaning search (see `embedding`).
    pub meaning: Arc<MeaningIndex>,
    /// The Meetings waiting to be embedded.
    pub embedding: EmbeddingQueue,
    /// The Ask conversation, until a new one is started or Tiro quits.
    pub conversation: crate::asking::AskConversation,
}

pub const PIPELINE_EVENT: &str = "pipeline";
pub const ACTIVE_MEETING_EVENT: &str = "active-meeting";

/// The app name in the default title of a Meeting recorded without a Detection prompt.
const MANUAL: &str = "Manual";

/// Must run on the async runtime: the pipeline spawns tokio tasks. `detected` is the meeting app a Detection
/// prompt was accepted for, `None` for a manual start; its name goes into the default Meeting title.
pub fn start(app: &AppHandle, state: &AppState, detected: Option<MeetingApp>) -> Result<Meeting, String> {
    let mut active = state.active.lock().unwrap();
    if let Some(current) = active.as_ref() {
        return Ok(current.meeting.clone());
    }
    let key = read_key(&default_key_path()).map_err(|e| e.to_string())?;
    let settings = settings::load();

    // Echo-cancel helps without headphones; if it is unavailable the You Track records the plain mic.
    let echo_cancel = capture::start_echo_cancel(settings.mic.as_deref());
    let mic = echo_cancel.as_ref().map_or(settings.mic.as_deref(), |ec| Some(ec.source.as_str()));
    let mut capture = Vec::new();
    let mut inputs: Vec<(Track, Box<dyn Read + Send>)> = Vec::new();
    for track in Track::ALL {
        match capture::spawn(track, mic) {
            Ok((child, stdout)) => {
                capture.push(child);
                inputs.push((track, Box::new(stdout)));
            }
            Err(e) => {
                capture.extend(echo_cancel.map(|ec| ec.process));
                kill_all(&mut capture);
                return Err(e.to_string());
            }
        }
    }
    // Last, so stopping kills pw-record before its echo-cancelled source goes away.
    capture.extend(echo_cancel.map(|ec| ec.process));
    let meeting = match state.store.create(detected.map_or(MANUAL, MeetingApp::name), Local::now().fixed_offset()) {
        Ok(meeting) => meeting,
        Err(e) => {
            kill_all(&mut capture);
            return Err(format!("could not create the Meeting folder: {e}"));
        }
    };

    let transcriber = Arc::new(OpenRouterTranscriber::new(DEFAULT_BASE_URL.into(), key, settings.stt_model));
    let handle = app.clone();
    // The Meeting's time zero for screenshots: Segment times count from when the pipeline starts reading audio.
    let zero = Instant::now();
    let pipeline = Pipeline::start(
        state.store.clone(),
        meeting.clone(),
        transcriber,
        state.spend.clone(),
        PipelineConfig { sample_rate: capture::SAMPLE_RATE, ..PipelineConfig::default() },
        inputs,
        move |event: PipelineEvent| on_pipeline_event(&handle, event),
    );
    let screenshots = settings
        .meeting_screenshots
        .then(|| Capture::start(app.clone(), meeting.id.clone(), &meeting.dir, detected, zero));
    *active = Some(ActiveMeeting { meeting: meeting.clone(), capture, pipeline, screenshots });
    drop(active);

    tray::set_recording(app, true);
    let _ = app.emit(ACTIVE_MEETING_EVENT, Some(&meeting));
    Ok(meeting)
}

/// Stops capture. Transcription of the remaining Chunks carries on until the pipeline reports `Finished`.
pub fn stop(app: &AppHandle, state: &AppState) {
    // Marked transcribing while capture still runs: once it ends, a pipeline with nothing left to transcribe can
    // finish at once, and its final status must not be overwritten.
    let Some(id) = state.active.lock().unwrap().as_ref().map(|a| a.meeting.id.clone()) else { return };
    let _ = state.store.set_status(&id, MeetingStatus::Transcribing, Some(Local::now().fixed_offset()));
    let Some(active) = end_capture(app, state) else { return };
    // Joining waits for the capture threads to queue their last Chunks; keep that off the UI thread.
    std::thread::spawn(move || active.pipeline.join());
}

/// Kills the capture processes and returns the tray and UI to idle.
fn end_capture(app: &AppHandle, state: &AppState) -> Option<ActiveMeeting> {
    let mut active = state.active.lock().unwrap().take()?;
    kill_all(&mut active.capture);
    if let Some(screenshots) = &active.screenshots {
        screenshots.stop();
    }
    tray::set_recording(app, false);
    let _ = app.emit(ACTIVE_MEETING_EVENT, None::<Meeting>);
    Some(active)
}

fn on_pipeline_event(app: &AppHandle, mut event: PipelineEvent) {
    match &mut event {
        // The live Transcript is shown without fillers and audio-event tags, like the stored one.
        PipelineEvent::Segments { segments, .. } => {
            *segments = transcript::cleaned(segments);
            if segments.is_empty() {
                return;
            }
        }
        PipelineEvent::Error { message, .. } => tracing::warn!("{message}"),
        PipelineEvent::Finished { meeting_id } => {
            // Finishing while still marked active means capture ended without Stop (pw-record died). The
            // pipeline has already written the final status, so only capture and the UI need resetting.
            let state = app.state::<AppState>();
            let still_active = state.active.lock().unwrap().as_ref().is_some_and(|a| &a.meeting.id == meeting_id);
            if still_active {
                end_capture(app, &state);
                let _ = app.emit(
                    PIPELINE_EVENT,
                    PipelineEvent::Error { meeting_id: meeting_id.clone(), message: "Audio capture stopped unexpectedly.".into() },
                );
            }
            // The Transcript is final: retry anything left over, then summarize.
            tauri::async_runtime::spawn(jobs::after_recording(app.clone(), meeting_id.clone()));
        }
        _ => {}
    }
    let _ = app.emit(PIPELINE_EVENT, event);
}

fn kill_all(capture: &mut Vec<Child>) {
    for child in capture.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    capture.clear();
}

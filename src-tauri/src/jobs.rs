//! Post-recording jobs on a Meeting: after it stops, retrying leftover Chunks, then the Summary (ADR-0006) and its
//! Tags. Also the same jobs on demand ("Retry failed Chunks", "Re-summarize"), and tagging, at startup, the Meetings
//! summarized but never tagged.

use tiro_core::keyfile::{default_key_path, read_key};
use tiro_core::openrouter::{OpenRouterSummarizer, OpenRouterTranscriber, DEFAULT_BASE_URL};
use tiro_core::pipeline::PipelineConfig;
use tiro_core::retry::{retry_failed_chunks, RetryReport};
use tiro_core::store::MeetingStatus;
use tiro_core::summary::{summarize_meeting, Summary};
use tiro_core::tags::{clear_outdated_tags, tag_meeting, TagError};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::recorder::AppState;
use crate::{capture, notes, settings};

pub const JOB_EVENT: &str = "meeting-job";

/// Progress of a post-recording job, for the UI. Matches `JobEvent` in src/lib/api.ts.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobEvent {
    Retrying { meeting_id: String },
    Retried { meeting_id: String, report: RetryReport },
    Summarizing { meeting_id: String },
    /// `title` is the Meeting's title now, which the Summary may have set.
    Summarized { meeting_id: String, summary: Summary, title: String },
    Tagged { meeting_id: String, tags: Vec<String> },
    Failed { meeting_id: String, message: String },
}

/// Marks a Meeting busy while a retry or summary runs, so they never overlap.
struct Job {
    app: AppHandle,
    meeting_id: String,
}

impl Job {
    fn claim(app: &AppHandle, meeting_id: &str) -> Result<Self, String> {
        let state = app.state::<AppState>();
        if !state.jobs.lock().unwrap().insert(meeting_id.to_owned()) {
            return Err("This Meeting is already being retried or summarized.".into());
        }
        Ok(Self { app: app.clone(), meeting_id: meeting_id.to_owned() })
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.app.state::<AppState>().jobs.lock().unwrap().remove(&self.meeting_id);
    }
}

/// Runs once the pipeline has finished with a stopped Meeting: one more try at any Chunks left on disk, then
/// the Summary.
pub async fn after_recording(app: AppHandle, meeting_id: String) {
    let Ok(_job) = Job::claim(&app, &meeting_id) else { return };
    let incomplete = app.state::<AppState>().store.get(&meeting_id).is_ok_and(|m| m.status == MeetingStatus::Incomplete);
    if incomplete {
        let _ = retry(&app, &meeting_id).await;
    }
    let _ = summarize(&app, &meeting_id).await;
}

/// Tags, one at a time and oldest first, the Meetings with a Summary and no Tags, so each Meeting's new Tags can be
/// reused by the next. When the tagging rules have changed, first clears every Meeting's Tags so all are tagged again. Stops at the first failed request: the rest wait for the next startup.
pub fn tag_in_background(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let Ok(key) = read_key(&default_key_path()) else { return };
        let store = app.state::<AppState>().store.clone();
        match clear_outdated_tags(&store, &settings::data_dir()) {
            Ok(true) => tracing::info!("cleared every Meeting's Tags to tag them again under the current rules"),
            Ok(false) => {}
            Err(e) => tracing::warn!("could not clear outdated Tags: {e}"),
        }
        let meetings = match store.list() {
            Ok(meetings) => meetings,
            Err(e) => return tracing::warn!("could not list Meetings to tag them: {e}"),
        };
        let spend = app.state::<AppState>().spend.clone();
        let summarizer = OpenRouterSummarizer::new(DEFAULT_BASE_URL.into(), key, settings::load().summary_model);
        for meeting in meetings.into_iter().rev().filter(|m| m.tags.is_none()) {
            // A Meeting being summarized gets its Tags from that job.
            let Ok(_job) = Job::claim(&app, &meeting.id) else { continue };
            match tag_meeting(&store, &meeting.id, &summarizer, &spend).await {
                Ok(tags) => {
                    let _ = app.emit(JOB_EVENT, JobEvent::Tagged { meeting_id: meeting.id, tags });
                }
                Err(TagError::Request(e)) => return tracing::warn!("stopped tagging Meetings: {e}"),
                Err(TagError::NoSummary) => {}
                Err(e) => tracing::warn!("could not tag {}: {e}", meeting.id),
            }
        }
    });
}

/// "Re-summarize".
pub async fn resummarize(app: &AppHandle, meeting_id: &str) -> Result<Summary, String> {
    let _job = Job::claim(app, meeting_id)?;
    summarize(app, meeting_id).await
}

/// "Retry failed Chunks" on a stopped Meeting.
pub async fn retry_failed(app: &AppHandle, meeting_id: &str) -> Result<RetryReport, String> {
    let state = app.state::<AppState>();
    let meeting = state.store.get(meeting_id).map_err(|e| e.to_string())?;
    if matches!(meeting.status, MeetingStatus::Recording | MeetingStatus::Transcribing) {
        return Err("Wait until the Meeting has stopped and its Transcript is finished.".into());
    }
    let _job = Job::claim(app, meeting_id)?;
    retry(app, meeting_id).await
}

async fn summarize(app: &AppHandle, meeting_id: &str) -> Result<Summary, String> {
    let id = meeting_id.to_owned();
    let _ = app.emit(JOB_EVENT, JobEvent::Summarizing { meeting_id: id.clone() });
    // Notes typed just before Stop may still be waiting on autosave; the Summary must see them.
    notes::flush(app).await;
    let settings = settings::load();
    let store = app.state::<AppState>().store.clone();
    let spend = app.state::<AppState>().spend.clone();
    let summarizer = read_key(&default_key_path())
        .map(|key| OpenRouterSummarizer::new(DEFAULT_BASE_URL.into(), key, settings.summary_model.clone()));
    let result = match &summarizer {
        Ok(summarizer) => {
            summarize_meeting(&store, meeting_id, summarizer, &settings.summary(), &spend).await.map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    };
    let event = match &result {
        Ok(summary) => {
            let title = store.get(meeting_id).map(|m| m.title).unwrap_or_default();
            JobEvent::Summarized { meeting_id: id.clone(), summary: summary.clone(), title }
        }
        Err(message) => {
            tracing::warn!("summary failed: {message}");
            JobEvent::Failed { meeting_id: id.clone(), message: message.clone() }
        }
    };
    let _ = app.emit(JOB_EVENT, event);
    // The Summary stands without Tags: a Meeting left untagged is tagged at the next startup.
    if let (Ok(_), Ok(summarizer)) = (&result, &summarizer) {
        match tag_meeting(&store, meeting_id, summarizer, &spend).await {
            Ok(tags) => {
                let _ = app.emit(JOB_EVENT, JobEvent::Tagged { meeting_id: id, tags });
            }
            Err(e) => tracing::warn!("could not tag {meeting_id}: {e}"),
        }
    }
    result
}

async fn retry(app: &AppHandle, meeting_id: &str) -> Result<RetryReport, String> {
    let id = meeting_id.to_owned();
    let key = read_key(&default_key_path()).map_err(|e| e.to_string())?;
    let _ = app.emit(JOB_EVENT, JobEvent::Retrying { meeting_id: id.clone() });
    let transcriber = OpenRouterTranscriber::new(DEFAULT_BASE_URL.into(), key, settings::load().stt_model);
    let store = app.state::<AppState>().store.clone();
    let config = PipelineConfig { sample_rate: capture::SAMPLE_RATE, ..PipelineConfig::default() };
    let spend = app.state::<AppState>().spend.clone();
    let report = retry_failed_chunks(&store, meeting_id, &transcriber, &config, &spend).await;
    let _ = app.emit(JOB_EVENT, JobEvent::Retried { meeting_id: id, report: report.clone() });
    Ok(report)
}

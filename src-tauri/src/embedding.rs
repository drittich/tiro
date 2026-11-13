//! Embedding Meetings' Passages for meaning search in the background: each Meeting once its Passages are indexed,
//! and at startup or when the Embedding model changes, every Meeting whose stored vectors don't cover it.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tiro_core::embeddings::{embed_meeting, load_embedded, EmbedError};
use tiro_core::keyfile::{default_key_path, read_key};
use tiro_core::openrouter::{OpenRouterEmbedder, DEFAULT_BASE_URL};

use crate::recorder::AppState;
use crate::settings;

/// Carries an `EmbeddingProgress`. Matches `EVENTS.embedding` in src/lib/api.ts.
pub const EMBEDDING_EVENT: &str = "embedding";

/// How far embedding the queued Meetings has got. Matches `EmbeddingProgress` in src/lib/api.ts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingProgress {
    pub done: usize,
    pub total: usize,
    /// Why embedding stopped, until a Meeting is embedded again.
    pub problem: Option<String>,
}

/// The Meetings waiting to be embedded, one at a time, oldest first as queued.
#[derive(Default)]
pub struct EmbeddingQueue {
    state: Mutex<Queue>,
    wake: tokio::sync::Notify,
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<String>,
    /// The Meeting being embedded now. Not in `pending`, so a change to it meanwhile queues it again.
    current: Option<String>,
    /// Embedded since the queue was last empty.
    done: usize,
    problem: Option<String>,
}

impl EmbeddingQueue {
    pub fn progress(&self) -> EmbeddingProgress {
        let queue = self.state.lock().unwrap();
        let total = queue.done + queue.pending.len() + usize::from(queue.current.is_some());
        EmbeddingProgress { done: queue.done, total, problem: queue.problem.clone() }
    }
}

/// The Embedder for the key and the Embedding model in Settings; `None` when meaning search is off.
pub fn embedder() -> Option<OpenRouterEmbedder> {
    let model = settings::load().embedding_model.trim().to_owned();
    if model.is_empty() {
        return None;
    }
    let key = read_key(&default_key_path()).ok()?;
    Some(OpenRouterEmbedder::new(DEFAULT_BASE_URL.into(), key, model))
}

/// Queues these Meetings for embedding, after any already waiting.
pub fn enqueue(app: &AppHandle, ids: impl IntoIterator<Item = String>) {
    let state = app.state::<AppState>();
    {
        let mut queue = state.embedding.state.lock().unwrap();
        for id in ids {
            if !queue.pending.contains(&id) {
                queue.pending.push_back(id);
            }
        }
    }
    let _ = app.emit(EMBEDDING_EVENT, state.embedding.progress());
    state.embedding.wake.notify_one();
}

/// Embeds the queued Meetings for as long as the app runs. A failed request stops it and empties the queue, like
/// tagging: the Meetings left are queued again at the next startup, or when they are next indexed. A Meeting
/// whose files can't be read, as when it was deleted while queued, is skipped.
pub fn spawn_worker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        loop {
            let next = {
                let mut queue = state.embedding.state.lock().unwrap();
                queue.current = queue.pending.pop_front();
                queue.current.clone()
            };
            let Some(id) = next else {
                state.embedding.state.lock().unwrap().done = 0;
                let _ = app.emit(EMBEDDING_EVENT, state.embedding.progress());
                state.embedding.wake.notified().await;
                continue;
            };
            let result = match embedder() {
                Some(embedder) => embed_meeting(&state.store, &state.meaning, &id, &embedder, &state.spend).await,
                // Meaning search is off: nothing to do until it is turned on, which queues every Meeting.
                None => {
                    let mut queue = state.embedding.state.lock().unwrap();
                    queue.pending.clear();
                    queue.current = None;
                    continue;
                }
            };
            {
                let mut queue = state.embedding.state.lock().unwrap();
                queue.current = None;
                match result {
                    Ok(()) => {
                        queue.done += 1;
                        queue.problem = None;
                    }
                    Err(EmbedError::Meeting(e)) => tracing::warn!("could not embed {id}: {e}"),
                    Err(e) => {
                        tracing::warn!("stopped embedding Meetings for meaning search: {e}");
                        queue.pending.clear();
                        queue.problem = Some(e.to_string());
                    }
                }
            }
            // Deleted while it was being embedded: its vectors may have gone in after the delete removed them.
            if state.store.get(&id).is_err() {
                state.meaning.remove(&id);
            }
            let _ = app.emit(EMBEDDING_EVENT, state.embedding.progress());
        }
    });
}

/// Loads every stopped Meeting's stored Embeddings for the Embedding model, oldest first, and queues the Meetings
/// they don't cover. Call once the startup indexing is done, and again when the Embedding model or the key
/// changes. Blocks while it reads the Meeting folders.
pub fn load_all(app: &AppHandle) {
    let Some(embedder) = embedder() else { return };
    let state = app.state::<AppState>();
    let meetings = match state.store.list() {
        Ok(meetings) => meetings,
        Err(e) => return tracing::warn!("could not list Meetings to embed them: {e}"),
    };
    let model = tiro_core::embeddings::Embedder::model(&embedder);
    let mut stale = Vec::new();
    for meeting in meetings.into_iter().rev() {
        match load_embedded(&state.store, &state.meaning, &meeting.id, model) {
            Ok(true) => {}
            Ok(false) => stale.push(meeting.id),
            Err(e) => tracing::warn!("could not load the embeddings of {}: {e}", meeting.id),
        }
    }
    enqueue(app, stale);
}

/// [`load_all`] on a thread of its own, after forgetting every vector in memory, as when the Embedding model
/// changes.
pub fn reload_in_background(app: AppHandle) {
    app.state::<AppState>().meaning.clear();
    let spawned = std::thread::Builder::new().name("load-embeddings".into()).spawn(move || load_all(&app));
    if let Err(e) = spawned {
        tracing::warn!("could not start loading embeddings: {e}");
    }
}

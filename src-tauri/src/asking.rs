//! Ask: the conversation in memory, and asking the Ask model with the key and the model in Settings.

use std::sync::Mutex;

use tauri::ipc::Channel;
use tiro_core::ask::{self, Answer, AskEvent, Asking, Conversation};
use tiro_core::keyfile::{default_key_path, read_key};
use tiro_core::openrouter::{OpenRouterAnswerer, DEFAULT_BASE_URL};
use tiro_core::search::Meaning;

use crate::embedding;
use crate::recorder::AppState;
use crate::settings;

/// The Ask conversation, kept in memory only.
#[derive(Default)]
pub struct AskConversation(Mutex<Versioned>);

/// The conversation with a count of its changes, so an answer that comes back after a new conversation was started,
/// or after another answer, isn't added to it.
#[derive(Clone, Default)]
struct Versioned {
    version: u64,
    conversation: Conversation,
}

impl AskConversation {
    pub fn clear(&self) {
        let mut current = self.0.lock().unwrap();
        *current = Versioned { version: current.version + 1, conversation: Conversation::default() };
    }

    fn snapshot(&self) -> Versioned {
        self.0.lock().unwrap().clone()
    }

    fn replace(&self, version: u64, conversation: Conversation) {
        let mut current = self.0.lock().unwrap();
        if current.version == version {
            *current = Versioned { version: version + 1, conversation };
        }
    }
}

/// The Answerer for the key and the Ask model in Settings, or why asking is off.
pub fn answerer() -> Result<OpenRouterAnswerer, String> {
    let model = settings::load().ask_model.trim().to_owned();
    let key = read_key(&default_key_path()).map_err(|_| "Asking is off: set an OpenRouter API key in Settings.".to_owned())?;
    if model.is_empty() {
        return Err("Asking is off: choose an Ask model in Settings.".into());
    }
    Ok(OpenRouterAnswerer::new(DEFAULT_BASE_URL.into(), key, model))
}

/// Answers `question` as the next turn of the conversation, by meaning too when meaning search is on.
pub async fn ask(state: &AppState, question: &str, on_event: &Channel<AskEvent>) -> Result<Answer, String> {
    let answerer = answerer()?;
    let embedder = embedding::embedder();
    let meaning = embedder.as_ref().map(|embedder| Meaning { index: &state.meaning, embedder, spend: &state.spend });
    let asking = Asking {
        store: &state.store,
        meaning,
        answerer: &answerer,
        spend: &state.spend,
        today: chrono::Local::now().date_naive(),
    };
    let Versioned { version, mut conversation } = state.conversation.snapshot();
    let send = |event: AskEvent| {
        if let Err(e) = on_event.send(event) {
            tracing::warn!("could not stream the answer to the Ask view: {e}");
        }
    };
    let answer = ask::ask(&asking, &mut conversation, question, &send).await.map_err(|e| e.to_string())?;
    state.conversation.replace(version, conversation);
    Ok(answer)
}

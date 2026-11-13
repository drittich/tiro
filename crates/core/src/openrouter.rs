//! OpenRouter's `/audio/transcriptions` endpoint (ADR-0004), `/chat/completions` for the Summary (ADR-0006) and
//! `/embeddings` for meaning search, and a streamed `/chat/completions` for Ask.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::ask::{Answerer, AskError, Message};
use crate::embeddings::{EmbedError, Embedder};
use crate::spend::{Billed, Usage};
use crate::summary::{SummarizeError, Summarizer};
use crate::transcriber::{ChunkAudio, TranscribeError, Transcriber};
use crate::transcript::Word;

pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_STT_MODEL: &str = "elevenlabs/scribe-v2";

/// A client whose every request names Tiro as the app in OpenRouter's Logs. A title without a referer creates no
/// app entry, and visibility only counts on the request that creates it, so all three go on every request.
fn attributed_client(timeout: Duration) -> reqwest::Client {
    let mut headers = HeaderMap::new();
    headers.insert("X-OpenRouter-Title", HeaderValue::from_static("Tiro"));
    headers.insert("HTTP-Referer", HeaderValue::from_static("https://github.com/drittich/tiro"));
    headers.insert("X-OpenRouter-App-Visibility", HeaderValue::from_static("hidden"));
    reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .build()
        .expect("reqwest client")
}

pub struct OpenRouterTranscriber {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    text: String,
    words: Option<Vec<ResponseWord>>,
}

#[derive(Deserialize)]
struct ResponseWord {
    word: String,
    start: f64,
    end: f64,
}

enum Attempt {
    Ok(Response),
    BadRequest(String),
}

impl OpenRouterTranscriber {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self { client: attributed_client(Duration::from_secs(120)), base_url, api_key, model }
    }

    async fn post(&self, mut body: Value, verbose: bool) -> Billed<Attempt, TranscribeError> {
        if verbose {
            body["response_format"] = json!("verbose_json");
            body["timestamp_granularities"] = json!(["word"]);
        } else {
            body["response_format"] = json!("json");
        }
        let response =
            match self.client.post(format!("{}/audio/transcriptions", self.base_url)).bearer_auth(&self.api_key).json(&body).send().await {
                Ok(response) => response,
                Err(e) => return Err(TranscribeError::Transient(format!("could not reach OpenRouter: {e}"))).into(),
            };
        // The transcription endpoint names the generation only in this header.
        let generation_id = response.headers().get("x-generation-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let status = response.status().as_u16();
        let text = match response.text().await {
            Ok(text) => text,
            Err(e) => return Err(TranscribeError::Transient(format!("reading response: {e}"))).into(),
        };
        let usage = usage_of(&self.client, &self.base_url, &self.api_key, &self.model, status, &text, generation_id).await;
        Billed { result: self.attempt(status, &text), usage }
    }

    fn attempt(&self, status: u16, text: &str) -> Result<Attempt, TranscribeError> {
        if let Some(refusal) = refusal(status, text, &self.model, "STT") {
            return Err(refusal.into());
        }
        match status {
            200..=299 => serde_json::from_str(text)
                .map(Attempt::Ok)
                .map_err(|e| TranscribeError::Transient(format!("unexpected response from OpenRouter: {e}"))),
            400 => Ok(Attempt::BadRequest(error_message(text))),
            402 | 408 | 429 | 500..=599 => {
                Err(TranscribeError::Transient(format!("OpenRouter returned {status}: {}", error_message(text))))
            }
            _ => Err(TranscribeError::Rejected(format!("OpenRouter returned {status}: {}", error_message(text)))),
        }
    }
}

#[async_trait]
impl Transcriber for OpenRouterTranscriber {
    async fn transcribe(&self, audio: &ChunkAudio) -> Billed<Vec<Word>, TranscribeError> {
        let bytes = match tokio::fs::read(&audio.path).await {
            Ok(bytes) => bytes,
            Err(e) => return Err(TranscribeError::Rejected(format!("reading {}: {e}", audio.path.display()))).into(),
        };
        let format = match audio.path.extension().and_then(|e| e.to_str()) {
            Some("flac") => "flac",
            _ => "ogg",
        };
        let body = json!({
            "model": self.model,
            "language": "en",
            "input_audio": { "data": base64::engine::general_purpose::STANDARD.encode(bytes), "format": format },
        });

        // A rejected verbose_json reports no cost, so only the json retry's is kept.
        let verbose = self.post(body.clone(), true).await;
        match verbose.result {
            Ok(Attempt::Ok(response)) => return Billed { result: Ok(words_of(response, audio.duration)), usage: verbose.usage },
            Ok(Attempt::BadRequest(message)) => {
                tracing::info!("{} rejected verbose_json ({message}); falling back to json", self.model);
            }
            Err(e) => return Billed { result: Err(e), usage: verbose.usage },
        }
        let plain = self.post(body, false).await;
        let result = plain.result.and_then(|attempt| match attempt {
            Attempt::Ok(response) => Ok(words_of(response, audio.duration)),
            Attempt::BadRequest(message) => Err(TranscribeError::Rejected(message)),
        });
        Billed { result, usage: plain.usage }
    }
}

/// The provider's words, or, when it gave none, the text's words spread evenly over the Chunk.
fn words_of(response: Response, duration: f64) -> Vec<Word> {
    match response.words {
        Some(words) if !words.is_empty() || response.text.trim().is_empty() => words
            .into_iter()
            .filter(|w| !w.word.trim().is_empty())
            .map(|w| Word { text: w.word, start: w.start, end: w.end })
            .collect(),
        _ => {
            let tokens: Vec<&str> = response.text.split_whitespace().collect();
            let step = duration / tokens.len().max(1) as f64;
            tokens
                .iter()
                .enumerate()
                .map(|(i, t)| Word { text: (*t).to_owned(), start: i as f64 * step, end: (i + 1) as f64 * step })
                .collect()
        }
    }
}

/// The summary model reasons before it answers, and a long Meeting's Summary is long: a small budget returns
/// empty content (a 5-token test did).
pub const SUMMARY_MAX_TOKENS: u32 = 32_000;

pub struct OpenRouterSummarizer {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenRouterSummarizer {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        // A whole Meeting plus reasoning can take minutes.
        Self { client: attributed_client(Duration::from_secs(600)), base_url, api_key, model }
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[async_trait]
impl Summarizer for OpenRouterSummarizer {
    async fn summarize(&self, prompt: &str) -> Billed<String, SummarizeError> {
        let body = json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": SUMMARY_MAX_TOKENS,
        });
        let response =
            match self.client.post(format!("{}/chat/completions", self.base_url)).bearer_auth(&self.api_key).json(&body).send().await {
                Ok(response) => response,
                Err(e) => return Err(SummarizeError::Failed(format!("could not reach OpenRouter: {e}"))).into(),
            };
        let status = response.status().as_u16();
        let text = match response.text().await {
            Ok(text) => text,
            Err(e) => return Err(SummarizeError::Failed(format!("reading response: {e}"))).into(),
        };
        let usage = usage_of(&self.client, &self.base_url, &self.api_key, &self.model, status, &text, None).await;
        Billed { result: self.reply(status, &text), usage }
    }
}

impl OpenRouterSummarizer {
    fn reply(&self, status: u16, text: &str) -> Result<String, SummarizeError> {
        if let Some(refusal) = refusal(status, text, &self.model, "summary") {
            return Err(refusal.into());
        }
        if !(200..=299).contains(&status) {
            return Err(SummarizeError::Failed(format!("OpenRouter returned {status}: {}", error_message(text))));
        }
        let response: ChatResponse = serde_json::from_str(text)
            .map_err(|e| SummarizeError::Failed(format!("unexpected response from OpenRouter: {e}")))?;
        let choice = response.choices.into_iter().next();
        let finish = choice.as_ref().and_then(|c| c.finish_reason.clone()).unwrap_or_else(|| "none".into());
        match choice.and_then(|c| c.message.content).filter(|c| !c.trim().is_empty()) {
            Some(content) => Ok(content),
            None => Err(SummarizeError::Failed(format!("{} returned an empty Summary (finish reason: {finish})", self.model))),
        }
    }
}

pub struct OpenRouterEmbedder {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenRouterEmbedder {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        Self { client: attributed_client(Duration::from_secs(120)), base_url, api_key, model }
    }

    fn vectors(&self, status: u16, text: &str, count: usize) -> Result<Vec<Vec<f32>>, EmbedError> {
        if let Some(refusal) = refusal(status, text, &self.model, "Embedding") {
            return Err(refusal.into());
        }
        if !(200..=299).contains(&status) {
            return Err(EmbedError::Failed(format!("OpenRouter returned {status}: {}", error_message(text))));
        }
        let response: EmbeddingsResponse = serde_json::from_str(text)
            .map_err(|e| EmbedError::Failed(format!("unexpected response from OpenRouter: {e}")))?;
        let mut vectors: Vec<Option<Vec<f32>>> = vec![None; count];
        for item in response.data {
            if let Some(slot) = vectors.get_mut(item.index) {
                *slot = Some(item.embedding);
            }
        }
        vectors
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| EmbedError::Failed(format!("{} returned no vector for some of the {count} texts", self.model)))
    }
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
    index: usize,
}

#[async_trait]
impl Embedder for OpenRouterEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    async fn embed(&self, texts: &[String]) -> Billed<Vec<Vec<f32>>, EmbedError> {
        let body = json!({ "model": self.model, "input": texts });
        let response =
            match self.client.post(format!("{}/embeddings", self.base_url)).bearer_auth(&self.api_key).json(&body).send().await {
                Ok(response) => response,
                Err(e) => return Err(EmbedError::Failed(format!("could not reach OpenRouter: {e}"))).into(),
            };
        let status = response.status().as_u16();
        let text = match response.text().await {
            Ok(text) => text,
            Err(e) => return Err(EmbedError::Failed(format!("reading response: {e}"))).into(),
        };
        let usage = usage_of(&self.client, &self.base_url, &self.api_key, &self.model, status, &text, None).await;
        Billed { result: self.vectors(status, &text, texts.len()), usage }
    }
}

/// An answer seldom needs more, but the Ask model may reason before it writes.
pub const ASK_MAX_TOKENS: u32 = 8_000;

pub struct OpenRouterAnswerer {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenRouterAnswerer {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        // The whole stream, reasoning included, must finish within this.
        Self { client: attributed_client(Duration::from_secs(300)), base_url, api_key, model }
    }
}

/// One `data:` event of a streamed chat completion.
#[derive(Deserialize)]
struct StreamEvent {
    id: Option<String>,
    #[serde(default)]
    choices: Vec<StreamChoice>,
    usage: Option<Value>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
}

#[derive(Default, Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

#[async_trait]
impl Answerer for OpenRouterAnswerer {
    async fn answer(&self, messages: &[Message], on_text: &(dyn for<'s> Fn(&'s str) + Send + Sync)) -> Billed<String, AskError> {
        let body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "max_tokens": ASK_MAX_TOKENS,
            "usage": { "include": true },
        });
        let mut response =
            match self.client.post(format!("{}/chat/completions", self.base_url)).bearer_auth(&self.api_key).json(&body).send().await {
                Ok(response) => response,
                Err(e) => return Err(AskError::Failed(format!("could not reach OpenRouter: {e}"))).into(),
            };
        let status = response.status().as_u16();
        if !(200..=299).contains(&status) {
            let text = response.text().await.unwrap_or_default();
            if let Some(refusal) = refusal(status, &text, &self.model, "Ask") {
                return Err(refusal.into()).into();
            }
            return Err(AskError::Failed(format!("OpenRouter returned {status}: {}", error_message(&text)))).into();
        }

        // Server-sent events: `data: <json>` lines, `: comment` lines to keep the connection open, `data: [DONE]` last.
        let (mut answer, mut pending, mut generation_id, mut usage) = (String::new(), Vec::<u8>::new(), None, None);
        let mut failure = None;
        'read: loop {
            let bytes = match response.chunk().await {
                Ok(Some(bytes)) => bytes,
                Ok(None) => break,
                Err(e) => {
                    failure = Some(AskError::Failed(format!("the answer was cut off: {e}")));
                    break;
                }
            };
            pending.extend_from_slice(&bytes);
            while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim_end().strip_prefix("data:").map(str::trim) else { continue };
                if data == "[DONE]" {
                    break 'read;
                }
                let Ok(event) = serde_json::from_str::<StreamEvent>(data) else { continue };
                generation_id = event.id.or(generation_id);
                usage = event.usage.or(usage);
                if let Some(error) = event.error {
                    let message = error["message"].as_str().unwrap_or("unknown error").to_owned();
                    failure = Some(AskError::Failed(format!("OpenRouter stopped the answer: {message}")));
                    break 'read;
                }
                if let Some(text) = event.choices.into_iter().next().and_then(|c| c.delta.content).filter(|t| !t.is_empty()) {
                    answer.push_str(&text);
                    on_text(&text);
                }
            }
        }
        // The cost comes with the last event, as it does with a whole answer.
        let charge = json!({ "id": generation_id, "usage": usage }).to_string();
        let usage = usage_of(&self.client, &self.base_url, &self.api_key, &self.model, status, &charge, None).await;
        Billed { result: failure.map_or(Ok(answer), Err), usage }
    }
}

/// The parts of an answer that say what it cost. Chat completions name the generation in `id`.
#[derive(Deserialize)]
struct Charge {
    id: Option<String>,
    usage: Option<ChargeUsage>,
}

#[derive(Deserialize)]
struct ChargeUsage {
    cost: Option<f64>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    seconds: Option<f64>,
}

/// What the request answered by `body` cost: `usage.cost` from the answer, or failing that, for a successful answer,
/// the generation's cost looked up by its id (`header_id` when the answer names it only in a header). `None` when
/// neither says: a rejection such as a `verbose_json` 400 is not looked up, since it costs nothing.
async fn usage_of(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    status: u16,
    body: &str,
    header_id: Option<String>,
) -> Option<Usage> {
    let charge: Charge = serde_json::from_str(body).ok()?;
    let generation_id = charge.id.or(header_id);
    let usage = charge.usage;
    let cost = match usage.as_ref().and_then(|u| u.cost) {
        Some(cost) => cost,
        None if (200..=299).contains(&status) => generation_cost(client, base_url, api_key, generation_id.as_deref()?).await?,
        None => return None,
    };
    Some(Usage {
        model: model.to_owned(),
        cost,
        estimated: false,
        prompt_tokens: usage.as_ref().and_then(|u| u.prompt_tokens),
        completion_tokens: usage.as_ref().and_then(|u| u.completion_tokens),
        seconds: usage.as_ref().and_then(|u| u.seconds),
        generation_id,
    })
}

/// A generation's cost from `GET /generation`. Its stats can lag the answer, so a miss is tried once more.
async fn generation_cost(client: &reqwest::Client, base_url: &str, api_key: &str, id: &str) -> Option<f64> {
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let response = client.get(format!("{base_url}/generation")).query(&[("id", id)]).bearer_auth(api_key).send().await;
        let body: Option<Value> = match response {
            Ok(response) if response.status().is_success() => response.json().await.ok(),
            _ => None,
        };
        if let Some(cost) = body.and_then(|b| b["data"]["total_cost"].as_f64()) {
            return Some(cost);
        }
    }
    tracing::warn!("OpenRouter reported no cost for generation {id}; it is not recorded");
    None
}

/// The answers transcription and summaries treat alike: no point trying again until the user changes a setting.
enum Refusal {
    /// The model has no zero-data-retention endpoint.
    ZdrViolation(String),
    /// The API key was rejected.
    Unauthorized(String),
}

/// The Refusal in an OpenRouter answer, if it is one. `setting` names the model setting to change ("STT",
/// "summary", "Embedding" or "Ask").
fn refusal(status: u16, body: &str, model: &str, setting: &str) -> Option<Refusal> {
    match status {
        404 if body.contains("ZDR violation") => Some(Refusal::ZdrViolation(format!(
            "{model} has no zero-data-retention endpoint, so OpenRouter refused it. Choose another {setting} model in Settings."
        ))),
        401 | 403 => Some(Refusal::Unauthorized(format!(
            "OpenRouter rejected the API key ({}). Set a new key in Settings.",
            error_message(body)
        ))),
        _ => None,
    }
}

impl From<Refusal> for TranscribeError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::ZdrViolation(message) => TranscribeError::ZdrViolation(message),
            Refusal::Unauthorized(message) => TranscribeError::Unauthorized(message),
        }
    }
}

impl From<Refusal> for EmbedError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::ZdrViolation(message) => EmbedError::ZdrViolation(message),
            Refusal::Unauthorized(message) => EmbedError::Unauthorized(message),
        }
    }
}

impl From<Refusal> for AskError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::ZdrViolation(message) => AskError::ZdrViolation(message),
            Refusal::Unauthorized(message) => AskError::Unauthorized(message),
        }
    }
}

impl From<Refusal> for SummarizeError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::ZdrViolation(message) => SummarizeError::ZdrViolation(message),
            Refusal::Unauthorized(message) => SummarizeError::Unauthorized(message),
        }
    }
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(300).collect())
}

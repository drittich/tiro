//! Embeddings for meaning search: one per Passage from the Embedding model, kept in each Meeting's folder
//! (ADR-0005) and loaded into memory, where a query's Embedding is compared with every Passage's.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::search::{Fragment, Hit, Passage, Place};
use crate::spend::{Billed, SpendKind, SpendLedger};
use crate::store::{MeetingStatus, MeetingStore, StoreError};

pub const DEFAULT_EMBEDDING_MODEL: &str = "openai/text-embedding-3-small";

/// The most texts sent in one request, and the most characters (about 15k tokens) across them, which the
/// embedding models on OpenRouter all take.
const BATCH_TEXTS: usize = 96;
const BATCH_CHARS: usize = 60_000;

/// The most Passages one search takes from the meaning ranking.
const MEANING_HITS: usize = 50;
/// A Passage less similar to the query than this is no match, however few there are.
const MIN_SIMILARITY: f32 = 0.25;
/// A Passage less similar to the query than this share of the best one's similarity is no match either: a query
/// with little meaning is about as similar to every Passage, and only the ones standing out count.
const SHARE_OF_BEST: f32 = 0.8;
/// A meaning-only Hit's snippet: the Passage's first words.
const SNIPPET_WORDS: usize = 32;
/// The most query vectors kept, so typing a query again costs nothing.
const QUERY_CACHE: usize = 200;

const MANIFEST: &str = "embeddings.json";
const VECTORS: &str = "embeddings.bin";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EmbedError {
    /// The Embedding model has no zero-data-retention endpoint; the user must pick another model.
    #[error("{0}")]
    ZdrViolation(String),
    /// OpenRouter rejected the API key.
    #[error("{0}")]
    Unauthorized(String),
    /// Anything else about the request: network trouble, server errors, an answer that doesn't fit. Trying again may
    /// help.
    #[error("{0}")]
    Failed(String),
    /// The Meeting's files couldn't be read or its Embeddings saved, as when it was deleted meanwhile. No request
    /// failed.
    #[error("{0}")]
    Meeting(String),
}

/// The Embedding model boundary.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// The model's id: vectors from different models can't be compared.
    fn model(&self) -> &str;
    /// One vector per text, in order, and what the request cost.
    async fn embed(&self, texts: &[String]) -> Billed<Vec<Vec<f32>>, EmbedError>;
}

/// A Passage with its vector, scaled to length 1 so a dot product is the cosine similarity.
struct Embedded {
    place: Place,
    text: String,
    vector: Vec<f32>,
}

struct MeetingVectors {
    model: String,
    passages: Vec<Embedded>,
}

/// Every embedded Passage, in memory, for meaning search. Starts empty and fills as Meetings are embedded or
/// their stored vectors are read (`embed_meeting`).
#[derive(Default)]
pub struct MeaningIndex {
    meetings: RwLock<HashMap<String, MeetingVectors>>,
    /// Query vectors by model and query text.
    queries: Mutex<HashMap<(String, String), Vec<f32>>>,
}

impl MeaningIndex {
    /// Forgets the Meeting's vectors, as when it is deleted.
    pub fn remove(&self, meeting_id: &str) {
        self.meetings.write().unwrap().remove(meeting_id);
    }

    /// Forgets every Meeting's vectors, as when the embedding model changes.
    pub fn clear(&self) {
        self.meetings.write().unwrap().clear();
    }

    /// Whether any Passage has a vector from `model`.
    pub fn has_model(&self, model: &str) -> bool {
        self.meetings.read().unwrap().values().any(|m| m.model == model && !m.passages.is_empty())
    }

    fn set(&self, meeting_id: &str, model: &str, passages: Vec<Embedded>) {
        self.meetings.write().unwrap().insert(meeting_id.to_owned(), MeetingVectors { model: model.to_owned(), passages });
    }

    /// [`Self::set`], unless the Meeting already has `model`'s vectors, which were embedded since the files were read.
    fn set_if_absent(&self, meeting_id: &str, model: &str, passages: Vec<Embedded>) {
        let mut meetings = self.meetings.write().unwrap();
        if !meetings.get(meeting_id).is_some_and(|m| m.model == model) {
            meetings.insert(meeting_id.to_owned(), MeetingVectors { model: model.to_owned(), passages });
        }
    }

    /// The query's vector, from the cache or else from `embedder`, its cost recorded against no Meeting.
    pub async fn query_vector(&self, query: &str, embedder: &dyn Embedder, spend: &SpendLedger) -> Result<Vec<f32>, EmbedError> {
        let key = (embedder.model().to_owned(), query.to_owned());
        if let Some(vector) = self.queries.lock().unwrap().get(&key) {
            return Ok(vector.clone());
        }
        let billed = embedder.embed(&[query.to_owned()]).await;
        spend.record(None, SpendKind::Embedding, billed.usage.as_ref());
        let vector = billed.result?.into_iter().next().ok_or_else(|| EmbedError::Failed("no vector for the query".into()))?;
        let vector = normalized(vector);
        let mut queries = self.queries.lock().unwrap();
        if queries.len() >= QUERY_CACHE {
            queries.clear();
        }
        queries.insert(key, vector.clone());
        Ok(vector)
    }

    /// The Passages embedded by `model` most similar to `query` (a vector from [`Self::query_vector`]), best first:
    /// at most [`MEANING_HITS`], none below [`MIN_SIMILARITY`] or [`SHARE_OF_BEST`] of the best, only of the Meetings
    /// in `among` when it is given.
    /// Their snippets are plain, with nothing highlighted.
    pub fn ranked(&self, model: &str, query: &[f32], among: Option<&[String]>) -> Vec<Hit> {
        let meetings = self.meetings.read().unwrap();
        let mut scored: Vec<(f32, &str, &Embedded)> = Vec::new();
        for (id, vectors) in meetings.iter() {
            if vectors.model != model || among.is_some_and(|ids| !ids.contains(id)) {
                continue;
            }
            for passage in &vectors.passages {
                if passage.vector.len() != query.len() {
                    continue;
                }
                let similarity: f32 = passage.vector.iter().zip(query).map(|(a, b)| a * b).sum();
                if similarity >= MIN_SIMILARITY {
                    scored.push((similarity, id, passage));
                }
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let floor = scored.first().map_or(0.0, |best| best.0 * SHARE_OF_BEST);
        scored
            .into_iter()
            .take_while(|(similarity, ..)| *similarity >= floor)
            .take(MEANING_HITS)
            .map(|(similarity, id, passage)| Hit {
                meeting_id: id.to_owned(),
                place: passage.place.clone(),
                snippet: vec![Fragment { text: plain_snippet(&passage.text), matched: false }],
                score: similarity as f64,
            })
            .collect()
    }
}

/// Gives every Passage of the Meeting a vector from `embedder`'s model: the ones stored in its folder for the
/// same text and model are reused, and only the rest are sent, in as few requests as fit. The folder then keeps
/// the vectors of the Passages it has now, and `index` holds them. Each request's cost goes in `spend`, against
/// the Meeting. A Meeting still recording or transcribing is left until its Transcript is final. On a failed
/// request the vectors got so far are kept and the error returned.
pub async fn embed_meeting(
    store: &MeetingStore,
    index: &MeaningIndex,
    id: &str,
    embedder: &dyn Embedder,
    spend: &SpendLedger,
) -> Result<(), EmbedError> {
    let Some(Prepared { dir, passages, mut stored }) =
        prepare(store, id, embedder.model()).map_err(|e| EmbedError::Meeting(format!("could not read the Meeting: {e}")))?
    else {
        return Ok(());
    };
    let model = embedder.model();
    let before = stored.len();

    let mut missing: Vec<(&str, &str)> = Vec::new();
    for (_, text, hash) in &passages {
        if !stored.contains_key(hash) && !missing.iter().any(|(h, _)| h == hash) {
            missing.push((hash, text));
        }
    }
    let mut failure = None;
    for batch in batches(&missing) {
        let texts: Vec<String> = batch.iter().map(|(_, text)| (*text).to_owned()).collect();
        let billed = embedder.embed(&texts).await;
        spend.record(Some(id), SpendKind::Embedding, billed.usage.as_ref());
        match billed.result {
            Ok(vectors) if vectors.len() == batch.len() => {
                for ((hash, _), vector) in batch.iter().zip(vectors) {
                    stored.insert((*hash).to_owned(), normalized(vector));
                }
            }
            Ok(vectors) => {
                failure = Some(EmbedError::Failed(format!("{model} returned {} vectors for {} texts", vectors.len(), batch.len())));
                break;
            }
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
    }

    let mut current: Vec<(&str, &[f32])> = Vec::new();
    for (_, _, hash) in &passages {
        if let Some(vector) = stored.get(hash).filter(|_| !current.iter().any(|(h, _)| h == hash)) {
            current.push((hash, vector));
        }
    }
    // Rewritten when there are new vectors or stale ones to drop.
    if stored.len() != before || current.len() != before {
        write_vectors(&dir, model, &current)
            .map_err(|e| EmbedError::Meeting(format!("could not save the Meeting's Embeddings: {e}")))?;
    }
    index.set(id, model, embedded(passages, &stored));
    failure.map_or(Ok(()), Err)
}

/// Puts the vectors stored in the Meeting's folder by `model` in `index`, without any request, unless `index`
/// already has newer ones for it. Returns whether every Passage has one; a Meeting still recording or
/// transcribing has nothing to embed yet.
pub fn load_embedded(store: &MeetingStore, index: &MeaningIndex, id: &str, model: &str) -> Result<bool, StoreError> {
    let Some(Prepared { passages, stored, .. }) = prepare(store, id, model)? else { return Ok(true) };
    let complete = passages.iter().all(|(_, _, hash)| stored.contains_key(hash));
    index.set_if_absent(id, model, embedded(passages, &stored));
    Ok(complete)
}

/// A Meeting's Passages, each with the text embedded for it and that text's hash, and the vectors its folder holds
/// from `model`; `None` while it is still recording or transcribing.
struct Prepared {
    dir: PathBuf,
    passages: Vec<(Passage, String, String)>,
    stored: HashMap<String, Vec<f32>>,
}

fn prepare(store: &MeetingStore, id: &str, model: &str) -> Result<Option<Prepared>, StoreError> {
    let meeting = store.get(id)?;
    if matches!(meeting.status, MeetingStatus::Recording | MeetingStatus::Transcribing) {
        return Ok(None);
    }
    let passages = store
        .passages(id)?
        .into_iter()
        .map(|p| {
            let text = embedding_text(&p);
            let hash = hash(&text);
            (p, text, hash)
        })
        .collect();
    let stored = read_vectors(&meeting.dir, model);
    Ok(Some(Prepared { dir: meeting.dir, passages, stored }))
}

/// The Passages that have a vector in `stored`, with it.
fn embedded(passages: Vec<(Passage, String, String)>, stored: &HashMap<String, Vec<f32>>) -> Vec<Embedded> {
    passages
        .into_iter()
        .filter_map(|(passage, _, hash)| {
            let vector = stored.get(&hash)?.clone();
            Some(Embedded { place: passage.place, text: passage.text, vector })
        })
        .collect()
}

/// What is embedded for a Passage: its text, after its Summary section's heading when it has one.
fn embedding_text(passage: &Passage) -> String {
    match &passage.place {
        Place::Summary { heading: Some(heading), .. } if passage.text.is_empty() => heading.clone(),
        Place::Summary { heading: Some(heading), .. } => format!("{heading}\n{}", passage.text),
        _ => passage.text.clone(),
    }
}

fn hash(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The texts in runs of at most [`BATCH_TEXTS`] and [`BATCH_CHARS`]; a longer text goes alone.
fn batches<'a, 'b>(texts: &'a [(&'b str, &'b str)]) -> Vec<&'a [(&'b str, &'b str)]> {
    let mut batches = Vec::new();
    let (mut start, mut chars) = (0, 0);
    for (n, (_, text)) in texts.iter().enumerate() {
        if n > start && (n - start == BATCH_TEXTS || chars + text.len() > BATCH_CHARS) {
            batches.push(&texts[start..n]);
            (start, chars) = (n, 0);
        }
        chars += text.len();
    }
    if start < texts.len() {
        batches.push(&texts[start..]);
    }
    batches
}

fn normalized(mut vector: Vec<f32>) -> Vec<f32> {
    let length = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if length > 0.0 {
        vector.iter_mut().for_each(|x| *x /= length);
    }
    vector
}

fn plain_snippet(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= SNIPPET_WORDS {
        words.join(" ")
    } else {
        format!("{}…", words[..SNIPPET_WORDS].join(" "))
    }
}

/// `embeddings.json`: which model made the vectors in `embeddings.bin` and which Passage text each is for.
#[derive(Serialize, Deserialize)]
struct Manifest {
    model: String,
    dimensions: usize,
    /// The SHA-256 of each vector's text, in the order of `embeddings.bin`.
    hashes: Vec<String>,
}

/// The vectors stored in the Meeting folder `dir` by `model`, by text hash; none when there are none, they are
/// another model's, or the files don't agree.
fn read_vectors(dir: &Path, model: &str) -> HashMap<String, Vec<f32>> {
    let read = || -> Option<HashMap<String, Vec<f32>>> {
        let manifest: Manifest = serde_json::from_slice(&fs::read(dir.join(MANIFEST)).ok()?).ok()?;
        if manifest.model != model || manifest.dimensions == 0 {
            return None;
        }
        let bytes = fs::read(dir.join(VECTORS)).ok()?;
        if bytes.len() != manifest.hashes.len() * manifest.dimensions * 4 {
            tracing::warn!("ignoring the embeddings in {}: they don't match their manifest", dir.display());
            return None;
        }
        let floats: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        Some(manifest.hashes.into_iter().zip(floats.chunks_exact(manifest.dimensions).map(<[f32]>::to_vec)).collect())
    };
    read().unwrap_or_default()
}

/// Replaces the Meeting folder's vectors with these, or removes the files when there are none. The vectors are
/// written before the manifest that describes them, each atomically.
fn write_vectors(dir: &Path, model: &str, vectors: &[(&str, &[f32])]) -> std::io::Result<()> {
    if vectors.is_empty() {
        for name in [MANIFEST, VECTORS] {
            match fs::remove_file(dir.join(name)) {
                Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        return Ok(());
    }
    let dimensions = vectors[0].1.len();
    if vectors.iter().any(|(_, v)| v.len() != dimensions) {
        return Err(std::io::Error::other(format!("{model} returned vectors of different lengths")));
    }
    let bytes: Vec<u8> = vectors.iter().flat_map(|(_, v)| v.iter().flat_map(|x| x.to_le_bytes())).collect();
    crate::xdg::write_atomic(&dir.join(VECTORS), &bytes)?;
    let manifest = Manifest {
        model: model.to_owned(),
        dimensions,
        hashes: vectors.iter().map(|(hash, _)| (*hash).to_owned()).collect(),
    };
    crate::xdg::write_atomic(&dir.join(MANIFEST), &serde_json::to_vec_pretty(&manifest)?)
}

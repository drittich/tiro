//! The speech-to-text boundary.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::spend::Billed;
use crate::transcript::Word;

/// An encoded Chunk on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkAudio {
    pub path: PathBuf,
    /// Seconds of audio; used to estimate word times when the provider returns none.
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TranscribeError {
    /// The model has no zero-data-retention endpoint. Retrying cannot help; the user must pick another model.
    #[error("{0}")]
    ZdrViolation(String),
    /// OpenRouter rejected the API key. Retrying cannot help until the user sets a new one.
    #[error("{0}")]
    Unauthorized(String),
    /// The provider refused this Chunk (bad audio, unsupported format…). Retrying it will not help.
    #[error("chunk rejected: {0}")]
    Rejected(String),
    /// Network trouble, rate limits or server errors: worth retrying.
    #[error("{0}")]
    Transient(String),
}

#[async_trait]
pub trait Transcriber: Send + Sync {
    /// Returns the Chunk's words with times relative to the start of the Chunk, and what transcribing it cost.
    async fn transcribe(&self, audio: &ChunkAudio) -> Billed<Vec<Word>, TranscribeError>;
}

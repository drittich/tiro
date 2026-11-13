//! Sending Chunks to the Transcriber, in order per Track, retrying with backoff.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::assembler::ChunkSpan;
use crate::spend::Usage;
use crate::transcriber::{ChunkAudio, TranscribeError, Transcriber};
use crate::transcript::{Track, Word};

#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueuedChunk {
    pub track: Track,
    pub span: ChunkSpan,
    pub audio: ChunkAudio,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueueEvent {
    Transcribed { track: Track, span: ChunkSpan, words: Vec<Word> },
    Retrying { track: Track, span: ChunkSpan, error: TranscribeError, delay: Duration },
    Failed { track: Track, span: ChunkSpan, error: TranscribeError },
    Blocked { error: TranscribeError },
    /// The Track has no speech before `until` (seconds) beyond the Chunks already reported.
    Silent { track: Track, until: f64 },
    Drained { track: Track },
    /// What one request cost, whatever came of it. Precedes the event reporting that outcome.
    Charged { usage: Usage },
}

/// What a Track's worker processes, in order.
enum TrackItem {
    Chunk(QueuedChunk),
    Silence(f64),
}

/// One worker per Track transcribes that Track's Chunks strictly in order, so a Track that is retrying never
/// reorders its own Chunks but never holds up the other Track either. New Chunks queue up while a worker
/// retries, so recording carries on while offline.
///
/// A transcribed Chunk's file is deleted. A rejected Chunk is kept on disk and skipped. A ZDR violation or a
/// rejected API key blocks the whole queue: it is reported once and later Chunks are kept on disk untouched.
pub struct TranscriptionQueue {
    senders: Mutex<HashMap<Track, UnboundedSender<TrackItem>>>,
}

impl TranscriptionQueue {
    pub fn start(transcriber: Arc<dyn Transcriber>, backoff: Backoff) -> (Self, UnboundedReceiver<QueueEvent>) {
        let (events, receiver) = unbounded_channel();
        let blocked = Arc::new(AtomicBool::new(false));
        let mut senders = HashMap::new();
        for track in Track::ALL {
            let (sender, chunks) = unbounded_channel();
            senders.insert(track, sender);
            let worker = Worker { track, transcriber: transcriber.clone(), backoff, events: events.clone(), blocked: blocked.clone() };
            tokio::spawn(worker.run(chunks));
        }
        (Self { senders: Mutex::new(senders) }, receiver)
    }

    pub fn enqueue(&self, chunk: QueuedChunk) {
        self.send(chunk.track, TrackItem::Chunk(chunk));
    }

    /// The Track will produce no Chunk starting before `until`. Reported as `Silent` once the Chunks queued
    /// before it are done, so the Transcript need not wait on a Track nobody is speaking on.
    pub fn silent(&self, track: Track, until: f64) {
        self.send(track, TrackItem::Silence(until));
    }

    fn send(&self, track: Track, item: TrackItem) {
        if let Some(sender) = self.senders.lock().unwrap().get(&track) {
            let _ = sender.send(item);
        }
    }

    /// No more Chunks are coming for this Track; `Drained` follows once its queue is empty.
    pub fn close(&self, track: Track) {
        self.senders.lock().unwrap().remove(&track);
    }
}

struct Worker {
    track: Track,
    transcriber: Arc<dyn Transcriber>,
    backoff: Backoff,
    events: UnboundedSender<QueueEvent>,
    blocked: Arc<AtomicBool>,
}

impl Worker {
    async fn run(self, mut items: UnboundedReceiver<TrackItem>) {
        while let Some(item) = items.recv().await {
            match item {
                TrackItem::Chunk(chunk) => self.process(chunk).await,
                TrackItem::Silence(until) => {
                    let _ = self.events.send(QueueEvent::Silent { track: self.track, until });
                }
            }
        }
        let _ = self.events.send(QueueEvent::Drained { track: self.track });
    }

    async fn process(&self, chunk: QueuedChunk) {
        let mut delay = self.backoff.initial;
        loop {
            if self.blocked.load(Ordering::SeqCst) {
                return;
            }
            let billed = self.transcriber.transcribe(&chunk.audio).await;
            if let Some(usage) = billed.usage {
                let _ = self.events.send(QueueEvent::Charged { usage });
            }
            let error = match billed.result {
                Ok(words) => {
                    if let Err(e) = tokio::fs::remove_file(&chunk.audio.path).await {
                        tracing::warn!("could not delete {}: {e}", chunk.audio.path.display());
                    }
                    let _ = self.events.send(QueueEvent::Transcribed { track: self.track, span: chunk.span, words });
                    return;
                }
                Err(error) => error,
            };
            match error {
                TranscribeError::Transient(_) => {
                    let _ = self.events.send(QueueEvent::Retrying { track: self.track, span: chunk.span, error, delay });
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(self.backoff.max);
                }
                TranscribeError::Rejected(_) => {
                    let _ = self.events.send(QueueEvent::Failed { track: self.track, span: chunk.span, error });
                    return;
                }
                TranscribeError::ZdrViolation(_) | TranscribeError::Unauthorized(_) => {
                    if !self.blocked.swap(true, Ordering::SeqCst) {
                        let _ = self.events.send(QueueEvent::Blocked { error });
                    }
                    return;
                }
            }
        }
    }
}

//! The transcription pipeline for one Meeting: captured audio in, Segments in `transcript.jsonl` and live events out.
//!
//! ```text
//! Track reader ─▶ VAD Chunker ─▶ encoder (chunks/*.ogg) ─▶ TranscriptionQueue
//!                                                              │ QueueEvent
//!                                  events ◀─ store ◀─ TranscriptBuilder (overlap and echo dedupe)
//! ```
//!
//! What each Chunk cost goes in the spend ledger under the Meeting's id.

use std::io::{ErrorKind, Read};
use std::sync::Arc;
use std::thread::JoinHandle;

use serde::Serialize;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::assembler::{AssemblerConfig, ChunkSpan};
use crate::chunker::{Chunk, Chunker, FixedChunker, VadChunker, VadConfig};
use crate::encoder::write_ogg_opus;
use crate::queue::{Backoff, QueueEvent, QueuedChunk, TranscriptionQueue};
use crate::retry::chunk_file_name;
use crate::spend::{SpendKind, SpendLedger};
use crate::store::{Meeting, MeetingStatus, MeetingStore};
use crate::transcriber::{ChunkAudio, Transcriber};
use crate::transcript::{Segment, Track};
use crate::transcript_builder::TranscriptBuilder;
use crate::vad::SileroVad;

#[derive(Debug, Clone, Copy)]
pub struct PipelineConfig {
    pub sample_rate: u32,
    pub chunking: Chunking,
    pub overlap_secs: f64,
    pub pause_secs: f64,
    pub max_segment_secs: f64,
    /// How far apart (seconds) a You word and the Others word it echoes may be.
    pub echo_window_secs: f64,
    pub backoff: Backoff,
}

/// How each Track's audio is cut into Chunks.
#[derive(Debug, Clone, Copy)]
pub enum Chunking {
    /// Chunks bounded by pauses, found by Silero VAD. Falls back to `Fixed` if the model cannot load.
    Vad(VadConfig),
    /// Fixed-length Chunks of this many seconds.
    Fixed(f64),
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            chunking: Chunking::Vad(VadConfig::default()),
            overlap_secs: 2.0,
            pause_secs: 1.0,
            max_segment_secs: 30.0,
            echo_window_secs: 1.0,
            backoff: Backoff { initial: std::time::Duration::from_secs(2), max: std::time::Duration::from_secs(60) },
        }
    }
}

/// What the UI hears about a running Meeting.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PipelineEvent {
    Segments { meeting_id: String, segments: Vec<Segment> },
    /// A transient problem; the Chunk will be retried.
    Retrying { meeting_id: String, track: Track, message: String, delay_secs: u64 },
    /// A problem retrying cannot fix. Recording carries on and Chunks stay on disk.
    Error { meeting_id: String, message: String },
    /// Every Chunk has been processed: the capture ended (stopped, or failed on its own) and the queue drained.
    Finished { meeting_id: String },
}

/// Reads s16le mono samples from each Track's reader until it ends, so stopping a recording means ending the
/// readers (e.g. killing the capture process).
pub struct Pipeline {
    readers: Vec<JoinHandle<()>>,
}

impl Pipeline {
    /// Must be called inside a tokio runtime.
    pub fn start(
        store: Arc<MeetingStore>,
        meeting: Meeting,
        transcriber: Arc<dyn Transcriber>,
        spend: Arc<SpendLedger>,
        config: PipelineConfig,
        inputs: Vec<(Track, Box<dyn Read + Send>)>,
        on_event: impl Fn(PipelineEvent) + Send + 'static,
    ) -> Self {
        let (queue, events) = TranscriptionQueue::start(transcriber, config.backoff);
        let queue = Arc::new(queue);

        let tracks: Vec<Track> = inputs.iter().map(|(t, _)| *t).collect();
        tokio::spawn(assemble(store, spend, meeting.clone(), config, tracks, events, on_event));

        let readers = inputs
            .into_iter()
            .map(|(track, reader)| {
                let queue = queue.clone();
                let meeting = meeting.clone();
                std::thread::Builder::new()
                    .name(format!("capture-{}", track.as_str()))
                    .spawn(move || read_track(track, reader, &meeting, &config, &queue))
                    .expect("spawn capture thread")
            })
            .collect();
        Self { readers }
    }

    /// Waits for the readers to end (after the capture processes have been stopped) and their last Chunks to
    /// be queued. Transcription carries on in the background until `PipelineEvent::Finished`.
    pub fn join(self) {
        for reader in self.readers {
            let _ = reader.join();
        }
    }
}

fn read_track(track: Track, mut reader: Box<dyn Read + Send>, meeting: &Meeting, config: &PipelineConfig, queue: &TranscriptionQueue) {
    const SILENCE_REPORT_SECS: f64 = 1.0;
    let mut chunker = chunker(track, config);
    let mut silent_until = 0.0;
    let mut bytes = vec![0u8; 3200];
    let mut carry: Option<u8> = None;
    let mut samples = Vec::with_capacity(1600);
    loop {
        let n = match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                tracing::error!("{} capture failed: {e}", track.as_str());
                break;
            }
        };
        samples.clear();
        let mut data = &bytes[..n];
        if let Some(lo) = carry.take() {
            samples.push(i16::from_le_bytes([lo, data[0]]));
            data = &data[1..];
        }
        let (pairs, rest) = data.as_chunks::<2>();
        carry = rest.first().copied();
        samples.extend(pairs.iter().map(|&p| i16::from_le_bytes(p)));
        for chunk in chunker.push(&samples) {
            enqueue(track, chunk, meeting, queue);
        }
        // While nobody speaks on this Track, say so now and then, so the Transcript need not wait for it.
        let horizon = chunker.horizon();
        if horizon >= silent_until + SILENCE_REPORT_SECS {
            queue.silent(track, horizon);
            silent_until = horizon;
        }
    }
    if let Some(chunk) = chunker.finish() {
        enqueue(track, chunk, meeting, queue);
    }
    queue.close(track);
}

fn chunker(track: Track, config: &PipelineConfig) -> Box<dyn Chunker> {
    const FALLBACK_SECS: f64 = 30.0;
    let secs = match config.chunking {
        Chunking::Vad(vad) => match SileroVad::new() {
            Ok(detector) => return Box::new(VadChunker::new(detector, config.sample_rate, config.overlap_secs, vad)),
            Err(e) => {
                tracing::error!("{}: {e}; sending fixed-length Chunks, silence included", track.as_str());
                FALLBACK_SECS
            }
        },
        Chunking::Fixed(secs) => secs,
    };
    Box::new(FixedChunker::new(config.sample_rate, secs, config.overlap_secs))
}

fn enqueue(track: Track, chunk: Chunk, meeting: &Meeting, queue: &TranscriptionQueue) {
    let span = ChunkSpan { index: chunk.index, start: chunk.start, end: chunk.end() };
    let path = meeting.chunks_dir().join(chunk_file_name(track, span));
    if let Err(e) = write_ogg_opus(&path, chunk.sample_rate, &chunk.samples) {
        tracing::error!("could not write {}: {e}", path.display());
        return;
    }
    let duration = span.end - span.start;
    queue.enqueue(QueuedChunk { track, span, audio: ChunkAudio { path, duration } });
}

async fn assemble(
    store: Arc<MeetingStore>,
    spend: Arc<SpendLedger>,
    meeting: Meeting,
    config: PipelineConfig,
    tracks: Vec<Track>,
    mut events: UnboundedReceiver<QueueEvent>,
    on_event: impl Fn(PipelineEvent),
) {
    let assembler_config =
        AssemblerConfig { overlap: config.overlap_secs, pause: config.pause_secs, max_segment: config.max_segment_secs };
    let mut transcript = TranscriptBuilder::new(&tracks, assembler_config, config.echo_window_secs);
    let id = meeting.id.clone();
    let mut incomplete = false;

    while let Some(event) = events.recv().await {
        let released = match event {
            QueueEvent::Transcribed { track, span, words } => transcript.accept(track, span, words),
            QueueEvent::Silent { track, until } => transcript.silent(track, until),
            QueueEvent::Drained { track } => transcript.drain(track),
            QueueEvent::Charged { usage } => {
                spend.record(Some(&id), SpendKind::Transcription, Some(&usage));
                vec![]
            }
            QueueEvent::Retrying { track, error, delay, .. } => {
                on_event(PipelineEvent::Retrying {
                    meeting_id: id.clone(),
                    track,
                    message: error.to_string(),
                    delay_secs: delay.as_secs(),
                });
                vec![]
            }
            QueueEvent::Failed { track, span, error } => {
                incomplete = true;
                on_event(PipelineEvent::Error {
                    meeting_id: id.clone(),
                    message: format!("{} Chunk at {:.0}s was skipped: {error}", track.as_str(), span.start),
                });
                vec![]
            }
            QueueEvent::Blocked { error } => {
                incomplete = true;
                on_event(PipelineEvent::Error { meeting_id: id.clone(), message: error.to_string() });
                vec![]
            }
        };
        if !released.is_empty() {
            if let Err(e) = store.append_segments(&meeting, &released) {
                on_event(PipelineEvent::Error { meeting_id: id.clone(), message: format!("could not save the Transcript: {e}") });
            }
            on_event(PipelineEvent::Segments { meeting_id: id.clone(), segments: released });
        }
        if transcript.is_done() {
            break;
        }
    }
    // Any Chunk still on disk (skipped, blocked, or transcribed but not deleted) makes the Meeting incomplete,
    // so the retry after stop picks it up.
    let leftover = std::fs::read_dir(meeting.chunks_dir()).is_ok_and(|mut entries| entries.next().is_some());
    let status = if incomplete || leftover { MeetingStatus::Incomplete } else { MeetingStatus::Complete };
    let _ = store.set_status(&id, status, None);
    on_event(PipelineEvent::Finished { meeting_id: id });
}

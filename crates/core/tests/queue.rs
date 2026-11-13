use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tiro_core::assembler::ChunkSpan;
use tiro_core::queue::{Backoff, QueueEvent, QueuedChunk, TranscriptionQueue};
use tiro_core::spend::Billed;
use tiro_core::transcriber::{ChunkAudio, TranscribeError, Transcriber};
use tiro_core::transcript::{Track, Word};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

/// Answers each call from a script; once the script runs out, every Chunk transcribes as its file name.
#[derive(Default, Clone)]
struct FakeTranscriber {
    script: Arc<Mutex<VecDeque<Result<(), TranscribeError>>>>,
    calls: Arc<Mutex<Vec<(String, Instant)>>>,
}

impl FakeTranscriber {
    fn scripted(results: Vec<Result<(), TranscribeError>>) -> Self {
        Self { script: Arc::new(Mutex::new(results.into())), ..Default::default() }
    }

    fn calls(&self) -> Vec<(String, Instant)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Transcriber for FakeTranscriber {
    async fn transcribe(&self, audio: &ChunkAudio) -> Billed<Vec<Word>, TranscribeError> {
        let name = audio.path.file_stem().unwrap().to_string_lossy().into_owned();
        self.calls.lock().unwrap().push((name.clone(), Instant::now()));
        let scripted = self.script.lock().unwrap().pop_front();
        scripted.unwrap_or(Ok(())).map(|()| vec![Word { text: name, start: 0.0, end: 1.0 }]).into()
    }
}

fn backoff() -> Backoff {
    Backoff { initial: Duration::from_secs(2), max: Duration::from_secs(60) }
}

fn chunk_file(dir: &Path, track: Track, index: u32) -> QueuedChunk {
    let path: PathBuf = dir.join(format!("{}-{index}.ogg", track.as_str()));
    std::fs::write(&path, b"audio").unwrap();
    QueuedChunk {
        track,
        span: ChunkSpan { index, start: index as f64 * 28.0, end: index as f64 * 28.0 + 30.0 },
        audio: ChunkAudio { path, duration: 30.0 },
    }
}

async fn next(events: &mut UnboundedReceiver<QueueEvent>) -> QueueEvent {
    tokio::time::timeout(Duration::from_secs(3600), events.recv()).await.expect("event").expect("open")
}

fn transcribed_text(event: &QueueEvent) -> Option<&str> {
    match event {
        QueueEvent::Transcribed { words, .. } => Some(words[0].text.as_str()),
        _ => None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_transcribed_chunk_is_reported_and_its_file_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTranscriber::default();
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(fake.clone()), backoff());

    let chunk = chunk_file(dir.path(), Track::Others, 0);
    let path = chunk.audio.path.clone();
    queue.enqueue(chunk.clone());

    let event = next(&mut events).await;
    assert_eq!(
        event,
        QueueEvent::Transcribed { track: Track::Others, span: chunk.span, words: vec![Word { text: "others-0".into(), start: 0.0, end: 1.0 }] }
    );
    assert!(!path.exists());
}

#[tokio::test(start_paused = true)]
async fn transient_failures_back_off_exponentially_while_later_chunks_wait_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let offline = || Err(TranscribeError::Transient("connection refused".into()));
    let fake = FakeTranscriber::scripted(vec![offline(), offline(), offline()]);
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(fake.clone()), backoff());

    for index in 0..3 {
        queue.enqueue(chunk_file(dir.path(), Track::You, index));
    }

    let mut done = Vec::new();
    while done.len() < 3 {
        if let Some(text) = transcribed_text(&next(&mut events).await) {
            done.push(text.to_owned());
        }
    }
    assert_eq!(done, vec!["you-0", "you-1", "you-2"]);

    let calls = fake.calls();
    let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["you-0", "you-0", "you-0", "you-0", "you-1", "you-2"]);
    let gaps: Vec<u64> = calls.windows(2).take(3).map(|p| (p[1].1 - p[0].1).as_secs()).collect();
    assert_eq!(gaps, vec![2, 4, 8]);
}

#[tokio::test(start_paused = true)]
async fn backoff_is_capped() {
    let dir = tempfile::tempdir().unwrap();
    let offline = || Err(TranscribeError::Transient("503".into()));
    let fake = FakeTranscriber::scripted((0..8).map(|_| offline()).collect());
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(fake.clone()), backoff());

    queue.enqueue(chunk_file(dir.path(), Track::You, 0));
    while transcribed_text(&next(&mut events).await).is_none() {}

    let calls = fake.calls();
    let gaps: Vec<u64> = calls.windows(2).map(|p| (p[1].1 - p[0].1).as_secs()).collect();
    assert_eq!(gaps, vec![2, 4, 8, 16, 32, 60, 60, 60]);
}

#[tokio::test(start_paused = true)]
async fn one_track_retrying_does_not_hold_up_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTranscriber::scripted(vec![Err(TranscribeError::Transient("timeout".into()))]);
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(fake.clone()), backoff());

    queue.enqueue(chunk_file(dir.path(), Track::You, 0));
    tokio::task::yield_now().await;
    queue.enqueue(chunk_file(dir.path(), Track::Others, 0));

    let mut done = Vec::new();
    while done.len() < 2 {
        if let Some(text) = transcribed_text(&next(&mut events).await) {
            done.push(text.to_owned());
        }
    }
    assert_eq!(done, vec!["others-0", "you-0"]);
}

#[tokio::test(start_paused = true)]
async fn a_zdr_violation_is_reported_once_and_never_retried() {
    let dir = tempfile::tempdir().unwrap();
    let zdr = TranscribeError::ZdrViolation("No endpoints available matching your guardrail restrictions (ZDR violation)".into());
    let fake = FakeTranscriber::scripted(vec![Err(zdr.clone())]);
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(fake.clone()), backoff());

    let first = chunk_file(dir.path(), Track::Others, 0);
    let second = chunk_file(dir.path(), Track::Others, 1);
    let kept = [first.audio.path.clone(), second.audio.path.clone()];
    queue.enqueue(first);
    queue.enqueue(second);

    assert_eq!(next(&mut events).await, QueueEvent::Blocked { error: zdr });
    tokio::time::sleep(Duration::from_secs(600)).await;

    assert_eq!(fake.calls().len(), 1);
    assert!(events.try_recv().is_err(), "no further events");
    assert!(kept.iter().all(|p| p.exists()), "Chunks stay on disk for a later retry");
}

#[tokio::test(start_paused = true)]
async fn a_rejected_api_key_blocks_the_queue_like_a_zdr_violation() {
    let dir = tempfile::tempdir().unwrap();
    let unauthorized = TranscribeError::Unauthorized("OpenRouter rejected the API key".into());
    let fake = FakeTranscriber::scripted(vec![Err(unauthorized.clone())]);
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(fake.clone()), backoff());

    queue.enqueue(chunk_file(dir.path(), Track::You, 0));
    queue.enqueue(chunk_file(dir.path(), Track::Others, 0));

    assert_eq!(next(&mut events).await, QueueEvent::Blocked { error: unauthorized });
    tokio::time::sleep(Duration::from_secs(600)).await;
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_rejected_chunk_is_kept_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTranscriber::scripted(vec![Err(TranscribeError::Rejected("corrupt audio".into()))]);
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(fake.clone()), backoff());

    let bad = chunk_file(dir.path(), Track::You, 0);
    let bad_path = bad.audio.path.clone();
    queue.enqueue(bad.clone());
    queue.enqueue(chunk_file(dir.path(), Track::You, 1));

    assert_eq!(
        next(&mut events).await,
        QueueEvent::Failed { track: Track::You, span: bad.span, error: TranscribeError::Rejected("corrupt audio".into()) }
    );
    assert_eq!(transcribed_text(&next(&mut events).await), Some("you-1"));
    assert!(bad_path.exists());
}

#[tokio::test(start_paused = true)]
async fn closing_a_track_reports_it_drained_after_its_last_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(FakeTranscriber::default()), backoff());

    queue.enqueue(chunk_file(dir.path(), Track::You, 0));
    queue.close(Track::You);

    assert_eq!(transcribed_text(&next(&mut events).await), Some("you-0"));
    assert_eq!(next(&mut events).await, QueueEvent::Drained { track: Track::You });
}

#[tokio::test(start_paused = true)]
async fn silence_is_reported_in_order_behind_the_chunks_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let transcriber = FakeTranscriber::scripted(vec![Err(TranscribeError::Transient("503".into()))]);
    let (queue, mut events) = TranscriptionQueue::start(Arc::new(transcriber), backoff());

    queue.enqueue(chunk_file(dir.path(), Track::You, 0));
    queue.silent(Track::You, 45.0);

    assert!(matches!(next(&mut events).await, QueueEvent::Retrying { .. }));
    assert_eq!(transcribed_text(&next(&mut events).await), Some("you-0"));
    assert_eq!(next(&mut events).await, QueueEvent::Silent { track: Track::You, until: 45.0 });
}

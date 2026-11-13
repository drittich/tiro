use std::io::{Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::DateTime;
use tiro_core::pipeline::{Chunking, Pipeline, PipelineConfig, PipelineEvent};
use tiro_core::spend::{Billed, SpendKind, SpendLedger, Usage};
use tiro_core::store::{MeetingStatus, MeetingStore};
use tiro_core::transcriber::{ChunkAudio, TranscribeError, Transcriber};
use tiro_core::transcript::{Track, Word};

/// Transcribes every Chunk, but write-protects `chunks/` first, so the transcribed file cannot be deleted.
struct UndeletableChunks;

#[async_trait]
impl Transcriber for UndeletableChunks {
    async fn transcribe(&self, audio: &ChunkAudio) -> Billed<Vec<Word>, TranscribeError> {
        let dir = audio.path.parent().unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        Ok(vec![Word { text: "hello".into(), start: 0.2, end: 0.6 }]).into()
    }
}

#[tokio::test]
async fn a_meeting_with_chunks_left_on_disk_finishes_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap());
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339("2026-10-17T14:05:00-04:00").unwrap()).unwrap();
    let config = PipelineConfig { chunking: Chunking::Fixed(1.0), overlap_secs: 0.0, ..PipelineConfig::default() };
    let silence: Box<dyn Read + Send> = Box::new(Cursor::new(vec![0u8; 16_000 * 2]));

    let (events, mut finished) = tokio::sync::mpsc::unbounded_channel();
    let spend = Arc::new(SpendLedger::new(dir.path().join("spend.jsonl")));
    let inputs = vec![(Track::Others, silence)];
    let pipeline = Pipeline::start(store.clone(), meeting.clone(), Arc::new(UndeletableChunks), spend, config, inputs, move |e| {
        if matches!(e, PipelineEvent::Finished { .. }) {
            let _ = events.send(());
        }
    });
    tokio::task::spawn_blocking(move || pipeline.join()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), finished.recv()).await.expect("finished");
    std::fs::set_permissions(meeting.chunks_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(std::fs::read_dir(meeting.chunks_dir()).unwrap().count(), 1, "the transcribed Chunk could not be deleted");
    assert_eq!(store.get(&meeting.id).unwrap().status, MeetingStatus::Incomplete, "so the retry after stop picks it up");
}

/// Transcribes every Chunk as one word, charging for each.
struct Charging;

#[async_trait]
impl Transcriber for Charging {
    async fn transcribe(&self, audio: &ChunkAudio) -> Billed<Vec<Word>, TranscribeError> {
        let usage = Usage {
            model: "elevenlabs/scribe-v2".into(),
            cost: 0.0002,
            estimated: false,
            prompt_tokens: None,
            completion_tokens: None,
            seconds: Some(audio.duration),
            generation_id: None,
        };
        Billed { result: Ok(vec![Word { text: "hello".into(), start: 0.2, end: 0.6 }]), usage: Some(usage) }
    }
}

#[tokio::test]
async fn each_chunk_transcribed_while_recording_is_recorded_against_the_meeting() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap());
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339("2026-10-17T14:05:00-04:00").unwrap()).unwrap();
    let config = PipelineConfig { chunking: Chunking::Fixed(1.0), overlap_secs: 0.0, ..PipelineConfig::default() };
    let two_seconds: Box<dyn Read + Send> = Box::new(Cursor::new(vec![0u8; 16_000 * 2 * 2]));
    let spend = Arc::new(SpendLedger::new(dir.path().join("spend.jsonl")));

    let (events, mut finished) = tokio::sync::mpsc::unbounded_channel();
    let inputs = vec![(Track::Others, two_seconds)];
    let pipeline = Pipeline::start(store.clone(), meeting.clone(), Arc::new(Charging), spend.clone(), config, inputs, move |e| {
        if matches!(e, PipelineEvent::Finished { .. }) {
            let _ = events.send(());
        }
    });
    tokio::task::spawn_blocking(move || pipeline.join()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), finished.recv()).await.expect("finished");

    let entries = spend.entries();
    assert_eq!(entries.len(), 2, "one per Chunk");
    assert!(entries.iter().all(|e| e.kind == SpendKind::Transcription && e.meeting_id.as_deref() == Some(meeting.id.as_str())));
}

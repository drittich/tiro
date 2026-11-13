use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::DateTime;
use tiro_core::assembler::ChunkSpan;
use tiro_core::pipeline::PipelineConfig;
use tiro_core::retry::{chunk_file_name, parse_chunk_file_name, retry_failed_chunks, RetryReport};
use tiro_core::spend::{Billed, SpendKind, SpendLedger, Usage};
use tiro_core::store::{Meeting, MeetingStatus, MeetingStore};
use tiro_core::transcriber::{ChunkAudio, TranscribeError, Transcriber};
use tiro_core::transcript::{Segment, Track, Word};

type Answer = Result<Vec<Word>, TranscribeError>;

/// Answers by file name; a name it has no answer for fails as `Transient`. Charges `cost` for every answer, if set.
#[derive(Default, Clone)]
struct FakeTranscriber {
    answers: Arc<Mutex<HashMap<String, Answer>>>,
    calls: Arc<Mutex<Vec<(String, f64)>>>,
    cost: Option<f64>,
}

impl FakeTranscriber {
    fn answer(&self, name: &str, answer: Answer) {
        self.answers.lock().unwrap().insert(name.to_owned(), answer);
    }
}

#[async_trait]
impl Transcriber for FakeTranscriber {
    async fn transcribe(&self, audio: &ChunkAudio) -> Billed<Vec<Word>, TranscribeError> {
        let name = audio.path.file_name().unwrap().to_string_lossy().into_owned();
        self.calls.lock().unwrap().push((name.clone(), audio.duration));
        let result = self.answers.lock().unwrap().get(&name).cloned().unwrap_or(Err(TranscribeError::Transient("offline".into())));
        let usage = self.cost.map(|cost| Usage {
            model: "elevenlabs/scribe-v2".into(),
            cost,
            estimated: false,
            prompt_tokens: None,
            completion_tokens: None,
            seconds: Some(audio.duration),
            generation_id: None,
        });
        Billed { result, usage }
    }
}

fn w(text: &str, start: f64, end: f64) -> Word {
    Word { text: text.into(), start, end }
}

fn seg(track: Track, start: f64, end: f64, text: &str) -> Segment {
    Segment { track, start, end, text: text.into(), speaker: None }
}

fn span(index: u32, start: f64, end: f64) -> ChunkSpan {
    ChunkSpan { index, start, end }
}

struct Env {
    _dir: tempfile::TempDir,
    store: MeetingStore,
    meeting: Meeting,
    spend: SpendLedger,
}

/// An incomplete Meeting, as a recording with failed Chunks leaves it.
fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap();
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339("2026-10-17T14:05:00-04:00").unwrap()).unwrap();
    store.set_status(&meeting.id, MeetingStatus::Incomplete, None).unwrap();
    let spend = SpendLedger::new(dir.path().join("spend.jsonl"));
    Env { _dir: dir, store, meeting, spend }
}

fn leave_chunk(meeting: &Meeting, track: Track, span: ChunkSpan) -> String {
    let name = chunk_file_name(track, span);
    std::fs::write(meeting.chunks_dir().join(&name), b"audio").unwrap();
    name
}

#[test]
fn a_chunk_file_name_records_its_track_and_place_on_the_timeline() {
    let name = chunk_file_name(Track::Others, span(7, 152.25, 181.5));
    assert_eq!(name, "others-00007-000152250-000181500.ogg");
    assert_eq!(parse_chunk_file_name(&name), Some((Track::Others, span(7, 152.25, 181.5))));
    assert_eq!(parse_chunk_file_name("you-00003.ogg"), None, "older Chunks carry no times");
    assert_eq!(parse_chunk_file_name("notes.md"), None);
}

#[tokio::test]
async fn recovered_chunks_join_the_transcript_in_time_order_and_their_files_go() {
    let env = env();
    env.store
        .append_segments(&env.meeting, &[seg(Track::Others, 1.0, 3.0, "Welcome everyone."), seg(Track::Others, 70.0, 72.0, "Any questions?")])
        .unwrap();
    let fake = FakeTranscriber::default();
    let others = leave_chunk(&env.meeting, Track::Others, span(1, 30.0, 60.0));
    let you = leave_chunk(&env.meeting, Track::You, span(0, 40.0, 50.0));
    fake.answer(&others, Ok(vec![w("Budget", 2.0, 2.5), w("first.", 2.6, 3.0)]));
    fake.answer(&you, Ok(vec![w("Agreed.", 5.0, 5.5)]));

    let report = retry_failed_chunks(&env.store, &env.meeting.id, &fake, &PipelineConfig::default(), &env.spend).await;

    assert_eq!(report, RetryReport { recovered: 2, remaining: 0, error: None });
    assert_eq!(
        env.store.transcript(&env.meeting).unwrap(),
        vec![
            seg(Track::Others, 1.0, 3.0, "Welcome everyone."),
            seg(Track::Others, 32.0, 33.0, "Budget first."),
            seg(Track::You, 45.0, 45.5, "Agreed."),
            seg(Track::Others, 70.0, 72.0, "Any questions?"),
        ]
    );
    assert_eq!(std::fs::read_dir(env.meeting.chunks_dir()).unwrap().count(), 0);
    assert_eq!(env.store.get(&env.meeting.id).unwrap().status, MeetingStatus::Complete);
    assert_eq!(fake.calls.lock().unwrap().iter().find(|(n, _)| n == &others).unwrap().1, 30.0, "duration from the name");
}

#[tokio::test]
async fn words_already_in_the_transcript_from_the_overlap_are_not_repeated() {
    let env = env();
    // The previous Chunk (0–30 s) was transcribed; its last words sit in the 2 s overlap with the failed one.
    env.store.append_segments(&env.meeting, &[seg(Track::Others, 25.0, 29.4, "so the plan is")]).unwrap();
    let fake = FakeTranscriber::default();
    let name = leave_chunk(&env.meeting, Track::Others, span(1, 28.0, 58.0));
    fake.answer(&name, Ok(vec![w("plan", 0.6, 0.9), w("is", 1.0, 1.3), w("to", 1.6, 1.8), w("ship", 1.9, 2.3)]));

    retry_failed_chunks(&env.store, &env.meeting.id, &fake, &PipelineConfig::default(), &env.spend).await;

    assert_eq!(
        env.store.transcript(&env.meeting).unwrap(),
        vec![seg(Track::Others, 25.0, 29.4, "so the plan is"), seg(Track::Others, 29.6, 30.3, "to ship")]
    );
}

#[tokio::test]
async fn recovered_you_words_that_echo_others_are_dropped() {
    let env = env();
    env.store.append_segments(&env.meeting, &[seg(Track::Others, 10.0, 12.0, "we should ship friday")]).unwrap();
    let fake = FakeTranscriber::default();
    let name = leave_chunk(&env.meeting, Track::You, span(0, 0.0, 30.0));
    fake.answer(
        &name,
        Ok(vec![w("we", 10.0, 10.4), w("should", 10.5, 11.0), w("ship", 11.0, 11.5), w("friday", 11.5, 12.0), w("Okay.", 20.0, 20.5)]),
    );

    retry_failed_chunks(&env.store, &env.meeting.id, &fake, &PipelineConfig::default(), &env.spend).await;

    assert_eq!(
        env.store.transcript(&env.meeting).unwrap(),
        vec![seg(Track::Others, 10.0, 12.0, "we should ship friday"), seg(Track::You, 20.0, 20.5, "Okay.")]
    );
}

#[tokio::test]
async fn a_chunk_that_fails_again_stays_and_the_meeting_stays_incomplete() {
    let env = env();
    let fake = FakeTranscriber::default();
    let good = leave_chunk(&env.meeting, Track::Others, span(0, 0.0, 30.0));
    let bad = leave_chunk(&env.meeting, Track::Others, span(1, 28.0, 58.0));
    fake.answer(&good, Ok(vec![w("Hi.", 1.0, 1.5)]));

    let report = retry_failed_chunks(&env.store, &env.meeting.id, &fake, &PipelineConfig::default(), &env.spend).await;

    assert_eq!(report, RetryReport { recovered: 1, remaining: 1, error: Some("offline".into()) });
    assert!(env.meeting.chunks_dir().join(&bad).exists());
    assert!(!env.meeting.chunks_dir().join(&good).exists());
    assert_eq!(env.store.get(&env.meeting.id).unwrap().status, MeetingStatus::Incomplete);
    assert_eq!(env.store.transcript(&env.meeting).unwrap(), vec![seg(Track::Others, 1.0, 1.5, "Hi.")]);
}

#[tokio::test]
async fn a_zdr_violation_stops_the_retry_and_keeps_every_remaining_chunk() {
    let env = env();
    let fake = FakeTranscriber::default();
    let first = leave_chunk(&env.meeting, Track::Others, span(0, 0.0, 30.0));
    leave_chunk(&env.meeting, Track::Others, span(1, 28.0, 58.0));
    fake.answer(&first, Err(TranscribeError::ZdrViolation("pick another model".into())));

    let report = retry_failed_chunks(&env.store, &env.meeting.id, &fake, &PipelineConfig::default(), &env.spend).await;

    assert_eq!(report, RetryReport { recovered: 0, remaining: 2, error: Some("pick another model".into()) });
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read_dir(env.meeting.chunks_dir()).unwrap().count(), 2);
}

#[tokio::test]
async fn chunks_from_older_versions_cannot_be_placed_and_are_counted_as_remaining() {
    let env = env();
    std::fs::write(env.meeting.chunks_dir().join("you-00003.ogg"), b"audio").unwrap();
    let fake = FakeTranscriber::default();

    let report = retry_failed_chunks(&env.store, &env.meeting.id, &fake, &PipelineConfig::default(), &env.spend).await;

    assert_eq!(report.recovered, 0);
    assert_eq!(report.remaining, 1);
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn each_charged_retry_is_recorded_against_the_meeting_even_when_it_fails() {
    let env = env();
    let fake = FakeTranscriber { cost: Some(0.0003), ..Default::default() };
    let recovered = leave_chunk(&env.meeting, Track::Others, span(0, 0.0, 30.0));
    let rejected = leave_chunk(&env.meeting, Track::You, span(0, 10.0, 20.0));
    fake.answer(&recovered, Ok(vec![w("Hello.", 1.0, 1.5)]));
    fake.answer(&rejected, Err(TranscribeError::Rejected("invalid audio".into())));

    retry_failed_chunks(&env.store, &env.meeting.id, &fake, &PipelineConfig::default(), &env.spend).await;

    let entries = env.spend.entries();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.kind == SpendKind::Transcription && e.meeting_id.as_deref() == Some(env.meeting.id.as_str())));
    assert_eq!(entries.iter().map(|e| e.usage.seconds).collect::<Vec<_>>(), [Some(30.0), Some(10.0)]);
}

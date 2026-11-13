use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::DateTime;
use tiro_core::embeddings::{embed_meeting, load_embedded, EmbedError, Embedder, MeaningIndex};
use tiro_core::search::{index_stale, search, search_by_meaning, Fragment, Meaning, MeaningSearch, Place};
use tiro_core::spend::{Billed, SpendKind, SpendLedger, Usage};
use tiro_core::store::{Meeting, MeetingStatus, MeetingStore};
use tiro_core::transcript::{Segment, Track};

/// Vectors with one axis per topic, so texts about the same topic point the same way whatever their words.
const TOPICS: [&[&str]; 3] = [
    &["cost", "costs", "spend", "token", "tokens", "price", "pricing", "money", "bill", "ai"],
    &["hiring", "interview", "interviews", "candidate", "candidates", "recruiting"],
    &["wolverine", "ticket", "813"],
];

fn vector(text: &str) -> Vec<f32> {
    let words: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).collect();
    let mut v: Vec<f32> = TOPICS.iter().map(|topic| words.iter().filter(|w| topic.contains(&w.as_str())).count() as f32).collect();
    // Off every topic a little, so no text is the zero vector.
    v.push(0.1);
    v
}

/// Embeds by topic, recording each request's texts; fails every request when `failing`.
struct FakeEmbedder {
    model: String,
    requests: Arc<Mutex<Vec<Vec<String>>>>,
    failing: Option<EmbedError>,
}

impl FakeEmbedder {
    fn new(model: &str) -> Self {
        Self { model: model.into(), requests: Arc::default(), failing: None }
    }

    fn failing(model: &str, error: EmbedError) -> Self {
        Self { failing: Some(error), ..Self::new(model) }
    }

    fn texts(&self) -> Vec<String> {
        self.requests.lock().unwrap().iter().flatten().cloned().collect()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl Embedder for FakeEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    async fn embed(&self, texts: &[String]) -> Billed<Vec<Vec<f32>>, EmbedError> {
        self.requests.lock().unwrap().push(texts.to_vec());
        let usage = Usage {
            model: self.model.clone(),
            cost: 0.00001,
            estimated: false,
            prompt_tokens: Some(texts.len() as u64 * 10),
            completion_tokens: None,
            seconds: None,
            generation_id: None,
        };
        let result = match &self.failing {
            Some(error) => Err(error.clone()),
            None => Ok(texts.iter().map(|t| vector(t)).collect()),
        };
        Billed { result, usage: Some(usage) }
    }
}

fn seg(start: f64, end: f64, text: &str) -> Segment {
    Segment { track: Track::Others, start, end, text: text.into(), speaker: None }
}

struct Env {
    dir: tempfile::TempDir,
    store: MeetingStore,
    spend: SpendLedger,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap();
    let spend = SpendLedger::new(dir.path().join("spend.jsonl"));
    Env { dir, store, spend }
}

/// A stopped Meeting titled `title` with this Transcript, started `minute` minutes past 14:00.
fn finished(store: &MeetingStore, minute: u32, title: &str, segments: &[Segment]) -> Meeting {
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339(&format!("2026-10-17T14:{minute:02}:00-04:00")).unwrap()).unwrap();
    store.append_segments(&meeting, segments).unwrap();
    store.set_title(&meeting.id, title).unwrap();
    store.set_status(&meeting.id, MeetingStatus::Complete, None).unwrap();
    store.get(&meeting.id).unwrap()
}

async fn found_by_meaning(env: &Env, index: &MeaningIndex, embedder: &FakeEmbedder, query: &str) -> Vec<String> {
    let meaning = Meaning { index, embedder, spend: &env.spend };
    let results = search_by_meaning(&env.store, query, &meaning).await.unwrap();
    assert_eq!(results.meaning, MeaningSearch::On);
    results.meetings.unwrap().into_iter().map(|r| r.meeting_id).collect()
}

#[tokio::test]
async fn every_passage_of_a_meeting_is_embedded_and_charged_to_it() {
    let env = env();
    let meeting = finished(&env.store, 0, "Weekly sync", &[seg(0.0, 5.0, "We reviewed the token spend.")]);
    env.store.write_notes(&meeting, "first\n\nsecond").unwrap();
    env.store.index_passages(&meeting.id).unwrap();
    let embedder = FakeEmbedder::new("openai/text-embedding-3-small");

    embed_meeting(&env.store, &MeaningIndex::default(), &meeting.id, &embedder, &env.spend).await.unwrap();

    let mut texts = embedder.texts();
    texts.sort();
    assert_eq!(texts, ["We reviewed the token spend.", "Weekly sync", "first", "second"]);
    assert_eq!(embedder.request_count(), 1);
    let entries = env.spend.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!((entries[0].kind, entries[0].meeting_id.as_deref()), (SpendKind::Embedding, Some(meeting.id.as_str())));
}

#[tokio::test]
async fn a_summary_section_is_embedded_with_its_heading() {
    let env = env();
    let meeting = finished(&env.store, 0, "T", &[]);
    let summary = tiro_core::summary::Summary { suggested_title: None, markdown: "## Pricing\nPer seat.\n".into() };
    env.store.write_summary(&meeting, &summary).unwrap();
    let embedder = FakeEmbedder::new("m");

    embed_meeting(&env.store, &MeaningIndex::default(), &meeting.id, &embedder, &env.spend).await.unwrap();

    assert!(embedder.texts().contains(&"Pricing\nPer seat.".to_owned()), "{:?}", embedder.texts());
}

#[tokio::test]
async fn a_rebuilt_index_reuses_the_stored_vectors_without_any_request() {
    let env = env();
    let meeting = finished(&env.store, 0, "Weekly sync", &[seg(0.0, 5.0, "We reviewed the token spend.")]);
    embed_meeting(&env.store, &MeaningIndex::default(), &meeting.id, &FakeEmbedder::new("m"), &env.spend).await.unwrap();

    let store = MeetingStore::open(&env.dir.path().join("meetings"), &env.dir.path().join("fresh.sqlite")).unwrap();
    store.rebuild_index().unwrap();
    index_stale(&store, |_| {}).unwrap();
    let index = MeaningIndex::default();
    let embedder = FakeEmbedder::new("m");
    embed_meeting(&store, &index, &meeting.id, &embedder, &env.spend).await.unwrap();

    assert_eq!(embedder.request_count(), 0);
    let env = Env { store, ..env };
    // Only the query is embedded.
    assert_eq!(found_by_meaning(&env, &index, &embedder, "cost of ai for customers").await, vec![meeting.id]);
    assert_eq!(embedder.texts(), ["cost of ai for customers"]);
}

#[tokio::test]
async fn editing_one_notes_paragraph_embeds_only_that_paragraph_again() {
    let env = env();
    let meeting = finished(&env.store, 0, "T", &[]);
    env.store.write_notes(&meeting, "keep this\n\nchange this").unwrap();
    env.store.index_passages(&meeting.id).unwrap();
    let index = MeaningIndex::default();
    embed_meeting(&env.store, &index, &meeting.id, &FakeEmbedder::new("m"), &env.spend).await.unwrap();

    env.store.write_notes(&meeting, "keep this\n\nchanged").unwrap();
    env.store.index_passages(&meeting.id).unwrap();
    let embedder = FakeEmbedder::new("m");
    embed_meeting(&env.store, &index, &meeting.id, &embedder, &env.spend).await.unwrap();

    assert_eq!(embedder.texts(), ["changed"]);
}

#[tokio::test]
async fn a_new_embedding_model_embeds_everything_again_and_replaces_the_old_vectors() {
    let env = env();
    let meeting = finished(&env.store, 0, "T", &[seg(0.0, 5.0, "hello")]);
    let index = MeaningIndex::default();
    embed_meeting(&env.store, &index, &meeting.id, &FakeEmbedder::new("old"), &env.spend).await.unwrap();

    let new = FakeEmbedder::new("new");
    embed_meeting(&env.store, &index, &meeting.id, &new, &env.spend).await.unwrap();
    assert_eq!(new.texts().len(), 2);

    // The old model's vectors went when the new model's were saved.
    let old = FakeEmbedder::new("old");
    embed_meeting(&env.store, &index, &meeting.id, &old, &env.spend).await.unwrap();
    assert_eq!(old.texts().len(), 2);
}

#[tokio::test]
async fn a_failed_request_is_returned_and_its_cost_still_recorded() {
    let env = env();
    let meeting = finished(&env.store, 0, "T", &[]);
    let embedder = FakeEmbedder::failing("m", EmbedError::ZdrViolation("m has no zero-data-retention endpoint".into()));

    let result = embed_meeting(&env.store, &MeaningIndex::default(), &meeting.id, &embedder, &env.spend).await;

    assert!(matches!(result, Err(EmbedError::ZdrViolation(_))), "{result:?}");
    assert_eq!(env.spend.entries().len(), 1);
}

#[tokio::test]
async fn a_meeting_still_transcribing_is_not_embedded() {
    let env = env();
    let meeting = env.store.create("Manual", DateTime::parse_from_rfc3339("2026-10-17T14:00:00-04:00").unwrap()).unwrap();
    env.store.set_status(&meeting.id, MeetingStatus::Transcribing, None).unwrap();
    let embedder = FakeEmbedder::new("m");

    embed_meeting(&env.store, &MeaningIndex::default(), &meeting.id, &embedder, &env.spend).await.unwrap();

    assert_eq!(embedder.request_count(), 0);
}

/// Two embedded Meetings: one about token spend, one about hiring.
async fn two_meetings(env: &Env, index: &MeaningIndex) -> (Meeting, Meeting) {
    let spend = finished(&env.store, 0, "Weekly sync", &[seg(0.0, 5.0, "We reviewed token spend per seat for Entergy. Ticket 813 is done.")]);
    let hiring = finished(&env.store, 30, "Staff meeting", &[seg(0.0, 5.0, "Two interviews for the hiring plan.")]);
    let embedder = FakeEmbedder::new("m");
    for meeting in [&spend, &hiring] {
        embed_meeting(&env.store, index, &meeting.id, &embedder, &env.spend).await.unwrap();
    }
    (spend, hiring)
}

#[tokio::test]
async fn a_query_worded_differently_finds_the_meeting_it_means_with_a_plain_snippet() {
    let env = env();
    let index = MeaningIndex::default();
    let (spend, _) = two_meetings(&env, &index).await;
    // No word in common.
    assert_eq!(search(&env.store, "cost of AI for customers").unwrap().meetings, Some(vec![]));

    let embedder = FakeEmbedder::new("m");
    let meaning = Meaning { index: &index, embedder: &embedder, spend: &env.spend };
    let results = search_by_meaning(&env.store, "cost of AI for customers", &meaning).await.unwrap();

    let meetings = results.meetings.unwrap();
    assert_eq!(meetings.iter().map(|r| r.meeting_id.as_str()).collect::<Vec<_>>(), [spend.id.as_str()]);
    let hit = &meetings[0].hits[0];
    assert_eq!(hit.place, Place::Transcript { ms: 0 });
    assert_eq!(hit.snippet, vec![Fragment { text: "We reviewed token spend per seat for Entergy. Ticket 813 is done.".into(), matched: false }]);
}

#[tokio::test]
async fn a_rare_keyword_still_ranks_its_meeting_first() {
    let env = env();
    let index = MeaningIndex::default();
    let (spend, _) = two_meetings(&env, &index).await;

    // A name off every topic: its meaning matches the Meetings' titles as well as anything.
    let found = found_by_meaning(&env, &index, &FakeEmbedder::new("m"), "Entergy").await;

    assert_eq!(found.first(), Some(&spend.id));
}

#[tokio::test]
async fn tags_filter_meaning_matches_too() {
    let env = env();
    let index = MeaningIndex::default();
    let (spend, hiring) = two_meetings(&env, &index).await;
    env.store.set_tags(&hiring.id, &["People".to_owned()]).unwrap();

    assert!(found_by_meaning(&env, &index, &FakeEmbedder::new("m"), "pricing tag:People").await.is_empty());
    assert_eq!(found_by_meaning(&env, &index, &FakeEmbedder::new("m"), "pricing").await, vec![spend.id]);
}

#[tokio::test]
async fn a_query_without_a_letter_matches_keywords_only_without_a_request() {
    let env = env();
    let index = MeaningIndex::default();
    let (spend, _) = two_meetings(&env, &index).await;
    let embedder = FakeEmbedder::new("m");

    assert_eq!(found_by_meaning(&env, &index, &embedder, "813").await, vec![spend.id]);
    assert!(found_by_meaning(&env, &index, &embedder, "913").await.is_empty());
    assert_eq!(embedder.request_count(), 0);
}

#[tokio::test]
async fn a_passage_far_less_similar_than_the_best_is_no_match() {
    let env = env();
    let index = MeaningIndex::default();
    let (spend, _) = two_meetings(&env, &index).await;
    // Somewhat about cost, but mostly about something else.
    let weak = finished(&env.store, 45, "Planning", &[seg(0.0, 5.0, "The wolverine ticket 813 cost nothing.")]);
    let embedder = FakeEmbedder::new("m");
    embed_meeting(&env.store, &index, &weak.id, &embedder, &env.spend).await.unwrap();

    assert_eq!(found_by_meaning(&env, &index, &embedder, "pricing").await, vec![spend.id]);
}

#[tokio::test]
async fn a_query_is_embedded_once_however_often_it_is_searched_and_charged_to_no_meeting() {
    let env = env();
    let index = MeaningIndex::default();
    two_meetings(&env, &index).await;
    let before = env.spend.entries().len();
    let embedder = FakeEmbedder::new("m");

    for _ in 0..3 {
        found_by_meaning(&env, &index, &embedder, "pricing tag:People").await;
        found_by_meaning(&env, &index, &embedder, "  pricing ").await;
    }

    assert_eq!(embedder.texts(), ["pricing"]);
    let entries = env.spend.entries();
    assert_eq!(entries.len(), before + 1);
    assert_eq!((entries[before].kind, entries[before].meeting_id.as_deref()), (SpendKind::Embedding, None));
}

#[tokio::test]
async fn a_failed_query_embedding_falls_back_to_keywords_and_says_so() {
    let env = env();
    let index = MeaningIndex::default();
    let (spend, _) = two_meetings(&env, &index).await;
    let embedder = FakeEmbedder::failing("m", EmbedError::Failed("could not reach OpenRouter".into()));
    let meaning = Meaning { index: &index, embedder: &embedder, spend: &env.spend };

    let results = search_by_meaning(&env.store, "ticket 813", &meaning).await.unwrap();

    assert_eq!(results.meaning, MeaningSearch::Unavailable);
    assert_eq!(results.meetings.unwrap().iter().map(|r| r.meeting_id.clone()).collect::<Vec<_>>(), vec![spend.id]);
}

#[test]
fn keyword_search_alone_says_meaning_search_is_off() {
    let env = env();
    finished(&env.store, 0, "T", &[]);

    assert_eq!(search(&env.store, "anything").unwrap().meaning, MeaningSearch::Off);
}

#[tokio::test]
async fn a_deleted_meeting_is_no_longer_found_by_meaning() {
    let env = env();
    let index = MeaningIndex::default();
    let (spend, _) = two_meetings(&env, &index).await;

    env.store.delete(&spend.id).unwrap();
    index.remove(&spend.id);

    assert!(found_by_meaning(&env, &index, &FakeEmbedder::new("m"), "pricing").await.is_empty());
}

#[tokio::test]
async fn stored_vectors_load_without_any_request_and_say_which_meetings_need_embedding() {
    let env = env();
    let embedded = finished(&env.store, 0, "Weekly sync", &[seg(0.0, 5.0, "We reviewed the token spend.")]);
    let changed = finished(&env.store, 30, "Staff meeting", &[]);
    for meeting in [&embedded, &changed] {
        embed_meeting(&env.store, &MeaningIndex::default(), &meeting.id, &FakeEmbedder::new("m"), &env.spend).await.unwrap();
    }
    env.store.write_notes(&changed, "new paragraph").unwrap();
    env.store.index_passages(&changed.id).unwrap();

    let index = MeaningIndex::default();
    assert!(load_embedded(&env.store, &index, &embedded.id, "m").unwrap());
    assert!(!load_embedded(&env.store, &index, &changed.id, "m").unwrap());
    assert!(!load_embedded(&env.store, &MeaningIndex::default(), &embedded.id, "another model").unwrap());

    let embedder = FakeEmbedder::new("m");
    assert_eq!(found_by_meaning(&env, &index, &embedder, "pricing").await, vec![embedded.id]);
    assert_eq!(embedder.texts(), ["pricing"]);
}

#[tokio::test]
async fn passages_go_in_as_few_requests_as_fit_and_the_same_text_only_once() {
    let env = env();
    let meeting = finished(&env.store, 0, "T", &[]);
    let mut notes: Vec<String> = (0..150).map(|n| format!("paragraph {n}")).collect();
    notes.push("paragraph 0".into());
    env.store.write_notes(&meeting, &notes.join("\n\n")).unwrap();
    env.store.index_passages(&meeting.id).unwrap();
    let embedder = FakeEmbedder::new("m");

    embed_meeting(&env.store, &MeaningIndex::default(), &meeting.id, &embedder, &env.spend).await.unwrap();

    // The title and 150 distinct paragraphs, at most 96 a request.
    let sizes: Vec<usize> = embedder.requests.lock().unwrap().iter().map(Vec::len).collect();
    assert_eq!(sizes, [96, 55]);
}

#[tokio::test]
async fn before_anything_is_embedded_a_query_matches_keywords_only_and_says_so_without_a_request() {
    let env = env();
    let meeting = finished(&env.store, 0, "T", &[seg(0.0, 5.0, "Ticket 813")]);
    let embedder = FakeEmbedder::new("m");
    let meaning = Meaning { index: &MeaningIndex::default(), embedder: &embedder, spend: &env.spend };

    let results = search_by_meaning(&env.store, "ticket 813", &meaning).await.unwrap();

    assert_eq!(results.meaning, MeaningSearch::Unavailable);
    assert_eq!(results.meetings.unwrap()[0].meeting_id, meeting.id);
    assert_eq!(embedder.request_count(), 0);
}

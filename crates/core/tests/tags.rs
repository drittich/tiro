use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::DateTime;
use tiro_core::spend::{Billed, SpendKind, SpendLedger, Usage};
use tiro_core::store::{Meeting, MeetingStatus, MeetingStore};
use tiro_core::summary::{SummarizeError, Summarizer, Summary};
use tiro_core::tags::{build_prompt, clear_outdated_tags, parse_response, tag_meeting, vocabulary, TagError};

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|&s| s.to_owned()).collect()
}

#[test]
fn the_prompt_has_the_title_the_summary_the_vocabulary_and_the_rules() {
    let prompt = build_prompt("Q4 planning", "## TL;DR\nWe planned hiring.\n", &words(&["Hiring", "Product Roadmap"]));

    assert!(prompt.contains("Q4 planning"), "{prompt}");
    assert!(prompt.contains("We planned hiring."), "{prompt}");
    assert!(prompt.contains(r#"["Hiring","Product Roadmap"]"#), "{prompt}");
    assert!(prompt.contains(r#""suggested_new_tags""#) && prompt.contains(r#""confidence""#), "{prompt}");
    for rule in [
        "Only use names from the existing Tags",
        "which part of the work",
        "Name the area, not one aspect of it",
        "tag the customer by name",
        "\"Product Development\"",
        "\"AI-Assisted Development\"",
        "the organization's own product",
        "named only in passing gets no Tag",
        "never more than 5",
        "plain name",
        "0.0 to 1.0",
        "below 0.7",
        "at most 3 new Tags",
        "minor variants",
    ] {
        assert!(prompt.contains(rule), "missing {rule:?}: {prompt}");
    }
}

#[test]
fn confident_matches_keep_the_vocabularys_spelling_highest_confidence_first() {
    let vocabulary = words(&["Hiring", "Product Roadmap", "Budget"]);
    let reply = r#"{"tags": [
        {"name": "budget", "confidence": 0.7},
        {"name": "HIRING", "confidence": 0.95},
        {"name": "Product Roadmap", "confidence": 0.69},
        {"name": "Kubernetes", "confidence": 0.99}
    ], "suggested_new_tags": []}"#;

    assert_eq!(parse_response(reply, &vocabulary).unwrap(), words(&["Hiring", "Budget"]));
}

#[test]
fn up_to_three_new_tags_are_added_after_the_matches_and_one_already_in_the_vocabulary_becomes_that_tag() {
    let vocabulary = words(&["Hiring", "Product Roadmap"]);
    let reply = r#"{"tags": [{"name": "Hiring", "confidence": 0.8}],
        "suggested_new_tags": ["  Authentication ", "product roadmap", "Security", "Billing"]}"#;

    assert_eq!(
        parse_response(reply, &vocabulary).unwrap(),
        words(&["Hiring", "Authentication", "Product Roadmap", "Security"])
    );
}

#[test]
fn duplicates_are_dropped_ignoring_case_and_there_are_at_most_five() {
    let vocabulary: Vec<String> = (1..=12).map(|n| format!("Topic {n}")).collect();
    let tags: Vec<String> =
        (1..=12).map(|n| format!(r#"{{"name": "topic {n}", "confidence": {}}}"#, 1.0 - n as f64 * 0.01)).collect();
    let reply = format!(r#"{{"tags": [{{"name": "TOPIC 1", "confidence": 0.9}}, {}], "suggested_new_tags": ["Topic 2"]}}"#, tags.join(","));

    let parsed = parse_response(&reply, &vocabulary).unwrap();

    assert_eq!(parsed, vocabulary[..5].to_vec());
}

#[test]
fn a_reply_in_a_json_fence_is_read_and_one_that_isnt_json_fails() {
    let vocabulary = words(&["Hiring"]);
    let fenced = "```json\n{\"tags\": [{\"name\": \"Hiring\", \"confidence\": 0.9}]}\n```";
    assert_eq!(parse_response(fenced, &vocabulary).unwrap(), words(&["Hiring"]));

    assert!(parse_response("Hiring, Budget", &vocabulary).is_err());
}

fn tagged(id: &str, tags: Option<&[&str]>) -> Meeting {
    Meeting {
        id: id.into(),
        dir: id.into(),
        title: id.into(),
        app: "Manual".into(),
        started_at: DateTime::parse_from_rfc3339("2026-10-17T14:05:00-04:00").unwrap(),
        ended_at: None,
        status: MeetingStatus::Complete,
        tags: tags.map(words),
    }
}

#[test]
fn the_vocabulary_is_every_tag_in_use_most_used_first_then_alphabetical() {
    let meetings = [
        tagged("a", Some(&["Hiring", "Budget"])),
        tagged("b", Some(&["budget", "Authentication"])),
        tagged("c", None),
        tagged("d", Some(&[])),
        tagged("e", Some(&["Budget", "Hiring", "Security"])),
    ];

    assert_eq!(vocabulary(&meetings), words(&["Budget", "Hiring", "Authentication", "Security"]));
}

#[test]
fn the_prompt_lists_only_the_100_most_used_tags() {
    let vocabulary: Vec<String> = (1..=150).map(|n| format!("Topic {n:03}")).collect();
    let prompt = build_prompt("T", "S", &vocabulary);
    assert!(prompt.contains("\"Topic 100\""), "{prompt}");
    assert!(!prompt.contains("\"Topic 101\""), "{prompt}");
}

/// Records the prompt and answers with a fixed reply.
struct FakeSummarizer {
    reply: Result<String, SummarizeError>,
    prompts: Arc<Mutex<Vec<String>>>,
    usage: Option<Usage>,
}

impl FakeSummarizer {
    fn replying(reply: Result<&str, SummarizeError>) -> Self {
        Self { reply: reply.map(str::to_owned), prompts: Arc::default(), usage: None }
    }

    fn charging(self, cost: f64) -> Self {
        let usage = Usage {
            model: "deepseek/deepseek-v4.1-flash".into(),
            cost,
            estimated: false,
            prompt_tokens: Some(9000),
            completion_tokens: Some(2000),
            seconds: None,
            generation_id: None,
        };
        Self { usage: Some(usage), ..self }
    }
}

#[async_trait]
impl Summarizer for FakeSummarizer {
    async fn summarize(&self, prompt: &str) -> Billed<String, SummarizeError> {
        self.prompts.lock().unwrap().push(prompt.to_owned());
        Billed { result: self.reply.clone(), usage: self.usage.clone() }
    }
}

fn ledger(dir: &tempfile::TempDir) -> SpendLedger {
    SpendLedger::new(dir.path().join("spend.jsonl"))
}

fn store() -> (tempfile::TempDir, MeetingStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap();
    (dir, store)
}

fn summarized(store: &MeetingStore, start: &str, markdown: &str) -> Meeting {
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339(start).unwrap()).unwrap();
    store.write_summary(&meeting, &Summary { suggested_title: None, markdown: markdown.into() }).unwrap();
    meeting
}

#[tokio::test]
async fn tagging_a_meeting_sends_its_summary_and_the_other_meetings_tags_and_saves_the_result() {
    let (_dir, store) = store();
    let other = summarized(&store, "2026-10-16T09:00:00-04:00", "## TL;DR\nInterviews.\n");
    store.set_tags(&other.id, &words(&["Hiring"])).unwrap();
    let meeting = summarized(&store, "2026-10-17T09:00:00-04:00", "## TL;DR\nWe hired and talked login.\n");
    let fake = FakeSummarizer::replying(Ok(
        r#"{"tags": [{"name": "hiring", "confidence": 0.9}], "suggested_new_tags": ["Authentication"]}"#,
    ));

    let tags = tag_meeting(&store, &meeting.id, &fake, &ledger(&_dir)).await.unwrap();

    assert_eq!(tags, words(&["Hiring", "Authentication"]));
    assert_eq!(store.get(&meeting.id).unwrap().tags, Some(tags));
    let prompt = &fake.prompts.lock().unwrap()[0];
    assert!(prompt.contains("We hired and talked login.") && prompt.contains(r#"["Hiring"]"#), "{prompt}");
}

#[tokio::test]
async fn a_failed_tagging_request_leaves_the_meeting_untagged() {
    let (_dir, store) = store();
    let meeting = summarized(&store, "2026-10-17T09:00:00-04:00", "## TL;DR\nOk.\n");

    let network = FakeSummarizer::replying(Err(SummarizeError::Failed("could not reach OpenRouter".into())));
    assert!(matches!(tag_meeting(&store, &meeting.id, &network, &ledger(&_dir)).await, Err(TagError::Request(_))));
    let garbled = FakeSummarizer::replying(Ok("Sorry, I can't."));
    assert!(matches!(tag_meeting(&store, &meeting.id, &garbled, &ledger(&_dir)).await, Err(TagError::Reply(_))));

    assert_eq!(store.get(&meeting.id).unwrap().tags, None);
    assert!(meeting.dir.join("summary.md").is_file());
}

#[tokio::test]
async fn a_meeting_without_a_summary_is_not_tagged() {
    let (_dir, store) = store();
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339("2026-10-17T09:00:00-04:00").unwrap()).unwrap();
    let fake = FakeSummarizer::replying(Ok(r#"{"tags": [], "suggested_new_tags": ["Hiring"]}"#));

    assert!(matches!(tag_meeting(&store, &meeting.id, &fake, &ledger(&_dir)).await, Err(TagError::NoSummary)));
    assert!(fake.prompts.lock().unwrap().is_empty());
    assert_eq!(store.get(&meeting.id).unwrap().tags, None);
}

#[test]
fn outdated_tags_are_cleared_once_so_every_meeting_is_tagged_again() {
    let (dir, store) = store();
    let tagged = summarized(&store, "2026-10-16T09:00:00-04:00", "## TL;DR\nInterviews.\n");
    store.set_tags(&tagged.id, &words(&["Product Development"])).unwrap();
    let nothing_fit = summarized(&store, "2026-10-17T09:00:00-04:00", "## TL;DR\nOk.\n");
    store.set_tags(&nothing_fit.id, &[]).unwrap();

    assert!(clear_outdated_tags(&store, dir.path()).unwrap());
    assert_eq!(store.get(&tagged.id).unwrap().tags, None);
    assert_eq!(store.get(&nothing_fit.id).unwrap().tags, None);
    assert!(store.list().unwrap().iter().all(|m| m.tags.is_none()));

    store.set_tags(&tagged.id, &words(&["Hiring"])).unwrap();
    assert!(!clear_outdated_tags(&store, dir.path()).unwrap());
    assert_eq!(store.get(&tagged.id).unwrap().tags, Some(words(&["Hiring"])));
}

#[tokio::test]
async fn tagging_records_what_the_tags_cost_against_the_meeting_even_when_the_reply_is_garbled() {
    let (dir, store) = store();
    let meeting = summarized(&store, "2026-10-17T09:00:00-04:00", "## TL;DR\nOk.\n");
    let fine = FakeSummarizer::replying(Ok(r#"{"tags": [], "suggested_new_tags": ["Hiring"]}"#)).charging(0.01);
    let garbled = FakeSummarizer::replying(Ok("Sorry, I can't.")).charging(0.02);

    tag_meeting(&store, &meeting.id, &fine, &ledger(&dir)).await.unwrap();
    tag_meeting(&store, &meeting.id, &garbled, &ledger(&dir)).await.unwrap_err();

    let entries = ledger(&dir).entries();
    assert_eq!(entries.iter().map(|e| e.usage.cost).collect::<Vec<_>>(), [0.01, 0.02]);
    assert!(entries.iter().all(|e| e.kind == SpendKind::Tags && e.meeting_id.as_deref() == Some(meeting.id.as_str())));
}

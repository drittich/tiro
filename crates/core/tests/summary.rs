use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset};
use tiro_core::spend::{Billed, SpendKind, SpendLedger, Usage};
use tiro_core::store::MeetingStore;
use tiro_core::summary::{
    build_prompt, parse_response, summarize_meeting, Summarizer, SummarizeError, Summary, SummaryInput, SummarySettings,
    DEFAULT_PROMPT_TEMPLATE,
};
use tiro_core::transcript::{Segment, Track};

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn seg(track: Track, start: f64, end: f64, text: &str) -> Segment {
    Segment { track, start, end, text: text.into(), speaker: None }
}

fn input<'a>(transcript: &'a [Segment], notes: &'a str) -> SummaryInput<'a> {
    SummaryInput {
        title: "Manual — 2026-10-17 14:05",
        started_at: at("2026-10-17T14:05:00-04:00"),
        display_name: "Darcy",
        transcript,
        notes,
    }
}

#[test]
fn the_default_prompt_asks_for_every_section_in_order_and_never_to_drop_exploratory_discussion() {
    let transcript = [seg(Track::Others, 1.0, 2.0, "Hi.")];
    let prompt = build_prompt(DEFAULT_PROMPT_TEMPLATE, &input(&transcript, ""));

    let order = ["Title:", "## TL;DR", "## Discussion", "## Key points", "## Decisions", "## Action items", "## Open questions"];
    let positions: Vec<usize> = order.iter().map(|s| prompt.find(s).unwrap_or_else(|| panic!("missing {s}"))).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "sections out of order: {positions:?}");
    assert!(prompt.contains("Never drop exploratory discussion"), "{prompt}");
    assert!(!prompt.contains("{{"), "unfilled placeholder: {prompt}");
}

#[test]
fn the_transcript_is_timestamped_and_you_is_the_display_name() {
    let transcript = [
        seg(Track::Others, 1.5, 3.0, "Shall we start?"),
        seg(Track::You, 4.0, 5.0, "Sure."),
        Segment { track: Track::Others, start: 3725.0, end: 3727.0, text: "Thanks all.".into(), speaker: Some(2) },
    ];
    let prompt = build_prompt(DEFAULT_PROMPT_TEMPLATE, &input(&transcript, ""));

    assert!(prompt.contains("[00:01] Others: Shall we start?\n[00:04] Darcy: Sure.\n[1:02:05] Others (Speaker 2): Thanks all."), "{prompt}");
    assert!(prompt.contains("\"Darcy\" is the user"), "explains who Darcy is: {prompt}");
}

#[test]
fn without_a_display_name_the_user_is_called_you() {
    let transcript = [seg(Track::You, 4.0, 5.0, "Sure.")];
    let mut input = input(&transcript, "");
    input.display_name = "  ";
    let prompt = build_prompt(DEFAULT_PROMPT_TEMPLATE, &input);
    assert!(prompt.contains("[00:04] You: Sure."), "{prompt}");
}

#[test]
fn the_prompt_says_how_long_the_meeting_was_and_scales_the_target_length_with_it() {
    let short: Vec<Segment> = (0..10).map(|i| seg(Track::Others, i as f64 * 10.0, i as f64 * 10.0 + 9.0, "a few words here")).collect();
    let long: Vec<Segment> =
        (0..600).map(|i| seg(Track::Others, i as f64 * 6.0, i as f64 * 6.0 + 5.0, &"word ".repeat(15))).collect();

    let short_prompt = build_prompt(DEFAULT_PROMPT_TEMPLATE, &input(&short, ""));
    let long_prompt = build_prompt(DEFAULT_PROMPT_TEMPLATE, &input(&long, ""));

    assert!(short_prompt.contains("about 2 minutes"), "{short_prompt}");
    assert!(long_prompt.contains("about 60 minutes"), "{long_prompt}");
    let target = |p: &str| -> usize {
        let i = p.find("Aim for about ").expect("a target length") + "Aim for about ".len();
        p[i..].split_whitespace().next().unwrap().parse().unwrap()
    };
    assert_eq!(target(&short_prompt), 150);
    assert_eq!(target(&long_prompt), 1350);
}

#[test]
fn notes_go_in_as_the_users_own_notes_which_take_priority() {
    let transcript = [seg(Track::Others, 1.0, 2.0, "Hi.")];

    let with = build_prompt(DEFAULT_PROMPT_TEMPLATE, &input(&transcript, "- Ask Sam about the budget\n"));
    assert!(with.contains("the user's own notes, which take priority over the Transcript"), "{with}");
    assert!(with.contains("- Ask Sam about the budget"), "{with}");

    let without = build_prompt(DEFAULT_PROMPT_TEMPLATE, &input(&transcript, "  \n"));
    assert!(!without.contains("take priority over the Transcript:"), "{without}");
    assert!(without.contains("The user took no Notes"), "{without}");
}

#[test]
fn a_custom_template_fills_its_placeholders_and_still_gets_the_transcript() {
    let transcript = [seg(Track::You, 4.0, 5.0, "Sure.")];
    let prompt = build_prompt("Summarize {{title}} for {{display_name}}.", &input(&transcript, ""));
    assert!(prompt.starts_with("Summarize Manual — 2026-10-17 14:05 for Darcy."), "{prompt}");
    assert!(prompt.ends_with("[00:04] Darcy: Sure.\n"), "{prompt}");
}

#[test]
fn the_reply_is_split_into_the_suggested_title_and_the_summary() {
    let reply = "Title: Pricing page brainstorm\n\n## TL;DR\nWe talked.\n\n## Discussion\n### Layout\nIdeas.\n";
    assert_eq!(
        parse_response(reply),
        Summary {
            suggested_title: Some("Pricing page brainstorm".into()),
            markdown: "## TL;DR\nWe talked.\n\n## Discussion\n### Layout\nIdeas.\n".into(),
        }
    );
}

#[test]
fn a_decorated_title_line_inside_a_code_fence_after_a_think_block_is_still_found() {
    let reply = "<think>Let me see.\nTitle: wrong</think>\n```markdown\n**Title:** \"Q4 hiring plan\"\n\n## TL;DR\nHire two.\n```\n";
    assert_eq!(
        parse_response(reply),
        Summary { suggested_title: Some("Q4 hiring plan".into()), markdown: "## TL;DR\nHire two.\n".into() }
    );
}

#[test]
fn a_reply_without_a_title_line_is_all_summary() {
    let reply = "\n## TL;DR\nShort one.";
    assert_eq!(parse_response(reply), Summary { suggested_title: None, markdown: "## TL;DR\nShort one.\n".into() });
}

/// Records the prompt and answers with a fixed reply.
#[derive(Clone)]
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

fn settings() -> SummarySettings {
    SummarySettings { template: DEFAULT_PROMPT_TEMPLATE.into(), display_name: "Darcy".into() }
}

fn ledger(dir: &tempfile::TempDir) -> SpendLedger {
    SpendLedger::new(dir.path().join("spend.jsonl"))
}

fn store() -> (tempfile::TempDir, MeetingStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap();
    (dir, store)
}

#[tokio::test]
async fn summarizing_a_meeting_writes_summary_md_and_titles_the_meeting() {
    let (_dir, store) = store();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    store.append_segments(&meeting, &[seg(Track::You, 1.0, 2.0, "Let's brainstorm names.")]).unwrap();
    let fake = FakeSummarizer::replying(Ok("Title: Naming brainstorm\n\n## TL;DR\nNames."));

    let summary = summarize_meeting(&store, &meeting.id, &fake, &settings(), &ledger(&_dir)).await.unwrap();

    let expected = Summary { suggested_title: Some("Naming brainstorm".into()), markdown: "## TL;DR\nNames.\n".into() };
    assert_eq!(summary, expected);
    assert_eq!(std::fs::read_to_string(meeting.dir.join("summary.md")).unwrap(), "## TL;DR\nNames.\n");
    assert_eq!(store.get(&meeting.id).unwrap().title, "Naming brainstorm");
    assert_eq!(store.summary(&meeting).unwrap(), Some(Summary { suggested_title: None, ..expected }));

    let prompt = &fake.prompts.lock().unwrap()[0];
    assert!(prompt.contains("[00:01] Darcy: Let's brainstorm names."), "{prompt}");
    assert!(prompt.contains("Meeting: Manual — 2026-10-17 14:05"), "{prompt}");
}

#[test]
fn the_prompt_leaves_out_fillers_tags_and_segments_with_nothing_else() {
    let transcript = [
        seg(Track::Others, 1.0, 2.0, "Uh, the numbers are in. [laughs]"),
        seg(Track::You, 3.0, 4.0, "[clicking]"),
        seg(Track::You, 5.0, 6.0, "Um."),
    ];
    let prompt = build_prompt("{{length}}\n{{transcript}}", &input(&transcript, ""));
    assert_eq!(prompt, "150\n[00:01] Others: The numbers are in.\n");
}

#[test]
fn the_target_length_counts_only_the_words_left_after_cleaning() {
    // 1,000 real words and 1,000 fillers: 15% of the real words is 150, of all of them 300.
    let text = "word um ".repeat(1000);
    let transcript = [seg(Track::Others, 1.0, 2.0, &text)];
    let prompt = build_prompt("{{length}}", &input(&transcript, ""));
    assert!(prompt.starts_with("150"), "{prompt}");
}

#[tokio::test]
async fn notes_md_goes_into_the_prompt() {
    let (_dir, store) = store();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    store.append_segments(&meeting, &[seg(Track::Others, 1.0, 2.0, "Hello.")]).unwrap();
    std::fs::write(meeting.dir.join("notes.md"), "Budget is capped at 10k.\n").unwrap();
    let fake = FakeSummarizer::replying(Ok("Title: x\n\n## TL;DR\ny"));

    summarize_meeting(&store, &meeting.id, &fake, &settings(), &ledger(&_dir)).await.unwrap();

    assert!(fake.prompts.lock().unwrap()[0].contains("take priority over the Transcript:\n\nBudget is capped at 10k."));
}

#[tokio::test]
async fn re_summarizing_replaces_the_previous_summary() {
    let (_dir, store) = store();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    store.append_segments(&meeting, &[seg(Track::Others, 1.0, 2.0, "Hello.")]).unwrap();
    summarize_meeting(&store, &meeting.id, &FakeSummarizer::replying(Ok("Title: One\n\n## TL;DR\nfirst")), &settings(), &ledger(&_dir)).await.unwrap();

    summarize_meeting(&store, &meeting.id, &FakeSummarizer::replying(Ok("## TL;DR\nsecond")), &settings(), &ledger(&_dir)).await.unwrap();

    assert_eq!(store.summary(&meeting).unwrap(), Some(Summary { suggested_title: None, markdown: "## TL;DR\nsecond\n".into() }));
}

#[tokio::test]
async fn an_empty_transcript_is_not_sent_to_the_model() {
    let (_dir, store) = store();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    let fake = FakeSummarizer::replying(Ok("unused"));

    let result = summarize_meeting(&store, &meeting.id, &fake, &settings(), &ledger(&_dir)).await;

    assert!(matches!(result, Err(SummarizeError::Failed(ref m)) if m.contains("nothing to summarize")), "{result:?}");
    assert!(fake.prompts.lock().unwrap().is_empty());
    assert_eq!(store.summary(&meeting).unwrap(), None);
}

#[tokio::test]
async fn a_failed_call_leaves_the_previous_summary_alone() {
    let (_dir, store) = store();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    store.append_segments(&meeting, &[seg(Track::Others, 1.0, 2.0, "Hello.")]).unwrap();
    summarize_meeting(&store, &meeting.id, &FakeSummarizer::replying(Ok("Title: One\n\n## TL;DR\nfirst")), &settings(), &ledger(&_dir)).await.unwrap();

    let failing = FakeSummarizer::replying(Err(SummarizeError::Failed("offline".into())));
    let result = summarize_meeting(&store, &meeting.id, &failing, &settings(), &ledger(&_dir)).await;

    assert_eq!(result, Err(SummarizeError::Failed("offline".into())));
    assert_eq!(store.summary(&meeting).unwrap().unwrap().markdown, "## TL;DR\nfirst\n");
}

#[tokio::test]
async fn summarizing_records_what_the_summary_cost_against_the_meeting() {
    let (dir, store) = store();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    store.append_segments(&meeting, &[seg(Track::Others, 1.0, 2.0, "Hello.")]).unwrap();
    let fake = FakeSummarizer::replying(Ok("Title: x\n\n## TL;DR\ny")).charging(0.12);

    summarize_meeting(&store, &meeting.id, &fake, &settings(), &ledger(&dir)).await.unwrap();

    let entries = ledger(&dir).entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].meeting_id.as_deref(), Some(meeting.id.as_str()));
    assert_eq!(entries[0].kind, SpendKind::Summary);
    assert_eq!(entries[0].usage.cost, 0.12);
}

#[tokio::test]
async fn a_charged_reply_that_fails_is_still_recorded() {
    let (dir, store) = store();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    store.append_segments(&meeting, &[seg(Track::Others, 1.0, 2.0, "Hello.")]).unwrap();
    let empty = FakeSummarizer::replying(Err(SummarizeError::Failed("returned an empty Summary".into()))).charging(0.04);

    let result = summarize_meeting(&store, &meeting.id, &empty, &settings(), &ledger(&dir)).await;

    assert!(result.is_err());
    assert_eq!(ledger(&dir).entries().iter().map(|e| e.usage.cost).collect::<Vec<_>>(), [0.04]);
}

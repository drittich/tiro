use tiro_core::openrouter::{OpenRouterSummarizer, SUMMARY_MAX_TOKENS};
use tiro_core::spend::Usage;
use tiro_core::summary::{SummarizeError, Summarizer};
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn summarizer(server: &MockServer) -> OpenRouterSummarizer {
    OpenRouterSummarizer::new(format!("{}/api/v1", server.uri()), "sk-or-test".into(), "deepseek/deepseek-v4.1-flash".into())
}

#[tokio::test]
async fn the_prompt_goes_to_chat_completions_with_a_generous_token_budget() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-or-test"))
        .and(body_partial_json(json!({
            "model": "deepseek/deepseek-v4.1-flash",
            "messages": [{ "role": "user", "content": "Summarize this." }],
            "max_tokens": SUMMARY_MAX_TOKENS,
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "Title: T\n\n## TL;DR\nOk.", "reasoning": "hmm" }, "finish_reason": "stop" }],
        })))
        .expect(1)
        .mount(&server)
        .await;

    let reply = summarizer(&server).summarize("Summarize this.").await.result.unwrap();

    assert_eq!(reply, "Title: T\n\n## TL;DR\nOk.");
    const { assert!(SUMMARY_MAX_TOKENS >= 16_000, "a reasoning model needs room to think") };
}

#[tokio::test]
async fn an_empty_reply_is_an_error_that_says_why() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "" }, "finish_reason": "length" }],
        })))
        .mount(&server)
        .await;

    let result = summarizer(&server).summarize("x").await.result;
    let Err(SummarizeError::Failed(message)) = result else { panic!("{result:?}") };
    assert!(message.contains("length"), "{message}");
}

#[tokio::test]
async fn a_zdr_violation_and_a_rejected_key_say_how_to_fix_them() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"error":{"message":"ZDR violation (guardrail): 1 endpoint excluded"}}"#))
        .mount(&server)
        .await;
    let result = summarizer(&server).summarize("x").await.result;
    let Err(SummarizeError::ZdrViolation(message)) = result else { panic!("{result:?}") };
    assert!(message.contains("deepseek/deepseek-v4.1-flash") && message.contains("Settings"), "{message}");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":{"message":"User not found."}}"#))
        .mount(&server)
        .await;
    let result = summarizer(&server).summarize("x").await.result;
    assert!(matches!(result, Err(SummarizeError::Unauthorized(_))), "{result:?}");
}

#[tokio::test]
async fn other_failures_carry_the_providers_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string(r#"{"error":{"message":"upstream overloaded"}}"#))
        .mount(&server)
        .await;
    let result = summarizer(&server).summarize("x").await.result;
    let Err(SummarizeError::Failed(message)) = result else { panic!("{result:?}") };
    assert!(message.contains("502") && message.contains("upstream overloaded"), "{message}");
}

#[tokio::test]
async fn a_summary_request_names_tiro_as_the_app() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header("x-openrouter-title", "Tiro"))
        .and(header("http-referer", "https://github.com/drittich/tiro"))
        .and(header("x-openrouter-app-visibility", "hidden"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "Ok." }, "finish_reason": "stop" }],
        })))
        .expect(1)
        .mount(&server)
        .await;

    summarizer(&server).summarize("x").await.result.unwrap();
}

#[tokio::test]
async fn a_summary_reports_its_tokens_and_what_openrouter_charged() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "gen-1",
            "choices": [{ "message": { "role": "assistant", "content": "Ok." }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 32, "completion_tokens": 37, "total_tokens": 69, "cost": 0.000029751 },
        })))
        .mount(&server)
        .await;

    let billed = summarizer(&server).summarize("x").await;

    assert_eq!(billed.result.as_deref(), Ok("Ok."));
    assert_eq!(
        billed.usage,
        Some(Usage {
            model: "deepseek/deepseek-v4.1-flash".into(),
            cost: 0.000029751,
            estimated: false,
            prompt_tokens: Some(32),
            completion_tokens: Some(37),
            seconds: None,
            generation_id: Some("gen-1".into()),
        })
    );
}

#[tokio::test]
async fn an_empty_summary_is_a_failure_that_was_still_charged() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "gen-2",
            "choices": [{ "message": { "role": "assistant", "content": "" }, "finish_reason": "length" }],
            "usage": { "prompt_tokens": 9000, "completion_tokens": 32000, "cost": 0.04 },
        })))
        .mount(&server)
        .await;

    let billed = summarizer(&server).summarize("x").await;

    assert!(billed.result.is_err());
    assert_eq!(billed.usage.map(|u| u.cost), Some(0.04));
}

#[tokio::test]
async fn a_failure_with_no_cost_reports_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string(r#"{"error":{"message":"boom"}}"#))
        .mount(&server)
        .await;

    assert_eq!(summarizer(&server).summarize("x").await.usage, None);
}

/// Runs only with TIRO_LIVE_TESTS=1. Reads the key from the key file exactly as the app does. Summarizes
/// the brainstorming-heavy fixture Transcript and checks that every explored idea and set-aside alternative
/// survives in Discussion (ADR-0006).
#[tokio::test]
async fn live_summary_of_a_brainstorm_keeps_every_explored_idea_in_discussion() {
    use tiro_core::openrouter::DEFAULT_BASE_URL;
    use tiro_core::store::MeetingStore;
    use tiro_core::summary::{summarize_meeting, SummarySettings, DEFAULT_PROMPT_TEMPLATE, DEFAULT_SUMMARY_MODEL};

    if std::env::var("TIRO_LIVE_TESTS").as_deref() != Ok("1") {
        return;
    }
    let key = tiro_core::keyfile::read_key(&tiro_core::keyfile::default_key_path()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap();
    let meeting = store.create("Manual", chrono::DateTime::parse_from_rfc3339("2026-10-17T14:05:00-04:00").unwrap()).unwrap();
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/brainstorm-transcript.jsonl");
    std::fs::copy(fixture, meeting.dir.join("transcript.jsonl")).unwrap();

    let summarizer = OpenRouterSummarizer::new(DEFAULT_BASE_URL.into(), key, DEFAULT_SUMMARY_MODEL.into());
    let settings = SummarySettings { template: DEFAULT_PROMPT_TEMPLATE.into(), display_name: "Darcy".into() };
    let spend = tiro_core::spend::SpendLedger::new(dir.path().join("spend.jsonl"));
    let summary = summarize_meeting(&store, &meeting.id, &summarizer, &settings, &spend).await.unwrap();
    println!("Suggested title: {:?}\n\n{}", summary.suggested_title, summary.markdown);

    assert!(summary.suggested_title.is_some(), "a suggested title");
    assert!(spend.entries()[0].usage.cost > 0.0, "OpenRouter reports what the Summary cost");
    let md = summary.markdown.to_lowercase();
    let section = |name: &str| {
        let heading = format!("## {name}");
        md.match_indices(&heading)
            .map(|(i, _)| i)
            .find(|&i| i == 0 || md[..i].ends_with('\n'))
            .unwrap_or_else(|| panic!("no {name} section:\n{md}"))
    };
    let (discussion_at, key_points_at) = (section("discussion"), section("key points"));
    assert!(section("tl;dr") < discussion_at && discussion_at < key_points_at, "sections out of order");
    for later in ["decisions", "action items", "open questions"] {
        assert!(section(later) > key_points_at, "{later} out of order");
    }
    let discussion = &md[discussion_at..key_points_at];
    // Every idea raised, including the half-formed and the set-aside ones.
    for idea in ["checklist", "video", "sandbox", "buddy", "lisbon", "vermont", "hackathon", "commit message", "template"] {
        assert!(discussion.contains(idea), "Discussion dropped {idea:?}:\n{discussion}");
    }
    assert!(md[section("action items")..].contains("darcy"), "the user's action item is owned by their display name");
}

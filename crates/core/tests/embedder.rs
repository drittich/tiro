use tiro_core::embeddings::{EmbedError, Embedder};
use tiro_core::openrouter::OpenRouterEmbedder;
use tiro_core::spend::Usage;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn embedder(server: &MockServer) -> OpenRouterEmbedder {
    OpenRouterEmbedder::new(format!("{}/api/v1", server.uri()), "sk-or-test".into(), "openai/text-embedding-3-small".into())
}

fn texts(list: &[&str]) -> Vec<String> {
    list.iter().map(|&s| s.to_owned()).collect()
}

#[tokio::test]
async fn texts_go_to_embeddings_in_one_request_and_come_back_in_order_with_their_cost() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/embeddings"))
        .and(header("authorization", "Bearer sk-or-test"))
        .and(body_partial_json(json!({ "model": "openai/text-embedding-3-small", "input": ["token spend", "hiring"] })))
        // As the live API answers (October 2026), but out of order.
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [
                { "object": "embedding", "embedding": [0.0, 1.0], "index": 1 },
                { "object": "embedding", "embedding": [1.0, 0.0], "index": 0 },
            ],
            "model": "text-embedding-3-small",
            "usage": { "prompt_tokens": 3, "total_tokens": 3, "cost": 6e-8 },
            "provider": "Azure",
            "id": "gen-emb-1",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let billed = embedder(&server).embed(&texts(&["token spend", "hiring"])).await;

    assert_eq!(billed.result.unwrap(), vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
    assert_eq!(
        billed.usage,
        Some(Usage {
            model: "openai/text-embedding-3-small".into(),
            cost: 6e-8,
            estimated: false,
            prompt_tokens: Some(3),
            completion_tokens: None,
            seconds: None,
            generation_id: Some("gen-emb-1".into()),
        })
    );
}

#[tokio::test]
async fn a_zdr_violation_names_the_embedding_model_setting() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"error":{"message":"ZDR violation (guardrail): 1 endpoint excluded"}}"#))
        .mount(&server)
        .await;

    let result = embedder(&server).embed(&texts(&["x"])).await.result;

    let Err(EmbedError::ZdrViolation(message)) = result else { panic!("{result:?}") };
    assert!(message.contains("openai/text-embedding-3-small") && message.contains("Embedding model"), "{message}");
}

#[tokio::test]
async fn a_rejected_key_and_other_failures_say_so() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":{"message":"User not found."}}"#))
        .mount(&server)
        .await;
    let result = embedder(&server).embed(&texts(&["x"])).await.result;
    assert!(matches!(result, Err(EmbedError::Unauthorized(_))), "{result:?}");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string(r#"{"error":{"message":"upstream overloaded"}}"#))
        .mount(&server)
        .await;
    let result = embedder(&server).embed(&texts(&["x"])).await.result;
    let Err(EmbedError::Failed(message)) = result else { panic!("{result:?}") };
    assert!(message.contains("502") && message.contains("upstream overloaded"), "{message}");
}

#[tokio::test]
async fn an_answer_missing_a_vector_is_a_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": [1.0], "index": 0 }],
            "usage": { "cost": 1e-8 },
        })))
        .mount(&server)
        .await;

    let billed = embedder(&server).embed(&texts(&["a", "b"])).await;

    assert!(matches!(billed.result, Err(EmbedError::Failed(_))), "{:?}", billed.result);
    assert_eq!(billed.usage.map(|u| u.cost), Some(1e-8));
}

/// Against the live API with the default Embedding model: costs a fraction of a cent. Run with
/// `TIRO_LIVE_TESTS=1 cargo test -p tiro-core --test embedder -- --nocapture`.
#[tokio::test]
async fn live_a_paraphrase_with_no_word_in_common_finds_the_meeting_it_means() {
    use chrono::DateTime;
    use tiro_core::embeddings::{embed_meeting, MeaningIndex, DEFAULT_EMBEDDING_MODEL};
    use tiro_core::openrouter::DEFAULT_BASE_URL;
    use tiro_core::search::{search, search_by_meaning, Meaning};
    use tiro_core::spend::SpendLedger;
    use tiro_core::store::{MeetingStatus, MeetingStore};
    use tiro_core::transcript::{Segment, Track};

    if std::env::var("TIRO_LIVE_TESTS").as_deref() != Ok("1") {
        return;
    }
    let key = tiro_core::keyfile::read_key(&tiro_core::keyfile::default_key_path()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap();
    let spend = SpendLedger::new(dir.path().join("spend.jsonl"));
    let embedder = OpenRouterEmbedder::new(DEFAULT_BASE_URL.into(), key, DEFAULT_EMBEDDING_MODEL.into());
    let index = MeaningIndex::default();
    let meetings = [
        ("Weekly sync", "We looked at the token bill from the model provider. Each seat burns about two dollars a day in completions, so the per-user pricing has to cover it."),
        ("Hiring loop", "Two candidates for the frontend role. The second interview went well and we'll make an offer on Friday."),
        ("Release planning", "The release branch is cut on Tuesday. QA signs off on the regression suite before we tag it."),
    ];
    let mut ids = Vec::new();
    for (minute, (title, text)) in meetings.iter().enumerate() {
        let start = DateTime::parse_from_rfc3339(&format!("2026-10-17T14:{:02}:00-04:00", minute * 10)).unwrap();
        let meeting = store.create("Manual", start).unwrap();
        store.append_segments(&meeting, &[Segment { track: Track::Others, start: 0.0, end: 20.0, text: (*text).into(), speaker: None }]).unwrap();
        store.set_title(&meeting.id, title).unwrap();
        store.set_status(&meeting.id, MeetingStatus::Complete, None).unwrap();
        embed_meeting(&store, &index, &meeting.id, &embedder, &spend).await.unwrap();
        ids.push(meeting.id);
    }
    let query = "cost of AI for customers";
    assert_eq!(search(&store, query).unwrap().meetings, Some(vec![]), "no word in common");

    let results = search_by_meaning(&store, query, &Meaning { index: &index, embedder: &embedder, spend: &spend }).await.unwrap();

    let found: Vec<(String, f64)> = results.meetings.unwrap().into_iter().map(|r| (r.meeting_id, r.score)).collect();
    println!("{found:?}");
    assert_eq!(found.first().map(|(id, _)| id), Some(&ids[0]));
    assert!(spend.entries().iter().all(|e| e.usage.cost > 0.0), "OpenRouter reports what each embedding cost");
}

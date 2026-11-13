use std::path::Path;

use base64::Engine;
use tiro_core::openrouter::OpenRouterTranscriber;
use tiro_core::spend::Usage;
use tiro_core::transcriber::{ChunkAudio, TranscribeError, Transcriber};
use tiro_core::transcript::Word;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ZDR_BODY: &str = r#"{"error":{"message":"0 endpoints out of 1 requested are available matching your guardrail restrictions and data policy. We removed them for the following reasons (an endpoint may have matched multiple reasons):\nZDR violation (guardrail): 1 endpoint excluded; configurable at https://openrouter.ai/workspaces/default/guardrails","code":404}}"#;

fn chunk(dir: &Path, name: &str, bytes: &[u8]) -> ChunkAudio {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    ChunkAudio { path, duration: 6.0 }
}

fn transcriber(server: &MockServer) -> OpenRouterTranscriber {
    OpenRouterTranscriber::new(format!("{}/api/v1", server.uri()), "sk-or-test".into(), "elevenlabs/scribe-v2".into())
}

fn is_verbose(req: &Request) -> bool {
    serde_json::from_slice::<serde_json::Value>(&req.body).unwrap()["response_format"] == "verbose_json"
}

#[tokio::test]
async fn a_chunk_is_sent_as_base64_ogg_asking_for_english_word_timestamps() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let audio = chunk(dir.path(), "you-3.ogg", b"OggS fake opus");

    Mock::given(method("POST"))
        .and(path("/api/v1/audio/transcriptions"))
        .and(header("authorization", "Bearer sk-or-test"))
        .and(body_partial_json(json!({
            "model": "elevenlabs/scribe-v2",
            "language": "en",
            "response_format": "verbose_json",
            "timestamp_granularities": ["word"],
            "input_audio": { "format": "ogg", "data": base64::engine::general_purpose::STANDARD.encode(b"OggS fake opus") },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": "I think",
            "segments": [{ "id": 0, "start": 0, "end": 6, "text": "I think" }],
            "words": [
                { "word": "I", "start": 0.26, "end": 0.32, "confidence": 0.99 },
                { "word": "think", "start": 0.36, "end": 0.5, "confidence": 0.99 },
            ],
            "usage": { "seconds": 6, "cost": 0.0002 },
        })))
        .expect(1)
        .mount(&server)
        .await;

    let words = transcriber(&server).transcribe(&audio).await.result.unwrap();

    assert_eq!(
        words,
        vec![Word { text: "I".into(), start: 0.26, end: 0.32 }, Word { text: "think".into(), start: 0.36, end: 0.5 }]
    );
}

#[tokio::test]
async fn flac_chunks_are_labelled_flac() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(body_partial_json(json!({ "input_audio": { "format": "flac" } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "text": "", "words": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let words = transcriber(&server).transcribe(&chunk(dir.path(), "you-0.flac", b"fLaC")).await.result.unwrap();
    assert!(words.is_empty());
}

#[tokio::test]
async fn when_verbose_json_is_rejected_it_falls_back_to_json_and_spreads_the_words_over_the_chunk() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("POST"))
        .and(is_verbose)
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":{"message":"response_format not supported"}}"#))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "response_format": "json" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "text": " one two  three " })))
        .expect(1)
        .mount(&server)
        .await;

    let words = transcriber(&server).transcribe(&chunk(dir.path(), "others-0.ogg", b"x")).await.result.unwrap();

    assert_eq!(
        words,
        vec![
            Word { text: "one".into(), start: 0.0, end: 2.0 },
            Word { text: "two".into(), start: 2.0, end: 4.0 },
            Word { text: "three".into(), start: 4.0, end: 6.0 },
        ]
    );
}

#[tokio::test]
async fn a_chunk_rejected_in_both_formats_is_reported_as_rejected() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string(r#"{"error":{"message":"invalid audio"}}"#))
        .expect(2)
        .mount(&server)
        .await;

    let billed = transcriber(&server).transcribe(&chunk(dir.path(), "others-0.ogg", b"x")).await;
    assert_eq!(billed.result, Err(TranscribeError::Rejected("invalid audio".into())));
    assert_eq!(billed.usage, None, "a rejection reports no cost");
}

#[tokio::test]
async fn a_zdr_violation_404_is_reported_as_such_with_one_request() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(404).set_body_string(ZDR_BODY)).expect(1).mount(&server).await;

    let result = transcriber(&server).transcribe(&chunk(dir.path(), "others-0.ogg", b"x")).await.result;

    let Err(TranscribeError::ZdrViolation(message)) = result else { panic!("got {result:?}") };
    assert!(message.contains("elevenlabs/scribe-v2"), "names the model: {message}");
    assert!(message.contains("zero-data-retention"), "explains the cause: {message}");
}

#[tokio::test]
async fn server_errors_rate_limits_and_running_out_of_credit_are_worth_retrying() {
    let dir = tempfile::tempdir().unwrap();
    for status in [402, 408, 429, 500, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_string(r#"{"error":{"message":"nope"}}"#))
            .mount(&server)
            .await;
        let result = transcriber(&server).transcribe(&chunk(dir.path(), "others-0.ogg", b"x")).await.result;
        assert!(matches!(result, Err(TranscribeError::Transient(_))), "{status}: {result:?}");
    }
}

#[tokio::test]
async fn a_rejected_api_key_is_reported_as_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    for status in [401, 403] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_string(r#"{"error":{"message":"User not found."}}"#))
            .expect(1)
            .mount(&server)
            .await;
        let result = transcriber(&server).transcribe(&chunk(dir.path(), "others-0.ogg", b"x")).await.result;
        let Err(TranscribeError::Unauthorized(message)) = result else { panic!("{status}: {result:?}") };
        assert!(message.contains("Settings"), "says how to fix it: {message}");
    }
}

#[tokio::test]
async fn an_unreachable_server_is_worth_retrying() {
    let dir = tempfile::tempdir().unwrap();
    let t = OpenRouterTranscriber::new("http://127.0.0.1:9/api/v1".into(), "k".into(), "m".into());
    let result = t.transcribe(&chunk(dir.path(), "others-0.ogg", b"x")).await.result;
    assert!(matches!(result, Err(TranscribeError::Transient(_))), "{result:?}");
}

#[tokio::test]
async fn a_transcription_request_names_tiro_as_the_app() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let audio = chunk(dir.path(), "you-1.ogg", b"OggS");

    Mock::given(method("POST"))
        .and(path("/api/v1/audio/transcriptions"))
        .and(header("x-openrouter-title", "Tiro"))
        .and(header("http-referer", "https://github.com/drittich/tiro"))
        .and(header("x-openrouter-app-visibility", "hidden"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "text": "", "words": [] })))
        .expect(1)
        .mount(&server)
        .await;

    transcriber(&server).transcribe(&audio).await.result.unwrap();
}

#[tokio::test]
async fn a_transcription_reports_what_openrouter_charged_for_it() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-generation-id", "gen-stt-1")
                .set_body_json(json!({ "text": "hi", "words": [], "usage": { "seconds": 3.99, "cost": 0.000244 } })),
        )
        .mount(&server)
        .await;

    let billed = transcriber(&server).transcribe(&chunk(dir.path(), "you-1.ogg", b"OggS")).await;

    assert_eq!(
        billed.usage,
        Some(Usage {
            model: "elevenlabs/scribe-v2".into(),
            cost: 0.000244,
            estimated: false,
            prompt_tokens: None,
            completion_tokens: None,
            seconds: Some(3.99),
            generation_id: Some("gen-stt-1".into()),
        })
    );
}

#[tokio::test]
async fn a_rejected_verbose_json_and_its_json_retry_are_charged_once() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("POST"))
        .and(is_verbose)
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("x-generation-id", "gen-stt-rejected")
                .set_body_string(r#"{"error":{"message":"response_format not supported"}}"#),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "response_format": "json" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "text": "one", "usage": { "seconds": 6, "cost": 0.0002 } })))
        .mount(&server)
        .await;
    // A rejection costs nothing, so its generation isn't looked up.
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).expect(0).mount(&server).await;

    let billed = transcriber(&server).transcribe(&chunk(dir.path(), "others-0.ogg", b"x")).await;

    assert_eq!(billed.usage.map(|u| u.cost), Some(0.0002));
}

#[tokio::test]
async fn a_cost_missing_from_the_answer_is_looked_up_by_generation_id() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-generation-id", "gen-stt-2")
                .set_body_json(json!({ "text": "", "words": [], "usage": { "seconds": 6 } })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/generation"))
        .and(query_param("id", "gen-stt-2"))
        .and(header("authorization", "Bearer sk-or-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "id": "gen-stt-2", "total_cost": 0.0003 } })))
        .expect(1)
        .mount(&server)
        .await;

    let usage = transcriber(&server).transcribe(&chunk(dir.path(), "you-1.ogg", b"OggS")).await.usage.unwrap();

    assert_eq!((usage.cost, usage.estimated, usage.seconds), (0.0003, false, Some(6.0)));
}

/// Runs only with TIRO_LIVE_TESTS=1. Reads the key from the key file exactly as the app does.
#[tokio::test]
async fn live_openrouter_transcribes_a_fixture_chunk() {
    if std::env::var("TIRO_LIVE_TESTS").as_deref() != Ok("1") {
        return;
    }
    let key = tiro_core::keyfile::read_key(&tiro_core::keyfile::default_key_path()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let wav = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/others-62s.wav");
    let samples: Vec<i16> = hound::WavReader::open(wav).unwrap().into_samples().map(Result::unwrap).take(16_000 * 10).collect();
    let path = dir.path().join("others-0.ogg");
    tiro_core::encoder::write_ogg_opus(&path, 16_000, &samples).unwrap();

    let t = OpenRouterTranscriber::new("https://openrouter.ai/api/v1".into(), key, "elevenlabs/scribe-v2".into());
    let words = t.transcribe(&ChunkAudio { path, duration: 10.0 }).await.result.unwrap();

    assert!(words.len() > 10, "{words:?}");
    assert!(words.iter().all(|w| w.start <= w.end && w.end <= 10.5));
}

mod answerer {
    use std::sync::Mutex;

    use serde_json::json;
    use tiro_core::ask::{AskError, Answerer, Message, Role};
    use tiro_core::openrouter::OpenRouterAnswerer;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::ZDR_BODY;

    fn answerer(server: &MockServer) -> OpenRouterAnswerer {
        OpenRouterAnswerer::new(format!("{}/api/v1", server.uri()), "sk-or-test".into(), "deepseek/deepseek-v4.1-flash".into())
    }

    fn question() -> Vec<Message> {
        vec![
            Message { role: Role::System, content: "Answer only from the Passages.".into() },
            Message { role: Role::User, content: "Question: MCP?".into() },
        ]
    }

    fn sse(events: &[serde_json::Value]) -> String {
        let mut body = String::from(": OPENROUTER PROCESSING\n\n");
        for event in events {
            body.push_str(&format!("data: {event}\r\n\r\n"));
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    #[tokio::test]
    async fn the_answer_streams_piece_by_piece_and_reports_its_cost() {
        let server = MockServer::start().await;
        let delta = |text: &str| json!({ "id": "gen-7", "choices": [{ "delta": { "content": text } }] });
        let body = sse(&[
            json!({ "id": "gen-7", "choices": [{ "delta": { "role": "assistant", "content": "", "reasoning": "hmm" } }] }),
            delta("Per "),
            delta("project [1]."),
            json!({ "id": "gen-7", "choices": [{ "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 3000, "completion_tokens": 12, "cost": 0.0021 } }),
        ]);
        Mock::given(method("POST"))
            .and(path("/api/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-or-test"))
            .and(body_partial_json(json!({
                "model": "deepseek/deepseek-v4.1-flash",
                "stream": true,
                "messages": [
                    { "role": "system", "content": "Answer only from the Passages." },
                    { "role": "user", "content": "Question: MCP?" },
                ],
            })))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        let pieces = Mutex::new(Vec::new());

        let billed = answerer(&server).answer(&question(), &|piece| pieces.lock().unwrap().push(piece.to_owned())).await;

        assert_eq!(billed.result.unwrap(), "Per project [1].");
        assert_eq!(pieces.into_inner().unwrap(), ["Per ", "project [1]."]);
        let usage = billed.usage.unwrap();
        assert_eq!((usage.cost, usage.prompt_tokens, usage.completion_tokens), (0.0021, Some(3000), Some(12)));
        assert_eq!(usage.generation_id.as_deref(), Some("gen-7"));
    }

    #[tokio::test]
    async fn an_error_in_the_stream_fails_the_answer() {
        let server = MockServer::start().await;
        let body = sse(&[
            json!({ "id": "gen-8", "choices": [{ "delta": { "content": "Per" } }] }),
            json!({ "error": { "code": 502, "message": "Provider disconnected" } }),
        ]);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let billed = answerer(&server).answer(&question(), &|_| {}).await;

        assert_eq!(billed.result.unwrap_err(), AskError::Failed("OpenRouter stopped the answer: Provider disconnected".into()));
    }

    #[tokio::test]
    async fn a_model_without_zero_data_retention_is_refused_naming_the_ask_model_setting() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(404).set_body_string(ZDR_BODY)).mount(&server).await;

        let billed = answerer(&server).answer(&question(), &|_| {}).await;

        let AskError::ZdrViolation(message) = billed.result.unwrap_err() else { panic!("not a ZDR refusal") };
        assert!(message.contains("Choose another Ask model in Settings"));
        assert_eq!(billed.usage, None);
    }

    #[tokio::test]
    async fn a_rejected_key_is_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": { "message": "No auth credentials found" } })))
            .mount(&server)
            .await;

        let billed = answerer(&server).answer(&question(), &|_| {}).await;

        assert!(matches!(billed.result, Err(AskError::Unauthorized(_))));
    }
}

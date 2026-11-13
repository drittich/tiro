use chrono::{DateTime, FixedOffset};
use tiro_core::ask::{build_context, Conversation, MeetingMaterial, Source, Turn};
use tiro_core::search::{split, Hit, Place};
use tiro_core::transcript::{Segment, Track};

fn at(minute: u32) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(&format!("2026-10-14T14:{minute:02}:00-04:00")).unwrap()
}

fn seg(track: Track, speaker: Option<u32>, start: f64, text: &str) -> Segment {
    Segment { track, start, end: start + 30.0, text: text.into(), speaker }
}

/// A Meeting whose Transcript is `windows` 30-second windows, each one Segment saying "<id> w<n>", with a Summary of
/// one Decisions section.
fn material(id: &str, title: &str, windows: usize) -> MeetingMaterial {
    let segments: Vec<Segment> =
        (0..windows).map(|n| seg(Track::Others, None, n as f64 * 30.0, &format!("{id} w{n}"))).collect();
    let summary = format!("## Decisions\n{id} decided things.");
    MeetingMaterial {
        meeting_id: id.into(),
        title: title.into(),
        started_at: at(0),
        passages: split(id, title, &summary, "", &segments),
        segments,
    }
}

fn hit(meeting_id: &str, place: Place) -> Hit {
    Hit { meeting_id: meeting_id.into(), place, snippet: Vec::new(), score: 0.0 }
}

fn window(meeting_id: &str, n: u64) -> Hit {
    hit(meeting_id, Place::Transcript { ms: n * 30_000 })
}

fn decisions(meeting_id: &str) -> Hit {
    hit(meeting_id, Place::Summary { section: 1, heading: Some("Decisions".into()) })
}

fn lookup(materials: Vec<MeetingMaterial>) -> impl FnMut(&str) -> Option<MeetingMaterial> {
    move |id| materials.iter().find(|m| m.meeting_id == id).cloned()
}

fn places(sources: &[Source]) -> Vec<(String, Place)> {
    sources.iter().map(|s| (s.meeting_id.clone(), s.place.clone())).collect()
}

#[test]
fn a_transcript_passage_is_widened_by_the_window_on_each_side_and_a_summary_section_is_sent_as_it_is() {
    let ranked = [window("m1", 5), decisions("m1")];

    let sources = build_context(&ranked, &Conversation::default(), lookup(vec![material("m1", "Weekly sync", 10)]));

    assert_eq!(places(&sources), vec![("m1".into(), Place::Transcript { ms: 150_000 }), ("m1".into(), decisions("m1").place)]);
    assert_eq!(sources[0].text, "[02:00] Others: m1 w4\n[02:30] Others: m1 w5\n[03:00] Others: m1 w6");
    assert_eq!(sources[1].text, "Decisions\nm1 decided things.");
    assert_eq!((sources[0].number, sources[1].number), (1, 2));
    assert_eq!((sources[0].title.as_str(), sources[0].started_at), ("Weekly sync", at(0)));
}

#[test]
fn the_first_and_last_windows_are_widened_on_the_side_they_have() {
    let ranked = [window("m1", 0), window("m1", 9)];

    let sources = build_context(&ranked, &Conversation::default(), lookup(vec![material("m1", "T", 10)]));

    assert_eq!(sources[0].text, "[00:00] Others: m1 w0\n[00:30] Others: m1 w1");
    assert_eq!(sources[1].text, "[04:00] Others: m1 w8\n[04:30] Others: m1 w9");
}

#[test]
fn transcript_segments_are_labelled_me_others_or_their_speaker() {
    let segments = vec![
        seg(Track::You, None, 0.0, "I said this"),
        seg(Track::Others, None, 30.0, "someone said that"),
        seg(Track::Others, Some(2), 60.0, "Sam said so"),
    ];
    let meeting = MeetingMaterial {
        meeting_id: "m1".into(),
        title: "T".into(),
        started_at: at(0),
        passages: split("m1", "T", "", "", &segments),
        segments,
    };

    let sources = build_context(&[window("m1", 1)], &Conversation::default(), lookup(vec![meeting]));

    assert_eq!(sources[0].text, "[00:00] Me: I said this\n[00:30] Others: someone said that\n[01:00] Others (Speaker 2): Sam said so");
}

#[test]
fn a_window_already_sent_as_a_neighbour_is_neither_repeated_nor_sent_again() {
    let ranked = [window("m1", 5), window("m1", 6), window("m1", 7)];

    let sources = build_context(&ranked, &Conversation::default(), lookup(vec![material("m1", "T", 10)]));

    // w6 came with w5, so its hit adds nothing; w7 is widened by w8 only, since w6 is already there.
    assert_eq!(places(&sources), vec![("m1".into(), Place::Transcript { ms: 150_000 }), ("m1".into(), Place::Transcript { ms: 210_000 })]);
    assert_eq!(sources[1].text, "[03:30] Others: m1 w7\n[04:00] Others: m1 w8");
}

#[test]
fn at_most_25_passages_are_taken_and_at_most_6_from_any_one_meeting() {
    let materials: Vec<MeetingMaterial> = (0..6).map(|m| material(&format!("m{m}"), "T", 40)).collect();
    // m0 ranks first with 10 hits, far apart so none is another's neighbour; then 5 hits from each other Meeting.
    let mut ranked: Vec<Hit> = (0..10).map(|n| window("m0", n * 3)).collect();
    for m in 1..6 {
        ranked.extend((0..5).map(|n| window(&format!("m{m}"), n * 3)));
    }

    let sources = build_context(&ranked, &Conversation::default(), lookup(materials));

    assert_eq!(sources.len(), 25);
    assert_eq!(sources.iter().filter(|s| s.meeting_id == "m0").count(), 6);
    // The lowest-ranked hits, m4's last and all of m5's, are left out.
    assert_eq!(sources.iter().filter(|s| s.meeting_id == "m4").count(), 4);
    assert!(sources.iter().all(|s| s.meeting_id != "m5"));
}

#[test]
fn the_lowest_ranked_passages_are_dropped_until_the_rest_fit_about_12k_tokens_and_none_is_cut() {
    // About 3,000 tokens a section.
    let long = |id: &str| MeetingMaterial {
        passages: split(id, "T", &format!("## Decisions\n{}", "word ".repeat(2_400)), "", &[]),
        ..material(id, "T", 0)
    };
    let ranked: Vec<Hit> = (0..6).map(|m| decisions(&format!("m{m}"))).collect();

    let sources = build_context(&ranked, &Conversation::default(), lookup((0..6).map(|m| long(&format!("m{m}"))).collect()));

    assert_eq!(sources.iter().map(|s| s.meeting_id.as_str()).collect::<Vec<_>>(), ["m0", "m1", "m2"]);
    assert!(sources.iter().all(|s| s.text.ends_with("word")));
}

#[test]
fn a_smaller_lower_ranked_passage_is_dropped_before_a_larger_higher_ranked_one() {
    let sized = |id: &str, words: usize| MeetingMaterial {
        passages: split(id, "T", &format!("## Decisions\n{}", "word ".repeat(words)), "", &[]),
        ..material(id, "T", 0)
    };
    // About 3,000 + 3,000 + 7,500 tokens, then a small one: only dropping the last two brings it under 12k.
    let materials = vec![sized("m0", 2_400), sized("m1", 2_400), sized("m2", 6_000), sized("m3", 10)];
    let ranked: Vec<Hit> = (0..4).map(|m| decisions(&format!("m{m}"))).collect();

    let sources = build_context(&ranked, &Conversation::default(), lookup(materials));

    assert_eq!(sources.iter().map(|s| s.meeting_id.as_str()).collect::<Vec<_>>(), ["m0", "m1"]);
}

#[test]
fn passages_already_in_the_conversation_are_not_sent_again_and_numbers_carry_on() {
    let materials = || lookup(vec![material("m1", "T", 10)]);
    let mut conversation = Conversation::default();
    let first = build_context(&[window("m1", 5), decisions("m1")], &conversation, materials());
    conversation.push(Turn { question: "What was decided?".into(), sources: first, answer: "Things [2].".into() });

    let sources = build_context(&[decisions("m1"), window("m1", 6), window("m1", 2)], &conversation, materials());

    // The Decisions section and w6, which came with w5, are already in view.
    assert_eq!(places(&sources), vec![("m1".into(), Place::Transcript { ms: 60_000 })]);
    assert_eq!(sources[0].number, 3);
    assert_eq!(sources[0].text, "[00:30] Others: m1 w1\n[01:00] Others: m1 w2\n[01:30] Others: m1 w3");
}

#[test]
fn past_the_conversation_cap_the_oldest_turns_passages_go_first_and_their_answers_stay() {
    let materials = || lookup(vec![material("m1", "T", 10)]);
    let mut conversation = Conversation::default();
    for (n, question) in ["first", "second"].into_iter().enumerate() {
        let sources = build_context(&[window("m1", n as u64 * 3 + 1)], &conversation, materials());
        conversation.push(Turn { question: question.into(), sources, answer: format!("{question} answer") });
    }
    let second_turn_only = conversation.turns()[1].tokens() + conversation.turns()[0].tokens() - conversation.turns()[0].sources_tokens();

    conversation.cap(second_turn_only);

    let turns = conversation.turns();
    assert!(turns[0].sources.is_empty());
    assert_eq!(turns[0].answer, "first answer");
    assert_eq!(turns[1].sources.len(), 1);
    // The first turn's Passage is out of view now, so it can be sent again, with a new number.
    let again = build_context(&[window("m1", 1)], &conversation, materials());
    assert_eq!((again.len(), again[0].number), (1, 3));
}

mod citations {
    use tiro_core::ask::{link_citations, CITE_CLOSE, CITE_OPEN};

    fn cite(n: usize) -> String {
        format!("{CITE_OPEN}{n}{CITE_CLOSE}")
    }

    #[test]
    fn passage_numbers_in_brackets_become_citations_in_the_order_first_cited() {
        let (text, cited) = link_citations("Ship it [3]. Sam agreed [1, 3] and [Passage 2][1].", &[1, 2, 3]);

        assert_eq!(text, format!("Ship it {}. Sam agreed {}{} and {}{}.", cite(3), cite(1), cite(3), cite(2), cite(1)));
        assert_eq!(cited, [3, 1, 2]);
    }

    #[test]
    fn numbers_that_match_no_passage_are_dropped_rather_than_shown_as_broken_links() {
        let (text, cited) = link_citations("Decided [2, 9]. Made up [7]. Also [Passages 8; 2].", &[1, 2]);

        assert_eq!(text, format!("Decided {}. Made up. Also {}.", cite(2), cite(2)));
        assert_eq!(cited, [2]);
    }

    #[test]
    fn brackets_that_are_not_citations_are_left_alone() {
        let answer = "- [ ] Owner: task\n- [x] done\nSee [the doc] and [1a], or [].";

        assert_eq!(link_citations(answer, &[1]), (answer.to_owned(), vec![]));
    }

    #[test]
    fn an_unfinished_citation_at_the_end_of_a_streaming_answer_stays_as_text() {
        assert_eq!(link_citations("So far [1", &[1]), ("So far [1".to_owned(), vec![]));
    }
}

mod asking {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::{DateTime, NaiveDate};
    use tiro_core::ask::{ask, AskError, AskEvent, Answerer, Asking, Conversation, Message, Role, CITE_CLOSE, CITE_OPEN};
    use tiro_core::search::Place;
    use tiro_core::spend::{Billed, SpendKind, SpendLedger, Usage};
    use tiro_core::store::{MeetingStatus, MeetingStore};
    use tiro_core::transcript::{Segment, Track};

    /// Answers with each of `replies` in turn, streamed a few characters at a time, recording the messages it got.
    struct FakeAnswerer {
        replies: Mutex<Vec<Result<String, AskError>>>,
        asked: Arc<Mutex<Vec<Vec<Message>>>>,
    }

    impl FakeAnswerer {
        fn new(replies: &[&str]) -> Self {
            Self { replies: Mutex::new(replies.iter().map(|r| Ok(r.to_string())).collect()), asked: Arc::default() }
        }

        fn failing(error: AskError) -> Self {
            Self { replies: Mutex::new(vec![Err(error)]), asked: Arc::default() }
        }

        /// The last request's final message: the Passages and the question.
        fn last_prompt(&self) -> String {
            self.asked.lock().unwrap().last().unwrap().last().unwrap().content.clone()
        }
    }

    #[async_trait]
    impl Answerer for FakeAnswerer {
        async fn answer(&self, messages: &[Message], on_text: &(dyn for<'s> Fn(&'s str) + Send + Sync)) -> Billed<String, AskError> {
            self.asked.lock().unwrap().push(messages.to_vec());
            let reply = self.replies.lock().unwrap().remove(0);
            if let Ok(reply) = &reply {
                for piece in reply.as_bytes().chunks(4) {
                    on_text(std::str::from_utf8(piece).unwrap());
                }
            }
            let usage = Usage {
                model: "deepseek/deepseek-v4.1-flash".into(),
                cost: 0.002,
                estimated: false,
                prompt_tokens: Some(3000),
                completion_tokens: Some(200),
                seconds: None,
                generation_id: Some("gen-1".into()),
            };
            Billed { result: reply, usage: Some(usage) }
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        store: MeetingStore,
        spend: SpendLedger,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let store = MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap();
        let spend = SpendLedger::new(dir.path().join("spend.jsonl"));
        Env { _dir: dir, store, spend }
    }

    /// A stopped, indexed Meeting with one Segment per line, 30 seconds apart; a line starting "Me:" is the
    /// microphone's.
    fn meeting(env: &Env, title: &str, tags: &[&str], lines: &[&str]) -> String {
        let meeting = env.store.create("Manual", DateTime::parse_from_rfc3339("2026-10-14T14:00:00-04:00").unwrap()).unwrap();
        let segments: Vec<Segment> = lines
            .iter()
            .enumerate()
            .map(|(n, line)| {
                let (track, text) = match line.strip_prefix("Me: ") {
                    Some(text) => (Track::You, text),
                    None => (Track::Others, *line),
                };
                Segment { track, start: n as f64 * 30.0, end: n as f64 * 30.0 + 30.0, text: text.into(), speaker: None }
            })
            .collect();
        env.store.append_segments(&meeting, &segments).unwrap();
        env.store.set_title(&meeting.id, title).unwrap();
        env.store.set_tags(&meeting.id, &tags.iter().map(|t| t.to_string()).collect::<Vec<_>>()).unwrap();
        env.store.set_status(&meeting.id, MeetingStatus::Complete, None).unwrap();
        env.store.index_passages(&meeting.id).unwrap();
        meeting.id
    }

    fn asking<'a>(env: &'a Env, answerer: &'a FakeAnswerer) -> Asking<'a> {
        Asking {
            store: &env.store,
            meaning: None,
            answerer,
            spend: &env.spend,
            today: NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(),
        }
    }

    fn cite(n: usize) -> String {
        format!("{CITE_OPEN}{n}{CITE_CLOSE}")
    }

    #[tokio::test]
    async fn an_answer_streams_in_cites_its_passages_and_its_cost_is_recorded_as_an_answer() {
        let env = env();
        let id = meeting(&env, "Weekly sync", &[], &["We agreed MCP permissions stay per project."]);
        let answerer = FakeAnswerer::new(&["They stay per project [1]."]);
        let streamed = Mutex::new(Vec::new());
        let mut conversation = Conversation::default();

        let answer = ask(&asking(&env, &answerer), &mut conversation, "What about MCP permissions?", &|event| {
            streamed.lock().unwrap().push(event)
        })
        .await
        .unwrap();

        assert_eq!(answer.text, format!("They stay per project {}.", cite(1)));
        assert_eq!(answer.sources.len(), 1);
        assert_eq!((answer.sources[0].meeting_id.as_str(), &answer.sources[0].place), (id.as_str(), &Place::Transcript { ms: 0 }));
        let streamed = streamed.into_inner().unwrap();
        // The Passages come first, so citations work while the answer is still streaming.
        assert_eq!(streamed[0], AskEvent::Passages { sources: answer.sources.clone() });
        assert!(streamed.len() > 2);
        assert_eq!(streamed.last().unwrap(), &AskEvent::Text { text: answer.text.clone() });
        let prompt = answerer.last_prompt();
        assert!(prompt.contains("[1] Weekly sync · Wed 14 Oct 2026 · Transcript at 00:00\n[00:00] Others: We agreed MCP permissions stay per project."));
        assert!(prompt.ends_with("What about MCP permissions?"));
        let entries = env.spend.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0].kind, entries[0].meeting_id.as_deref()), (SpendKind::Answer, None));
        assert_eq!(conversation.turns().len(), 1);
    }

    #[tokio::test]
    async fn the_model_is_told_to_answer_only_from_the_passages_and_say_when_they_dont_answer() {
        let env = env();
        meeting(&env, "Weekly sync", &[], &["Lunch plans."]);
        let answerer = FakeAnswerer::new(&["The Meetings don't say."]);

        let answer = ask(&asking(&env, &answerer), &mut Conversation::default(), "Who won the election?", &|_| {}).await.unwrap();

        assert!(answer.sources.is_empty());
        let system = answerer.asked.lock().unwrap()[0][0].clone();
        assert_eq!(system.role, Role::System);
        assert!(system.content.contains("only from the numbered Passages"));
        assert!(system.content.contains("say so"));
        assert!(answerer.last_prompt().contains("No Passages"));
    }

    #[tokio::test]
    async fn what_i_said_is_labelled_me_in_the_passages() {
        let env = env();
        meeting(&env, "Entergy check-in", &[], &["Me: The Entergy migration slips to March.", "Others reply."]);
        let answerer = FakeAnswerer::new(&["You said it slips to March [1]."]);

        ask(&asking(&env, &answerer), &mut Conversation::default(), "What did I say about the Entergy migration?", &|_| {}).await.unwrap();

        assert!(answerer.last_prompt().contains("[00:00] Me: The Entergy migration slips to March."));
    }

    #[tokio::test]
    async fn a_follow_up_has_the_earlier_turns_in_view_and_retrieves_passages_for_itself() {
        let env = env();
        meeting(&env, "Weekly sync", &[], &["MCP permissions stay per project.", "Filler talk.", "More filler.", "Even more.", "Sam said the Entergy rollout waits."]);
        let answerer = FakeAnswerer::new(&["Per project [1].", "Sam said it waits [2]; see also [1]."]);
        let mut conversation = Conversation::default();
        let asking = asking(&env, &answerer);
        ask(&asking, &mut conversation, "What about MCP permissions?", &|_| {}).await.unwrap();

        let answer = ask(&asking, &mut conversation, "and what did Sam say?", &|_| {}).await.unwrap();

        let messages = answerer.asked.lock().unwrap()[1].clone();
        let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::System, Role::User, Role::Assistant, Role::User]);
        assert!(messages[1].content.contains("[1] Weekly sync"));
        assert_eq!(messages[2].content, "Per project [1].");
        // Asked with the previous question, it finds Sam's window; the earlier Passage isn't sent again, and can
        // still be cited.
        assert!(messages[3].content.contains("[2] Weekly sync · Wed 14 Oct 2026 · Transcript at 02:00"));
        assert!(!messages[3].content.contains("[1] Weekly sync"));
        assert_eq!(answer.text, format!("Sam said it waits {}; see also {}.", cite(2), cite(1)));
        assert_eq!(answer.sources.iter().map(|s| s.number).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(conversation.turns().len(), 2);
    }

    #[tokio::test]
    async fn a_follow_up_is_searched_with_the_previous_question_since_alone_it_may_find_nothing() {
        let env = env();
        meeting(&env, "Weekly sync", &[], &["MCP permissions stay per project."]);
        let answerer = FakeAnswerer::new(&["Per project [1].", "Nobody, but [2] went further."]);
        let mut conversation = Conversation::default();
        let asking = asking(&env, &answerer);
        ask(&asking, &mut conversation, "What about MCP permissions?", &|_| {}).await.unwrap();
        meeting(&env, "Security review", &[], &["MCP permissions need an audit log."]);

        ask(&asking, &mut conversation, "and who disagreed?", &|_| {}).await.unwrap();

        assert!(answerer.last_prompt().contains("[2] Security review"));
    }

    #[tokio::test]
    async fn a_tag_filter_limits_the_passages_to_meetings_with_that_tag() {
        let env = env();
        let tagged = meeting(&env, "Agent review", &["MCP"], &["Permissions are per project."]);
        meeting(&env, "Other sync", &["Hiring"], &["Permissions are per team."]);
        let answerer = FakeAnswerer::new(&["Per project [1]."]);

        let answer = ask(&asking(&env, &answerer), &mut Conversation::default(), "tag:MCP what about permissions?", &|_| {}).await.unwrap();

        assert_eq!(answer.sources[0].meeting_id, tagged);
        assert!(!answerer.last_prompt().contains("Other sync"));
    }

    #[tokio::test]
    async fn a_failed_answer_leaves_the_conversation_as_it_was_and_its_cost_is_still_recorded() {
        let env = env();
        meeting(&env, "Weekly sync", &[], &["MCP permissions."]);
        let answerer = FakeAnswerer::failing(AskError::Failed("OpenRouter returned 502".into()));
        let mut conversation = Conversation::default();

        let result = ask(&asking(&env, &answerer), &mut conversation, "MCP?", &|_| {}).await;

        assert_eq!(result.unwrap_err(), AskError::Failed("OpenRouter returned 502".into()));
        assert_eq!(conversation, Conversation::default());
        assert_eq!(env.spend.entries().len(), 1);
    }
}

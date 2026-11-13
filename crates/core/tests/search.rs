use chrono::DateTime;
use tiro_core::search::{fuse, group_by_meeting, index_stale, parse_query, search, split, Fragment, Hit, IndexProgress, Passage, Place, Query};
use tiro_core::store::{Meeting, MeetingStatus, MeetingStore};
use tiro_core::summary::Summary;
use tiro_core::transcript::{Segment, Track};

fn seg(start: f64, end: f64, text: &str) -> Segment {
    Segment { track: Track::Others, start, end, text: text.into(), speaker: None }
}

fn places(passages: &[Passage]) -> Vec<Place> {
    passages.iter().map(|p| p.place.clone()).collect()
}

#[test]
fn the_title_is_a_passage_of_its_own() {
    let passages = split("m1", "Wolverine sync", "", "", &[]);

    assert_eq!(passages, vec![Passage { meeting_id: "m1".into(), place: Place::Title, text: "Wolverine sync".into() }]);
}

#[test]
fn the_summary_splits_at_its_headings() {
    let summary = "Intro line.\n\n## TL;DR\n- We shipped **813**.\n\n## Decisions\n\nNo whitelisting.\n\n## Empty\n";

    let passages = split("m1", "T", summary, "", &[]);

    let summary: Vec<_> = passages.iter().filter(|p| matches!(p.place, Place::Summary { .. })).collect();
    assert_eq!(
        summary.iter().map(|p| (p.place.clone(), p.text.as_str())).collect::<Vec<_>>(),
        vec![
            (Place::Summary { section: 0, heading: None }, "Intro line."),
            (Place::Summary { section: 1, heading: Some("TL;DR".into()) }, "We shipped 813."),
            (Place::Summary { section: 2, heading: Some("Decisions".into()) }, "No whitelisting."),
            (Place::Summary { section: 3, heading: Some("Empty".into()) }, ""),
        ]
    );
}

#[test]
fn notes_split_into_paragraphs_at_blank_lines_each_with_its_first_line() {
    let notes = "first idea\nmore of it\n\n\n- action: call Entergy\n\nlast";

    let passages = split("m1", "T", "", notes, &[]);

    assert_eq!(
        passages[1..].iter().map(|p| (p.place.clone(), p.text.as_str())).collect::<Vec<_>>(),
        vec![
            (Place::Notes { line: 0 }, "first idea more of it"),
            (Place::Notes { line: 4 }, "action: call Entergy"),
            (Place::Notes { line: 6 }, "last"),
        ]
    );
}

#[test]
fn transcript_windows_run_30_to_60_seconds_of_whole_segments() {
    let segments: Vec<Segment> = (0..10).map(|i| seg(i as f64 * 10.0, i as f64 * 10.0 + 10.0, &format!("s{i}"))).collect();

    let passages = split("m1", "T", "", "", &segments);

    assert_eq!(
        passages[1..].iter().map(|p| (p.place.clone(), p.text.as_str())).collect::<Vec<_>>(),
        vec![
            (Place::Transcript { ms: 0 }, "s0 s1 s2"),
            (Place::Transcript { ms: 30_000 }, "s3 s4 s5"),
            (Place::Transcript { ms: 60_000 }, "s6 s7 s8"),
            (Place::Transcript { ms: 90_000 }, "s9"),
        ]
    );
}

#[test]
fn a_segment_that_would_stretch_a_window_past_60_seconds_starts_the_next() {
    let segments = [seg(0.0, 25.0, "short"), seg(25.5, 70.0, "long"), seg(70.0, 150.0, "very long")];

    let passages = split("m1", "T", "", "", &segments);

    assert_eq!(
        places(&passages[1..]),
        vec![Place::Transcript { ms: 0 }, Place::Transcript { ms: 25_500 }, Place::Transcript { ms: 70_000 }]
    );
}

#[test]
fn transcript_passages_hold_the_text_as_shown_without_fillers() {
    let segments = [seg(0.0, 1.0, "Um."), seg(2.0, 4.0, "Uh, ticket 813 is done.")];

    let passages = split("m1", "T", "", "", &segments);

    assert_eq!(passages[1].text, "Ticket 813 is done.");
    assert_eq!(passages[1].place, Place::Transcript { ms: 2_000 });
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|&s| s.to_owned()).collect()
}

#[test]
fn a_query_holds_tags_quoted_phrases_and_keywords() {
    let query = parse_query(r#"Wolverine tag:MCP "read-only access" 813 TAG:"Agent Permissions""#);

    assert_eq!(
        query,
        Query {
            tags: words(&["MCP", "Agent Permissions"]),
            phrases: words(&["read-only access"]),
            keywords: words(&["Wolverine", "813"]),
        }
    );
    assert_eq!(query.fts().as_deref(), Some(r#""read-only access" "Wolverine" "813""#));
}

#[test]
fn a_query_of_only_tags_has_no_keyword_match() {
    let query = parse_query("tag:MCP  tag:mcp tag:");

    assert_eq!(query.tags, words(&["MCP"]));
    assert_eq!(query.fts(), None);
    assert_eq!(parse_query("   ").fts(), None);
}

#[test]
fn an_unbalanced_quote_runs_to_the_end_of_the_query() {
    let query = parse_query(r#"budget "no whitelisting"#);

    assert_eq!(query.phrases, words(&["no whitelisting"]));
    assert_eq!(query.keywords, words(&["budget"]));
}

#[test]
fn fts_syntax_typed_in_the_box_is_only_ever_text() {
    let query = parse_query(r#"- * OR NEAR( a"b c* -d ^e """#);

    assert_eq!(query.fts().as_deref(), Some(r#""OR" "NEAR(" "a""b" "c*" "-d" "^e""#));
    assert_eq!(parse_query(r#"- * ( "" " ""#).fts(), None);
}

fn hit(meeting_id: &str, ms: u64, score: f64) -> Hit {
    Hit { meeting_id: meeting_id.into(), place: Place::Transcript { ms }, snippet: Vec::new(), score }
}

#[test]
fn a_meeting_ranks_by_its_best_passage_and_shows_its_top_ones() {
    let hits = vec![hit("a", 1, 2.0), hit("b", 1, 5.0), hit("a", 2, 9.0), hit("a", 3, 1.0), hit("a", 4, 3.0), hit("b", 2, 4.0)];

    let results = group_by_meeting(hits, 3);

    let summary: Vec<(&str, f64, Vec<f64>)> =
        results.iter().map(|r| (r.meeting_id.as_str(), r.score, r.hits.iter().map(|h| h.score).collect())).collect();
    assert_eq!(summary, vec![("a", 9.0, vec![9.0, 3.0, 2.0]), ("b", 5.0, vec![5.0, 4.0])]);
}

#[test]
fn no_hits_make_no_results() {
    assert!(group_by_meeting(Vec::new(), 3).is_empty());
}

struct Env {
    dir: tempfile::TempDir,
    store: MeetingStore,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("index.sqlite")).unwrap();
    Env { dir, store }
}

/// A stopped Meeting with this Transcript, started `minute` minutes past 14:00.
fn finished(store: &MeetingStore, minute: u32, segments: &[Segment]) -> Meeting {
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339(&format!("2026-10-17T14:{minute:02}:00-04:00")).unwrap()).unwrap();
    store.append_segments(&meeting, segments).unwrap();
    store.set_status(&meeting.id, MeetingStatus::Complete, None).unwrap();
    store.get(&meeting.id).unwrap()
}

fn found(store: &MeetingStore, query: &str) -> Vec<String> {
    search(store, query).unwrap().meetings.unwrap().into_iter().map(|r| r.meeting_id).collect()
}

#[test]
fn a_rare_word_in_a_finished_transcript_finds_its_meeting_at_that_time() {
    let env = env();
    let segments = [seg(0.0, 40.0, "Hello everyone."), seg(95.0, 99.0, "Ticket 813 blocks Wolverine.")];
    let meeting = finished(&env.store, 0, &segments);
    finished(&env.store, 30, &[seg(0.0, 5.0, "Nothing to see here.")]);

    let results = search(&env.store, "wolverine").unwrap().meetings.unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].meeting_id, meeting.id);
    let hit = &results[0].hits[0];
    assert_eq!(hit.place, Place::Transcript { ms: 95_000 });
    assert!(
        hit.snippet.contains(&Fragment { text: "Wolverine".into(), matched: true }),
        "{:?}",
        hit.snippet
    );
    assert_eq!(found(&env.store, "813"), vec![meeting.id]);
}

#[test]
fn a_meeting_is_not_searched_until_its_transcript_is_final() {
    let env = env();
    let meeting = env.store.create("Manual", DateTime::parse_from_rfc3339("2026-10-17T14:00:00-04:00").unwrap()).unwrap();
    env.store.append_segments(&meeting, &[seg(0.0, 5.0, "Wolverine")]).unwrap();
    env.store.set_status(&meeting.id, MeetingStatus::Transcribing, None).unwrap();

    assert!(found(&env.store, "wolverine").is_empty());

    env.store.set_status(&meeting.id, MeetingStatus::Incomplete, None).unwrap();
    assert_eq!(found(&env.store, "wolverine"), vec![meeting.id]);
}

#[test]
fn a_new_summary_replaces_the_old_ones_passages() {
    let env = env();
    let meeting = finished(&env.store, 0, &[]);
    env.store.write_summary(&meeting, &Summary { suggested_title: None, markdown: "## Plan\nMigrate Entergy.".into() }).unwrap();
    assert_eq!(found(&env.store, "entergy"), vec![meeting.id.clone()]);

    env.store.write_summary(&meeting, &Summary { suggested_title: None, markdown: "## Plan\nMigrate Acme.".into() }).unwrap();

    assert!(found(&env.store, "entergy").is_empty());
    let results = search(&env.store, "acme").unwrap().meetings.unwrap();
    assert_eq!(results[0].hits[0].place, Place::Summary { section: 1, heading: Some("Plan".into()) });
}

#[test]
fn notes_are_searched_once_indexed_not_on_every_save() {
    let env = env();
    let meeting = finished(&env.store, 0, &[]);
    env.store.write_notes(&meeting, "call Wolverine").unwrap();

    assert!(found(&env.store, "wolverine").is_empty());

    env.store.index_passages(&meeting.id).unwrap();
    assert_eq!(found(&env.store, "wolverine"), vec![meeting.id]);
}

#[test]
fn a_summary_heading_is_searched_but_left_out_of_the_snippet() {
    let env = env();
    let meeting = finished(&env.store, 0, &[]);
    env.store.write_summary(&meeting, &Summary { suggested_title: None, markdown: "## Wolverine\nWe met.".into() }).unwrap();

    let results = search(&env.store, "wolverine").unwrap().meetings.unwrap();

    let hit = &results[0].hits[0];
    assert_eq!(hit.place, Place::Summary { section: 1, heading: Some("Wolverine".into()) });
    assert_eq!(hit.snippet, vec![Fragment { text: "We met.".into(), matched: false }]);
}

#[test]
fn a_renamed_meeting_is_found_by_its_new_title() {
    let env = env();
    let meeting = finished(&env.store, 0, &[]);

    env.store.set_title(&meeting.id, "Wolverine kickoff").unwrap();

    let results = search(&env.store, "kickoff").unwrap().meetings.unwrap();
    assert_eq!(results[0].hits[0].place, Place::Title);
}

#[test]
fn several_tags_keep_only_the_meetings_with_all_of_them() {
    let env = env();
    let both = finished(&env.store, 0, &[seg(0.0, 5.0, "permissions review")]);
    let one = finished(&env.store, 10, &[seg(0.0, 5.0, "permissions review")]);
    env.store.set_tags(&both.id, &words(&["MCP", "Agent Permissions"])).unwrap();
    env.store.set_tags(&one.id, &words(&["MCP"])).unwrap();

    assert_eq!(found(&env.store, r#"permissions tag:mcp tag:"agent permissions""#), vec![both.id]);
    assert_eq!(found(&env.store, "permissions tag:MCP").len(), 2);
    assert!(found(&env.store, "permissions tag:Hiring").is_empty());

    let tags_only = search(&env.store, "tag:MCP tag:Hiring").unwrap();
    assert_eq!(tags_only.tags, words(&["MCP", "Hiring"]));
    assert_eq!(tags_only.meetings, None);
}

#[test]
fn a_deleted_meeting_is_no_longer_found() {
    let env = env();
    let meeting = finished(&env.store, 0, &[seg(0.0, 5.0, "Wolverine")]);

    env.store.delete(&meeting.id).unwrap();

    assert!(found(&env.store, "wolverine").is_empty());
}

#[test]
fn malformed_fts_input_never_fails() {
    let env = env();
    finished(&env.store, 0, &[seg(0.0, 5.0, "read-only access, OR not")]);

    for input in [r#"""#, r#""read-only"#, "-", "*", "OR", "AND OR NOT", "NEAR(a b)", "a*b", "^", "(", ")", "col:x", r#"a"b"#] {
        assert!(search(&env.store, input).is_ok(), "{input:?}");
    }
    assert_eq!(found(&env.store, r#""read-only"#).len(), 1);
    // Only FTS syntax: no matches, not every Meeting.
    assert_eq!(search(&env.store, "- *").unwrap().meetings, Some(vec![]));
    assert_eq!(found(&env.store, "OR").len(), 1);
}

#[test]
fn a_rebuilt_index_gets_every_meetings_passages_back_oldest_first() {
    let env = env();
    let oldest = finished(&env.store, 0, &[seg(0.0, 5.0, "Wolverine")]);
    let newest = finished(&env.store, 30, &[seg(0.0, 5.0, "Wolverine")]);
    let store = MeetingStore::open(&env.dir.path().join("meetings"), &env.dir.path().join("fresh.sqlite")).unwrap();
    store.rebuild_index().unwrap();
    assert!(found(&store, "wolverine").is_empty());

    let mut seen = Vec::new();
    index_stale(&store, |progress| {
        let mut ids = found(&store, "wolverine");
        ids.sort();
        seen.push((progress, ids));
    })
    .unwrap();

    assert_eq!(
        seen,
        vec![
            (IndexProgress { done: 0, total: 2 }, vec![]),
            (IndexProgress { done: 1, total: 2 }, vec![oldest.id.clone()]),
            (IndexProgress { done: 2, total: 2 }, vec![oldest.id.clone(), newest.id.clone()]),
        ]
    );
}

#[test]
fn only_meetings_changed_since_they_were_indexed_are_indexed_again() {
    let env = env();
    let meeting = finished(&env.store, 0, &[]);
    finished(&env.store, 30, &[]);
    let mut totals = Vec::new();
    index_stale(&env.store, |p| totals.push(p.total)).unwrap();
    assert_eq!(totals, vec![0]);

    env.store.write_notes(&meeting, "Wolverine").unwrap();
    totals.clear();
    index_stale(&env.store, |p| totals.push(p.total)).unwrap();

    assert_eq!(totals, vec![1, 1]);
    assert_eq!(found(&env.store, "wolverine"), vec![meeting.id]);
}

fn snippet_hit(meeting_id: &str, ms: u64, text: &str, matched: bool) -> Hit {
    Hit { meeting_id: meeting_id.into(), place: Place::Transcript { ms }, snippet: vec![Fragment { text: text.into(), matched }], score: 0.0 }
}

fn order(hits: &[Hit]) -> Vec<(&str, u64)> {
    hits.iter()
        .map(|h| match h.place {
            Place::Transcript { ms } => (h.meeting_id.as_str(), ms),
            _ => unreachable!(),
        })
        .collect()
}

#[test]
fn a_passage_both_rankings_found_outranks_those_only_one_found() {
    let keyword = vec![snippet_hit("a", 1, "k", true), snippet_hit("b", 1, "k", true)];
    let meaning = vec![snippet_hit("c", 1, "m", false), snippet_hit("b", 1, "m", false)];

    let fused = fuse(keyword, meaning);

    // b: 1/62 + 1/62; a and c: 1/61 each, the keyword match first.
    assert_eq!(order(&fused), vec![("b", 1), ("a", 1), ("c", 1)]);
    assert!((fused[0].score - 2.0 / 62.0).abs() < 1e-12);
    assert!((fused[1].score - 1.0 / 61.0).abs() < 1e-12);
}

#[test]
fn a_passage_both_found_keeps_its_highlighted_snippet() {
    let fused = fuse(vec![snippet_hit("a", 1, "Wolverine", true)], vec![snippet_hit("a", 1, "Wolverine plain", false)]);

    assert_eq!(fused.len(), 1);
    assert_eq!(fused[0].snippet, vec![Fragment { text: "Wolverine".into(), matched: true }]);
}

#[test]
fn either_ranking_alone_keeps_its_order() {
    let only_meaning = fuse(Vec::new(), vec![snippet_hit("a", 2, "x", false), snippet_hit("a", 1, "y", false)]);
    assert_eq!(order(&only_meaning), vec![("a", 2), ("a", 1)]);

    let only_keyword = fuse(vec![snippet_hit("b", 5, "x", true), snippet_hit("a", 1, "y", true)], Vec::new());
    assert_eq!(order(&only_keyword), vec![("b", 5), ("a", 1)]);
}

use chrono::{DateTime, FixedOffset};
use tiro_core::store::{MeetingStatus, MeetingStore};
use tiro_core::summary::Summary;
use tiro_core::transcript::{Segment, Track};

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    db: std::path::PathBuf,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("meetings");
    let db = dir.path().join("index.sqlite");
    Env { _dir: dir, root, db }
}

#[test]
fn a_new_meeting_gets_a_dated_folder_and_a_default_title() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();

    let meeting = store.create("Manual", at("2026-10-17T14:05:33-04:00")).unwrap();

    assert_eq!(meeting.title, "Manual — 2026-10-17 14:05");
    assert_eq!(meeting.id, "2026-10-17-1405-manual");
    assert_eq!(meeting.dir, env.root.join("2026-10-17-1405-manual"));
    assert!(meeting.chunks_dir().is_dir());
    assert_eq!(meeting.status, MeetingStatus::Recording);

    let listed = store.list().unwrap();
    assert_eq!(listed, vec![meeting]);
}

#[test]
fn meetings_started_in_the_same_minute_get_separate_folders() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();

    let first = store.create("Manual", at("2026-10-17T14:05:01-04:00")).unwrap();
    let second = store.create("Manual", at("2026-10-17T14:05:40-04:00")).unwrap();

    assert_eq!(first.id, "2026-10-17-1405-manual");
    assert_eq!(second.id, "2026-10-17-1405-manual-2");
}

#[test]
fn segments_are_appended_to_transcript_jsonl_in_the_adr_0005_schema() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();

    let first = Segment { track: Track::Others, start: 1.5, end: 3.25, text: "Hi all.".into(), speaker: None };
    let second = Segment { track: Track::You, start: 4.0, end: 5.0, text: "Hello!".into(), speaker: None };
    let third = Segment { track: Track::Others, start: 6.0, end: 7.5, text: "Let's start.".into(), speaker: Some(2) };
    store.append_segments(&meeting, &[first.clone(), second.clone()]).unwrap();
    store.append_segments(&meeting, std::slice::from_ref(&third)).unwrap();

    let raw = std::fs::read_to_string(meeting.dir.join("transcript.jsonl")).unwrap();
    assert_eq!(
        raw,
        concat!(
            r#"{"track":"others","start":1.5,"end":3.25,"text":"Hi all."}"#, "\n",
            r#"{"track":"you","start":4.0,"end":5.0,"text":"Hello!"}"#, "\n",
            r#"{"track":"others","start":6.0,"end":7.5,"text":"Let's start.","speaker":2}"#, "\n",
        )
    );
    assert_eq!(store.transcript(&meeting).unwrap(), vec![first, second, third]);
}

#[test]
fn finishing_a_meeting_records_its_end_and_status() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();

    store.set_status(&meeting.id, MeetingStatus::Complete, Some(at("2026-10-17T14:35:00-04:00"))).unwrap();

    let listed = &store.list().unwrap()[0];
    assert_eq!(listed.status, MeetingStatus::Complete);
    assert_eq!(listed.ended_at, Some(at("2026-10-17T14:35:00-04:00")));
}

#[test]
fn the_list_is_newest_first() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    store.create("Manual", at("2026-10-16T09:00:00-04:00")).unwrap();
    store.create("Manual", at("2026-10-17T09:00:00-04:00")).unwrap();

    let ids: Vec<String> = store.list().unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, vec!["2026-10-17-0900-manual", "2026-10-16-0900-manual"]);
}

#[test]
fn a_lost_index_is_rebuilt_from_the_meeting_folders() {
    let env = env();
    let before = {
        let store = MeetingStore::open(&env.root, &env.db).unwrap();
        store.create("Manual", at("2026-10-16T09:00:00-04:00")).unwrap();
        let m = store.create("Manual", at("2026-10-17T09:00:00-04:00")).unwrap();
        store.set_status(&m.id, MeetingStatus::Complete, Some(at("2026-10-17T10:00:00-04:00"))).unwrap();
        store.list().unwrap()
    };
    std::fs::remove_file(&env.db).unwrap();
    std::fs::create_dir_all(env.root.join("not-a-meeting")).unwrap();

    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    assert!(store.list().unwrap().is_empty());
    store.rebuild_index().unwrap();

    assert_eq!(store.list().unwrap(), before);
}

#[test]
fn meetings_left_recording_or_transcribing_by_a_previous_run_are_marked_incomplete() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let recording = store.create("Manual", at("2026-10-17T09:00:00-04:00")).unwrap();
    let transcribing = store.create("Manual", at("2026-10-17T10:00:00-04:00")).unwrap();
    let done = store.create("Manual", at("2026-10-17T11:00:00-04:00")).unwrap();
    store.set_status(&transcribing.id, MeetingStatus::Transcribing, Some(at("2026-10-17T10:30:00-04:00"))).unwrap();
    store.set_status(&done.id, MeetingStatus::Complete, Some(at("2026-10-17T11:30:00-04:00"))).unwrap();

    store.mark_interrupted().unwrap();

    let status = |id: &str| store.get(id).unwrap().status;
    assert_eq!(status(&recording.id), MeetingStatus::Incomplete);
    assert_eq!(status(&transcribing.id), MeetingStatus::Incomplete);
    assert_eq!(status(&done.id), MeetingStatus::Complete);
    assert!(store.list().unwrap().iter().all(|m| m.status != MeetingStatus::Recording && m.status != MeetingStatus::Transcribing));
}

#[test]
fn a_meeting_can_be_retitled() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();

    store.set_title(&meeting.id, "  Pricing brainstorm ").unwrap();

    assert_eq!(store.get(&meeting.id).unwrap().title, "Pricing brainstorm");
    assert_eq!(store.list().unwrap()[0].title, "Pricing brainstorm");
    store.rebuild_index().unwrap();
    assert_eq!(store.list().unwrap()[0].title, "Pricing brainstorm");
    assert_eq!(store.get(&meeting.id).unwrap().id, meeting.id, "the folder keeps its name");
}

#[test]
fn a_summary_titles_the_meeting_until_the_user_renames_it() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    let titled = |title: &str| Summary { suggested_title: Some(title.into()), markdown: "## TL;DR\nTiers.\n".into() };

    store.write_summary(&meeting, &titled("Pricing brainstorm")).unwrap();
    assert_eq!(store.get(&meeting.id).unwrap().title, "Pricing brainstorm", "it replaces the default title");
    assert_eq!(store.list().unwrap()[0].title, "Pricing brainstorm");

    store.write_summary(&meeting, &titled("Pricing tiers")).unwrap();
    assert_eq!(store.get(&meeting.id).unwrap().title, "Pricing tiers", "a new Summary replaces an earlier one's");

    store.write_summary(&meeting, &Summary { suggested_title: None, markdown: "## TL;DR\nx\n".into() }).unwrap();
    assert_eq!(store.get(&meeting.id).unwrap().title, "Pricing tiers", "a Summary without a title keeps it");

    store.set_title(&meeting.id, "Q4 pricing").unwrap();
    store.write_summary(&meeting, &titled("Pricing again")).unwrap();
    assert_eq!(store.get(&meeting.id).unwrap().title, "Q4 pricing", "a rename sticks");
    store.rebuild_index().unwrap();
    assert_eq!(store.list().unwrap()[0].title, "Q4 pricing");
}

#[test]
fn a_meeting_file_from_before_titles_were_applied_keeps_a_title_the_user_gave() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let renamed = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    let untouched = store.create("Zoom", at("2026-10-17T15:05:00-04:00")).unwrap();
    // Older files have no `renamed` field.
    let strip = |dir: &std::path::Path, title: &str| {
        let path = dir.join("meeting.json");
        let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("renamed");
        json["title"] = title.into();
        std::fs::write(&path, json.to_string()).unwrap();
    };
    strip(&renamed.dir, "Kickoff");
    strip(&untouched.dir, "Zoom — 2026-10-17 15:05");
    let summary = Summary { suggested_title: Some("From the Summary".into()), markdown: "## TL;DR\nx\n".into() };

    store.write_summary(&renamed, &summary).unwrap();
    store.write_summary(&untouched, &summary).unwrap();

    assert_eq!(store.get(&renamed.id).unwrap().title, "Kickoff");
    assert_eq!(store.get(&untouched.id).unwrap().title, "From the Summary");
}

#[test]
fn a_blank_title_is_refused() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();

    assert!(store.set_title(&meeting.id, "   ").is_err());
    assert_eq!(store.get(&meeting.id).unwrap().title, "Manual — 2026-10-17 14:05");
}

#[test]
fn a_meeting_without_notes_has_empty_notes() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    assert_eq!(store.notes(&meeting).unwrap(), "");
    std::fs::write(meeting.dir.join("notes.md"), "# mine\n").unwrap();
    assert_eq!(store.notes(&meeting).unwrap(), "# mine\n");
}

#[test]
fn notes_are_saved_to_notes_md_and_survive_a_restart() {
    let env = env();
    let meeting = {
        let store = MeetingStore::open(&env.root, &env.db).unwrap();
        let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
        store.write_notes(&meeting, "- ask about pricing\n").unwrap();
        store.write_notes(&meeting, "- ask about pricing\n- **tiers**\n").unwrap();
        meeting
    };
    assert_eq!(std::fs::read_to_string(meeting.dir.join("notes.md")).unwrap(), "- ask about pricing\n- **tiers**\n");

    // The app restarts mid-Meeting.
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    store.mark_interrupted().unwrap();
    let meeting = store.get(&meeting.id).unwrap();
    assert_eq!(store.notes(&meeting).unwrap(), "- ask about pricing\n- **tiers**\n");

    store.write_notes(&meeting, "").unwrap();
    assert_eq!(store.notes(&meeting).unwrap(), "");
}

#[test]
fn deleting_a_meeting_removes_its_folder_and_index_row() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let kept = store.create("Manual", at("2026-10-17T13:00:00-04:00")).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    store.write_notes(&meeting, "Agenda").unwrap();

    store.delete(&meeting.id).unwrap();

    assert!(!meeting.dir.exists());
    assert_eq!(store.list().unwrap(), vec![kept.clone()]);
    store.rebuild_index().unwrap();
    assert_eq!(store.list().unwrap(), vec![kept]);
}

#[test]
fn delete_only_removes_meeting_folders() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    let stray = env.root.join("not-a-meeting");
    std::fs::create_dir(&stray).unwrap();

    assert!(store.delete("not-a-meeting").is_err());
    assert!(store.delete("..").is_err());
    assert!(store.delete(&format!("{}/chunks", meeting.id)).is_err());
    assert!(store.delete("").is_err());

    assert!(stray.is_dir());
    assert!(meeting.chunks_dir().is_dir());
    assert!(env.root.is_dir());
}

#[test]
fn tags_are_kept_in_meeting_json_and_listed_and_a_meeting_json_without_them_still_loads() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    assert_eq!(store.get(&meeting.id).unwrap().tags, None, "never tagged");

    store.set_tags(&meeting.id, &["Hiring".to_owned(), "Budget".to_owned()]).unwrap();

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(meeting.dir.join("meeting.json")).unwrap()).unwrap();
    assert_eq!(json["tags"], serde_json::json!(["Hiring", "Budget"]));
    let tagged = Some(vec!["Hiring".to_owned(), "Budget".to_owned()]);
    assert_eq!(store.list().unwrap()[0].tags, tagged);

    // Tagged with nothing that fit is not the same as never tagged.
    store.set_tags(&meeting.id, &[]).unwrap();
    assert_eq!(store.get(&meeting.id).unwrap().tags, Some(vec![]));
}

#[test]
fn an_index_from_before_tags_gets_them_on_rebuild() {
    let env = env();
    let meeting = {
        let store = MeetingStore::open(&env.root, &env.db).unwrap();
        let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
        store.set_tags(&meeting.id, &["Hiring".to_owned()]).unwrap();
        meeting
    };
    let conn = rusqlite::Connection::open(&env.db).unwrap();
    conn.execute_batch("ALTER TABLE meetings DROP COLUMN tags;").unwrap();
    drop(conn);

    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    store.rebuild_index().unwrap();

    assert_eq!(store.list().unwrap()[0].tags, Some(vec!["Hiring".to_owned()]));
    assert_eq!(store.get(&meeting.id).unwrap().tags, Some(vec!["Hiring".to_owned()]));
}

#[test]
fn a_new_summary_clears_the_tags_of_the_old_one() {
    let env = env();
    let store = MeetingStore::open(&env.root, &env.db).unwrap();
    let meeting = store.create("Manual", at("2026-10-17T14:05:00-04:00")).unwrap();
    store.set_tags(&meeting.id, &["Hiring".to_owned()]).unwrap();

    store.write_summary(&meeting, &Summary { suggested_title: None, markdown: "## TL;DR\nBudget.\n".into() }).unwrap();

    assert_eq!(store.get(&meeting.id).unwrap().tags, None);
    assert_eq!(store.list().unwrap()[0].tags, None);
}

use std::path::{Path, PathBuf};

use chrono::DateTime;
use tiro_core::export::{default_path, markdown_path, render_markdown, ExportInput, ExportScreenshots, ScreenshotsMode};
use tiro_core::screenshots::Screenshot;
use tiro_core::store::{Meeting, MeetingStatus};
use tiro_core::transcript::{Segment, Track};

fn meeting() -> Meeting {
    Meeting {
        id: "2026-10-17-1405-manual".into(),
        dir: PathBuf::from("/nowhere"),
        title: "Pricing brainstorm".into(),
        app: "Manual".into(),
        started_at: DateTime::parse_from_rfc3339("2026-10-17T14:05:00-04:00").unwrap(),
        ended_at: Some(DateTime::parse_from_rfc3339("2026-10-17T14:37:10-04:00").unwrap()),
        status: MeetingStatus::Complete,
        tags: Some(vec!["Pricing".into(), "Product Roadmap".into()]),
    }
}

fn transcript() -> Vec<Segment> {
    vec![
        Segment { track: Track::Others, start: 1.5, end: 3.0, text: "Shall we start?".into(), speaker: None },
        Segment { track: Track::You, start: 4.0, end: 5.0, text: "Sure.".into(), speaker: None },
        Segment { track: Track::Others, start: 3725.0, end: 3727.0, text: "Thanks all.".into(), speaker: Some(1) },
    ]
}

#[test]
fn the_export_has_summary_notes_and_transcript_sections_under_the_title() {
    let meeting = meeting();
    let transcript = transcript();
    let markdown = render_markdown(&ExportInput {
        meeting: &meeting,
        summary: Some("## TL;DR\nWe priced it.\n\n## Discussion\n\n### Tiers\nThree or four.\n"),
        notes: "Ask Sam.\n",
        transcript: &transcript,
        display_name: "Darcy",
        screenshots: ExportScreenshots::None,
    });

    assert_eq!(
        markdown,
        "# Pricing brainstorm\n\
         \n\
         2026-10-17 14:05–14:37 (32 min) · Manual\n\
         \n\
         Tags: Pricing, Product Roadmap\n\
         \n\
         ## Summary\n\
         \n\
         ### TL;DR\n\
         We priced it.\n\
         \n\
         ### Discussion\n\
         \n\
         #### Tiers\n\
         Three or four.\n\
         \n\
         ## Notes\n\
         \n\
         Ask Sam.\n\
         \n\
         ## Transcript\n\
         \n\
         **[00:01] Others:** Shall we start?\n\
         \n\
         **[00:04] Darcy:** Sure.\n\
         \n\
         **[1:02:05] Others (Speaker 1):** Thanks all.\n"
    );
}

#[test]
fn headings_in_notes_nest_under_the_notes_section_but_code_blocks_are_left_alone() {
    let meeting = meeting();
    let markdown = render_markdown(&ExportInput {
        meeting: &meeting,
        summary: None,
        notes: "# Agenda\n```sh\n# not a heading\n```\n## Item\n#hashtag\n",
        transcript: &[],
        display_name: "",
        screenshots: ExportScreenshots::None,
    });

    let notes = &markdown[markdown.find("## Notes").unwrap()..markdown.find("## Transcript").unwrap()];
    assert_eq!(notes, "## Notes\n\n### Agenda\n```sh\n# not a heading\n```\n#### Item\n#hashtag\n\n");
}

#[test]
fn missing_parts_say_so() {
    let mut meeting = meeting();
    meeting.ended_at = None;
    meeting.tags = Some(vec![]);
    let markdown = render_markdown(&ExportInput { meeting: &meeting, summary: None, notes: " \n", transcript: &[], display_name: "", screenshots: ExportScreenshots::None });

    assert_eq!(
        markdown,
        "# Pricing brainstorm\n\n2026-10-17 14:05 · Manual\n\n## Summary\n\n_No Summary yet._\n\n## Notes\n\n_No Notes._\n\n## Transcript\n\n_The Transcript is empty._\n"
    );
}

/// Exports the stored Meeting to a scratch file and reads it back.
fn export_to_string(store: &tiro_core::store::MeetingStore, id: &str, mode: ScreenshotsMode) -> String {
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("export.md");
    tiro_core::export::export_meeting(store, id, "Darcy", mode, &path).unwrap();
    std::fs::read_to_string(path).unwrap()
}

fn stored_meeting_with_screenshots() -> (tempfile::TempDir, tiro_core::store::MeetingStore, tiro_core::store::Meeting) {
    let dir = tempfile::tempdir().unwrap();
    let store = tiro_core::store::MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("i.sqlite")).unwrap();
    let meeting = store.create("Teams", DateTime::parse_from_rfc3339("2026-10-20T09:03:00-04:00").unwrap()).unwrap();
    store.append_segments(&meeting, &transcript()[..2]).unwrap();
    let shots = meeting.dir.join("screenshots");
    std::fs::create_dir(&shots).unwrap();
    std::fs::write(shots.join("00000-000000500.webp"), b"first").unwrap();
    std::fs::write(shots.join("00001-000003500.webp"), b"second").unwrap();
    (dir, store, meeting)
}

#[test]
fn exporting_with_a_folder_copies_the_screenshots_next_to_the_markdown() {
    let (_dir, store, meeting) = stored_meeting_with_screenshots();
    let out = tempfile::tempdir().unwrap();
    let folder = out.path().join("Weekly sync-screenshots");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("00000-000000500.webp"), b"stale").unwrap();
    std::fs::write(folder.join("keep.txt"), b"mine").unwrap();
    let path = out.path().join("Weekly sync.md");

    tiro_core::export::export_meeting(&store, &meeting.id, "Darcy", ScreenshotsMode::Folder, &path).unwrap();

    let markdown = std::fs::read_to_string(&path).unwrap();
    assert!(
        markdown.ends_with(
            "## Transcript\n\n\
             ![Screenshot 1 at 00:00](Weekly%20sync-screenshots/00000-000000500.webp)\n\n\
             **[00:01] Others:** Shall we start?\n\n\
             ![Screenshot 2 at 00:03](Weekly%20sync-screenshots/00001-000003500.webp)\n\n\
             **[00:04] Darcy:** Sure.\n"
        ),
        "{markdown}"
    );
    assert_eq!(std::fs::read(folder.join("00000-000000500.webp")).unwrap(), b"first", "same-named files are overwritten");
    assert_eq!(std::fs::read(folder.join("00001-000003500.webp")).unwrap(), b"second");
    assert_eq!(std::fs::read(folder.join("keep.txt")).unwrap(), b"mine", "other files are left alone");
}

#[test]
fn exporting_embedded_inlines_each_screenshot_and_writes_no_folder() {
    let (_dir, store, meeting) = stored_meeting_with_screenshots();
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("Weekly sync.md");

    tiro_core::export::export_meeting(&store, &meeting.id, "Darcy", ScreenshotsMode::Embedded, &path).unwrap();

    let markdown = std::fs::read_to_string(&path).unwrap();
    assert!(markdown.contains("![Screenshot 1 at 00:00](data:image/webp;base64,Zmlyc3Q=)"), "{markdown}");
    assert!(markdown.contains("![Screenshot 2 at 00:03](data:image/webp;base64,c2Vjb25k)"), "{markdown}");
    assert!(!out.path().join("Weekly sync-screenshots").exists());
}

#[test]
fn exporting_without_screenshots_leaves_them_out() {
    let (_dir, store, meeting) = stored_meeting_with_screenshots();
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("Weekly sync.md");

    tiro_core::export::export_meeting(&store, &meeting.id, "Darcy", ScreenshotsMode::None, &path).unwrap();

    assert!(!std::fs::read_to_string(&path).unwrap().contains("Screenshot"));
    assert!(!out.path().join("Weekly sync-screenshots").exists());
}

#[test]
fn exporting_the_summary_leaves_out_the_transcript_and_screenshots() {
    let (_dir, store, meeting) = stored_meeting_with_screenshots();
    std::fs::write(meeting.dir.join("notes.md"), "Ask Sam.\n").unwrap();
    std::fs::write(meeting.dir.join("summary.md"), "## TL;DR\nShort.\n").unwrap();
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("Weekly sync.md");

    tiro_core::export::export_summary(&store, &meeting.id, "Darcy", &path).unwrap();

    let markdown = std::fs::read_to_string(&path).unwrap();
    assert!(markdown.contains("## Summary\n\n### TL;DR\nShort.\n"), "{markdown}");
    assert!(markdown.ends_with("## Notes\n\nAsk Sam.\n"), "{markdown}");
    assert!(!markdown.contains("Transcript") && !markdown.contains("Screenshot"), "{markdown}");
    assert!(!out.path().join("Weekly sync-screenshots").exists());
}

#[test]
fn a_stored_meeting_is_exported_from_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = tiro_core::store::MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("i.sqlite")).unwrap();
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339("2026-10-17T14:05:00-04:00").unwrap()).unwrap();
    store.append_segments(&meeting, &transcript()[1..2]).unwrap();
    std::fs::write(meeting.dir.join("notes.md"), "Ask Sam.\n").unwrap();
    std::fs::write(meeting.dir.join("summary.md"), "## TL;DR\nShort.\n").unwrap();

    let markdown = export_to_string(&store, &meeting.id, ScreenshotsMode::None);

    assert!(markdown.starts_with("# Manual — 2026-10-17 14:05\n"), "{markdown}");
    assert!(markdown.contains("## Summary\n\n### TL;DR\nShort.\n"), "{markdown}");
    assert!(markdown.contains("## Notes\n\nAsk Sam.\n"), "{markdown}");
    assert!(markdown.ends_with("## Transcript\n\n**[00:04] Darcy:** Sure.\n"), "{markdown}");
}

#[test]
fn the_export_leaves_out_fillers_and_tags_but_the_stored_transcript_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = tiro_core::store::MeetingStore::open(&dir.path().join("meetings"), &dir.path().join("i.sqlite")).unwrap();
    let meeting = store.create("Manual", DateTime::parse_from_rfc3339("2026-10-17T14:05:00-04:00").unwrap()).unwrap();
    let raw = vec![
        Segment { track: Track::Others, start: 1.0, end: 2.0, text: "Um, shall we start? [clears throat]".into(), speaker: None },
        Segment { track: Track::You, start: 3.0, end: 4.0, text: "[laughs]".into(), speaker: None },
        Segment { track: Track::You, start: 5.0, end: 6.0, text: "Sure, uh, go.".into(), speaker: None },
    ];
    store.append_segments(&meeting, &raw).unwrap();

    let markdown = export_to_string(&store, &meeting.id, ScreenshotsMode::None);

    assert!(
        markdown.ends_with("## Transcript\n\n**[00:01] Others:** Shall we start?\n\n**[00:05] Darcy:** Sure go.\n"),
        "{markdown}"
    );
    assert_eq!(store.transcript(&meeting).unwrap(), raw, "transcript.jsonl stays verbatim");
}

fn screenshot(index: u32, ms: u64) -> Screenshot {
    Screenshot { index, ms, path: PathBuf::from("/nowhere/screenshots").join(tiro_core::screenshots::file_name(index, ms)) }
}

fn transcript_section(markdown: &str) -> &str {
    &markdown[markdown.find("## Transcript").unwrap()..]
}

#[test]
fn screenshots_in_a_folder_are_linked_at_their_place_in_the_transcript() {
    let meeting = meeting();
    let transcript = transcript();
    let screenshots = [screenshot(0, 0), screenshot(1, 3_000), screenshot(2, 4_000), screenshot(3, 4_000_000)];
    let markdown = render_markdown(&ExportInput {
        meeting: &meeting,
        summary: Some("Short."),
        notes: "Ask Sam.",
        transcript: &transcript,
        display_name: "Darcy",
        screenshots: ExportScreenshots::Folder { folder: "Pricing brainstorm-screenshots", screenshots: &screenshots },
    });

    assert_eq!(
        transcript_section(&markdown),
        "## Transcript\n\
         \n\
         ![Screenshot 1 at 00:00](Pricing%20brainstorm-screenshots/00000-000000000.webp)\n\
         \n\
         **[00:01] Others:** Shall we start?\n\
         \n\
         ![Screenshot 2 at 00:03](Pricing%20brainstorm-screenshots/00001-000003000.webp)\n\
         \n\
         **[00:04] Darcy:** Sure.\n\
         \n\
         ![Screenshot 3 at 00:04](Pricing%20brainstorm-screenshots/00002-000004000.webp)\n\
         \n\
         **[1:02:05] Others (Speaker 1):** Thanks all.\n\
         \n\
         ![Screenshot 4 at 1:06:40](Pricing%20brainstorm-screenshots/00003-004000000.webp)\n"
    );
    assert!(markdown.contains("## Summary\n\nShort.\n\n## Notes\n\nAsk Sam.\n\n## Transcript"), "{markdown}");
}

#[test]
fn folder_links_escape_characters_that_would_break_a_markdown_link() {
    let meeting = meeting();
    let screenshots = [screenshot(0, 0)];
    let markdown = render_markdown(&ExportInput {
        meeting: &meeting,
        summary: None,
        notes: "",
        transcript: &[],
        display_name: "",
        screenshots: ExportScreenshots::Folder { folder: "Q3 (draft) <50%>-screenshots", screenshots: &screenshots },
    });
    assert!(
        markdown.contains("](Q3%20%28draft%29%20%3C50%25%3E-screenshots/00000-000000000.webp)"),
        "{markdown}"
    );
}

#[test]
fn embedded_screenshots_are_data_urls_and_show_even_without_a_transcript() {
    let meeting = meeting();
    let screenshots = [(screenshot(0, 3_000), b"RIFF".to_vec())];
    let markdown = render_markdown(&ExportInput {
        meeting: &meeting,
        summary: None,
        notes: "",
        transcript: &[],
        display_name: "",
        screenshots: ExportScreenshots::Embedded(&screenshots),
    });
    assert_eq!(transcript_section(&markdown), "## Transcript\n\n![Screenshot 1 at 00:03](data:image/webp;base64,UklGRg==)\n");
}

#[test]
fn without_a_display_name_the_user_is_you() {
    let meeting = meeting();
    let transcript = transcript();
    let markdown =
        render_markdown(&ExportInput { meeting: &meeting, summary: None, notes: "", transcript: &transcript, display_name: "", screenshots: ExportScreenshots::None });
    assert!(markdown.contains("**[00:04] You:** Sure."), "{markdown}");
}

#[test]
fn the_save_dialog_starts_in_the_last_export_folder() {
    let last = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let path = default_path("Weekly sync", Some(last.path()), downloads.path());
    assert_eq!(path, last.path().join("Weekly sync.md"));
}

#[test]
fn the_save_dialog_starts_in_downloads_before_the_first_export_or_when_the_last_folder_is_gone() {
    let downloads = tempfile::tempdir().unwrap();
    let gone = downloads.path().join("deleted");
    assert_eq!(default_path("Weekly sync", None, downloads.path()), downloads.path().join("Weekly sync.md"));
    assert_eq!(default_path("Weekly sync", Some(&gone), downloads.path()), downloads.path().join("Weekly sync.md"));
}

#[test]
fn a_name_typed_without_an_extension_is_saved_as_markdown() {
    assert_eq!(markdown_path(Path::new("/tmp/out/Weekly sync")), PathBuf::from("/tmp/out/Weekly sync.md"));
}

#[test]
fn a_name_with_an_extension_is_saved_as_typed() {
    assert_eq!(markdown_path(Path::new("/tmp/out/notes.md")), PathBuf::from("/tmp/out/notes.md"));
    assert_eq!(markdown_path(Path::new("/tmp/out/notes.txt")), PathBuf::from("/tmp/out/notes.txt"));
}

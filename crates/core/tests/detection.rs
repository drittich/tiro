use std::time::{Duration, Instant};

use tiro_core::detection::{meeting_window, Action, Detector, Event, MeetingApp, StreamProps, Window};

/// Drives a Detector on a fake clock, collecting every Action it asks for.
struct Harness {
    detector: Detector,
    start: Instant,
    now: Instant,
    actions: Vec<Action>,
}

impl Harness {
    fn new() -> Self {
        let start = Instant::now();
        Self { detector: Detector::new(), start, now: start, actions: Vec::new() }
    }

    fn send(&mut self, event: Event) {
        self.actions.extend(self.detector.handle(event, self.now));
    }

    /// Advances the clock to `secs` after the start, ticking every 100 ms as the driver does.
    fn at(&mut self, secs: f64) {
        let target = self.start + Duration::from_secs_f64(secs);
        while self.now < target {
            self.now = (self.now + Duration::from_millis(100)).min(target);
            self.actions.extend(self.detector.tick(self.now));
        }
    }

    fn take(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }
}

fn native(id: u32, binary: &str) -> Event {
    Event::StreamOpened { id, props: StreamProps { binary: Some(binary.into()), app_name: None } }
}

#[test]
fn a_meeting_app_stream_prompts_once_it_has_been_open_ten_seconds() {
    let mut h = Harness::new();
    h.send(native(7, "teams-for-linux"));
    h.at(9.8);
    assert_eq!(h.take(), vec![]);
    h.at(10.0);
    assert_eq!(h.take(), vec![Action::ShowStartPrompt(MeetingApp::Teams)]);
    h.at(30.0);
    assert_eq!(h.take(), vec![], "prompts only once");
}

#[test]
fn a_stream_that_closes_within_ten_seconds_never_prompts() {
    let mut h = Harness::new();
    h.send(native(7, "zoom"));
    h.at(6.0);
    h.send(Event::StreamClosed { id: 7 });
    h.send(native(8, "zoom"));
    h.at(15.0);
    assert_eq!(h.take(), vec![], "the reopened stream restarts the 10 s");
    h.at(16.0);
    assert_eq!(h.take(), vec![Action::ShowStartPrompt(MeetingApp::Zoom)]);
}

fn windows(list: &[(&str, &str)]) -> Event {
    Event::Windows(list.iter().map(|&(class, caption)| window(class, caption)).collect())
}

fn window(class: &str, caption: &str) -> Window {
    Window { id: format!("{{{class}: {caption}}}"), class: class.into(), caption: caption.into() }
}

#[test]
fn a_browser_stream_prompts_only_once_a_window_of_that_browser_shows_a_meeting() {
    let mut h = Harness::new();
    h.send(native(4, "firefox"));
    assert!(h.detector.wants_windows());
    h.send(windows(&[("firefox", "Inbox — Mozilla Firefox"), ("org.kde.konsole", "Meet - notes — Konsole")]));
    h.at(30.0);
    assert_eq!(h.take(), vec![], "no meeting window yet");
    h.send(windows(&[("firefox", "Meet - abc-defg-hij — Mozilla Firefox")]));
    assert_eq!(h.take(), vec![Action::ShowStartPrompt(MeetingApp::Meet)]);
}

#[test]
fn a_matching_window_still_waits_for_the_stream_to_be_open_ten_seconds() {
    let mut h = Harness::new();
    h.send(Event::StreamOpened {
        id: 9,
        props: StreamProps { binary: Some("msedge".into()), app_name: Some("Microsoft Edge".into()) },
    });
    h.send(windows(&[(
        "msedge-ompifgpmddkgmclendfeacglnodjjndh-Default",
        "Microsoft Teams (PWA) - (25) Chat | Alfred Oo | Microsoft Teams",
    )]));
    h.at(9.5);
    assert_eq!(h.take(), vec![]);
    h.at(10.0);
    assert_eq!(h.take(), vec![Action::ShowStartPrompt(MeetingApp::Teams)]);
}

#[test]
fn a_meeting_window_in_another_browser_does_not_confirm_the_stream() {
    let mut h = Harness::new();
    h.send(native(4, "chrome"));
    h.send(windows(&[("firefox", "Zoom Meeting — Mozilla Firefox"), ("google-chrome", "Docs - Google Chrome")]));
    h.at(20.0);
    assert_eq!(h.take(), vec![]);
    h.send(windows(&[("google-chrome", "Zoom Meeting - Google Chrome")]));
    assert_eq!(h.take(), vec![Action::ShowStartPrompt(MeetingApp::Zoom)]);
}

#[test]
fn windows_are_only_wanted_while_a_browser_stream_is_unconfirmed() {
    let mut h = Harness::new();
    assert!(!h.detector.wants_windows());
    h.send(native(1, "teams-for-linux"));
    assert!(!h.detector.wants_windows());
    h.send(native(2, "waterfox-bin"));
    assert!(h.detector.wants_windows());
    h.send(windows(&[("waterfox", "Meeting | Microsoft Teams — Waterfox")]));
    assert!(!h.detector.wants_windows());
}

/// A native Teams stream open long enough that the start prompt is showing.
fn start_prompt_showing() -> Harness {
    let mut h = Harness::new();
    h.send(native(7, "teams-for-linux"));
    h.at(10.0);
    assert_eq!(h.take(), vec![Action::ShowStartPrompt(MeetingApp::Teams)]);
    h
}

#[test]
fn accepting_the_start_prompt_starts_recording_with_the_detected_app() {
    let mut h = start_prompt_showing();
    h.send(Event::Accept);
    assert_eq!(h.take(), vec![Action::StartRecording(MeetingApp::Teams)]);
}

#[test]
fn declining_never_records_and_does_not_ask_again_for_the_same_meeting() {
    let mut h = start_prompt_showing();
    h.send(Event::Decline);
    h.send(native(8, "teams-for-linux"));
    h.at(120.0);
    assert_eq!(h.take(), vec![]);

    h.send(Event::StreamClosed { id: 7 });
    h.send(Event::StreamClosed { id: 8 });
    h.send(native(9, "zoom"));
    h.at(130.0);
    assert_eq!(h.take(), vec![Action::ShowStartPrompt(MeetingApp::Zoom)], "a later meeting asks again");
}

#[test]
fn the_start_prompt_goes_away_when_the_stream_ends_unanswered() {
    let mut h = start_prompt_showing();
    h.send(Event::StreamClosed { id: 7 });
    assert_eq!(h.take(), vec![Action::DismissPrompt]);
    h.send(Event::Accept);
    assert_eq!(h.take(), vec![], "a late answer to a dismissed prompt does nothing");
}

#[test]
fn starting_a_recording_from_the_tray_dismisses_the_start_prompt() {
    let mut h = start_prompt_showing();
    h.send(Event::Recording(true));
    assert_eq!(h.take(), vec![Action::DismissPrompt]);
}

#[test]
fn no_start_prompt_while_already_recording() {
    let mut h = Harness::new();
    h.send(Event::Recording(true));
    h.send(native(7, "zoom"));
    h.at(60.0);
    assert_eq!(h.take(), vec![]);
}

/// Recording a detected Teams meeting whose stream has just ended, so the stop prompt is showing (at 20 s).
fn stop_prompt_showing() -> Harness {
    let mut h = start_prompt_showing();
    h.send(Event::Accept);
    h.send(Event::Recording(true));
    h.take();
    h.at(20.0);
    h.send(Event::StreamClosed { id: 7 });
    assert_eq!(h.take(), vec![Action::ShowStopPrompt]);
    h
}

#[test]
fn an_unanswered_stop_prompt_stops_recording_after_sixty_seconds() {
    let mut h = stop_prompt_showing();
    h.at(79.9);
    assert_eq!(h.take(), vec![]);
    h.at(80.0);
    assert_eq!(h.take(), vec![Action::DismissPrompt, Action::StopRecording]);
    h.at(200.0);
    assert_eq!(h.take(), vec![]);
}

#[test]
fn accepting_the_stop_prompt_stops_recording_at_once() {
    let mut h = stop_prompt_showing();
    h.send(Event::Accept);
    assert_eq!(h.take(), vec![Action::StopRecording]);
    h.at(200.0);
    assert_eq!(h.take(), vec![]);
}

#[test]
fn declining_the_stop_prompt_keeps_recording() {
    let mut h = stop_prompt_showing();
    h.send(Event::Decline);
    h.at(200.0);
    assert_eq!(h.take(), vec![]);
}

#[test]
fn the_stop_prompt_goes_away_if_the_meeting_stream_comes_back() {
    let mut h = stop_prompt_showing();
    h.at(25.0);
    h.send(native(8, "teams-for-linux"));
    assert_eq!(h.take(), vec![Action::DismissPrompt]);
    h.at(200.0);
    assert_eq!(h.take(), vec![], "no auto-stop, and no start prompt while recording");
}

#[test]
fn stopping_from_the_tray_dismisses_the_stop_prompt() {
    let mut h = stop_prompt_showing();
    h.send(Event::Recording(false));
    assert_eq!(h.take(), vec![Action::DismissPrompt]);
    h.at(200.0);
    assert_eq!(h.take(), vec![]);
}

#[test]
fn stopping_mid_meeting_does_not_ask_to_record_the_same_meeting_again() {
    let mut h = start_prompt_showing();
    h.send(Event::Accept);
    h.send(Event::Recording(true));
    h.at(30.0);
    h.send(Event::Recording(false));
    h.at(120.0);
    assert_eq!(h.take(), vec![Action::StartRecording(MeetingApp::Teams)]);
}

#[test]
fn a_prompted_recording_asks_to_stop_when_its_meeting_ends_before_recording_begins() {
    let mut h = start_prompt_showing();
    h.send(Event::Accept);
    h.send(Event::StreamClosed { id: 7 });
    assert_eq!(h.take(), vec![Action::StartRecording(MeetingApp::Teams)]);
    h.send(Event::Recording(true));
    assert_eq!(h.take(), vec![Action::ShowStopPrompt]);
    h.at(80.0);
    assert_eq!(h.take(), vec![Action::DismissPrompt, Action::StopRecording]);
}

#[test]
fn a_prompted_recording_asks_to_stop_when_a_reopened_browser_stream_ends_unconfirmed() {
    let mut h = Harness::new();
    h.send(native(4, "firefox"));
    h.send(windows(&[("firefox", "Meet - abc-defg-hij — Mozilla Firefox")]));
    h.at(10.0);
    h.send(Event::Accept);
    h.send(Event::Recording(true));
    assert_eq!(h.take(), vec![Action::ShowStartPrompt(MeetingApp::Meet), Action::StartRecording(MeetingApp::Meet)]);

    // The browser drops its capture stream for a moment; the user keeps recording.
    h.at(20.0);
    h.send(Event::StreamClosed { id: 4 });
    h.send(Event::Decline);
    assert_eq!(h.take(), vec![Action::ShowStopPrompt]);

    // It reopens it while another tab is in front, so the new stream is never confirmed as a meeting.
    h.send(windows(&[("firefox", "Inbox — Mozilla Firefox")]));
    h.send(native(5, "firefox"));
    h.at(40.0);
    h.send(Event::StreamClosed { id: 5 });
    assert_eq!(h.take(), vec![Action::ShowStopPrompt], "the meeting it was started for has ended");
}

#[test]
fn a_manual_recording_gets_a_stop_prompt_only_when_a_meeting_stream_ends() {
    let mut h = Harness::new();
    h.send(Event::Recording(true));
    h.send(native(1, "wine64-preloader"));
    h.send(native(2, "firefox"));
    h.at(30.0);
    h.send(Event::StreamClosed { id: 1 });
    h.send(Event::StreamClosed { id: 2 });
    assert_eq!(h.take(), vec![], "neither stream was a meeting");

    h.send(native(3, "zoom"));
    h.at(40.0);
    h.send(Event::StreamClosed { id: 3 });
    assert_eq!(h.take(), vec![Action::ShowStopPrompt]);
}

#[test]
fn streams_from_other_programs_never_prompt() {
    let mut h = Harness::new();
    h.send(native(1, "wine64-preloader"));
    h.send(Event::StreamOpened { id: 2, props: StreamProps { binary: None, app_name: Some("Dirty Bomb".into()) } });
    h.send(native(3, "pw-cat"));
    h.at(60.0);
    assert_eq!(h.take(), vec![]);
}

const TEAMS_PWA: &str = "msedge-ompifgpmddkgmclendfeacglnodjjndh-Default";

fn caption_of(found: Option<&Window>) -> Option<&str> {
    found.map(|w| w.caption.as_str())
}

#[test]
fn the_meeting_window_is_the_browser_window_showing_the_meeting() {
    let windows = [
        window("firefox", "GitHub — Mozilla Firefox"),
        window(TEAMS_PWA, "Weekly sync | Microsoft Teams"),
        window("konsole", "~ : bash"),
    ];
    assert_eq!(caption_of(meeting_window(&windows, Some(MeetingApp::Teams), None)), Some("Weekly sync | Microsoft Teams"));
    assert_eq!(meeting_window(&windows, Some(MeetingApp::Meet), None), None, "only the detected app's meeting");
}

#[test]
fn a_native_apps_window_naming_a_meeting_beats_its_other_windows() {
    let windows = [
        window("teams-for-linux", "Teams"),
        window("teams-for-linux", "Weekly sync | Microsoft Teams"),
        window("teams-for-linux", "Settings"),
    ];
    assert_eq!(caption_of(meeting_window(&windows, Some(MeetingApp::Teams), None)), Some("Weekly sync | Microsoft Teams"));
}

#[test]
fn the_teams_flatpaks_window_is_the_meeting_window() {
    // The Flatpak's window class is its app id, with underscores.
    let windows = [window("com.github.IsmaelMartinez.teams_for_linux", "(30) Calendar | Microsoft Teams"), window("dolphin", "Home")];
    assert_eq!(caption_of(meeting_window(&windows, Some(MeetingApp::Teams), None)), Some("(30) Calendar | Microsoft Teams"));
}

#[test]
fn without_a_caption_to_go_on_the_most_recently_active_window_wins() {
    // KWin lists windows bottom to top.
    let windows = [window("teams-for-linux", "Chat"), window("teams-for-linux", "Call"), window("firefox", "Docs — Mozilla Firefox")];
    assert_eq!(caption_of(meeting_window(&windows, Some(MeetingApp::Teams), None)), Some("Call"));
}

#[test]
fn the_current_window_is_kept_while_it_is_open_even_once_its_caption_changes() {
    let mut windows = vec![window("firefox", "Inbox — Mozilla Firefox"), window(TEAMS_PWA, "Other call | Microsoft Teams")];
    let current = windows[0].id.clone();
    assert_eq!(caption_of(meeting_window(&windows, Some(MeetingApp::Meet), Some(&current))), Some("Inbox — Mozilla Firefox"));

    windows.remove(0);
    assert_eq!(meeting_window(&windows, Some(MeetingApp::Meet), Some(&current)), None, "gone, and no Meet window left");
}

#[test]
fn a_manual_start_takes_any_meeting_apps_window() {
    let windows = [window("firefox", "Meet - abc-defg-hij — Mozilla Firefox"), window("dolphin", "Home")];
    assert_eq!(caption_of(meeting_window(&windows, None, None)), Some("Meet - abc-defg-hij — Mozilla Firefox"));
    assert_eq!(meeting_window(&windows[1..], None, None), None);
}

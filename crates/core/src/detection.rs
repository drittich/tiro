//! Meeting detection (ADR-0003): decides when to show a Detection prompt from capture streams and window
//! titles. Pure: the PipeWire and KWin adapters feed it Events, and the driver carries out its Actions.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How long a meeting app's capture stream must stay open before the start prompt shows.
pub const DEBOUNCE: Duration = Duration::from_secs(10);
/// How long the stop prompt waits for an answer before recording stops anyway.
pub const AUTO_STOP: Duration = Duration::from_secs(60);

/// The meeting app a Meeting is detected in; its name becomes the default Meeting title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingApp {
    Teams,
    Zoom,
    Meet,
}

impl MeetingApp {
    /// The name used in the default Meeting title.
    pub fn name(self) -> &'static str {
        match self {
            MeetingApp::Teams => "Teams",
            MeetingApp::Zoom => "Zoom",
            MeetingApp::Meet => "Meet",
        }
    }
}

/// What PipeWire says about the program behind a capture stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamProps {
    /// `application.process.binary`.
    pub binary: Option<String>,
    /// `application.name`.
    pub app_name: Option<String>,
}

/// A top-level window as KWin reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// KWin's `internalId`, a `{uuid}` string; what ScreenShot2 captures a window by.
    pub id: String,
    /// `resourceClass`, e.g. `firefox` or `msedge-<app id>-Default` for a PWA.
    pub class: String,
    pub caption: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    StreamOpened { id: u32, props: StreamProps },
    StreamClosed { id: u32 },
    /// Every window currently open, replacing the previous list.
    Windows(Vec<Window>),
    /// The user accepted the prompt that is showing.
    Accept,
    /// The user declined the prompt that is showing.
    Decline,
    /// Recording started or stopped, by a prompt or from the tray.
    Recording(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    ShowStartPrompt(MeetingApp),
    /// Take down whichever prompt is showing.
    DismissPrompt,
    /// Only ever in answer to `Accept`: detection never records on its own.
    StartRecording(MeetingApp),
    ShowStopPrompt,
    StopRecording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Browser {
    Firefox,
    Chrome,
    Edge,
}

struct Stream {
    opened_at: Instant,
    /// The browser behind the stream, `None` for a native meeting app.
    browser: Option<Browser>,
    /// Known from the start for a native app; for a browser, once one of its windows shows a meeting.
    app: Option<MeetingApp>,
}

enum Prompt {
    None,
    Start(MeetingApp),
    Stop { deadline: Instant },
}

pub struct Detector {
    streams: BTreeMap<u32, Stream>,
    windows: Vec<Window>,
    prompt: Prompt,
    recording: bool,
    /// The current meeting was already prompted for, or recorded; cleared once all its streams close.
    handled: bool,
    /// The recording was started from a start prompt, so the end of any stream it could be recording asks to
    /// stop, even one never confirmed as a meeting (a browser reopening its stream while another tab is in front).
    prompted: bool,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        Self { streams: BTreeMap::new(), windows: Vec::new(), prompt: Prompt::None, recording: false, handled: false, prompted: false }
    }

    pub fn handle(&mut self, event: Event, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        match event {
            Event::StreamOpened { id, props } => {
                if let Some(stream) = classify(&props, now) {
                    self.streams.insert(id, stream);
                    self.confirm_browser_streams();
                }
            }
            Event::StreamClosed { id } => {
                let ended = self.streams.remove(&id).is_some_and(|s| s.app.is_some() || self.prompted);
                if self.streams.is_empty() {
                    self.handled = false;
                }
                if ended && self.recording {
                    actions.extend(self.prompt_to_stop(now));
                }
            }
            Event::Windows(windows) => {
                self.windows = windows;
                self.confirm_browser_streams();
            }
            Event::Accept => match std::mem::replace(&mut self.prompt, Prompt::None) {
                Prompt::Start(app) => {
                    self.handled = true;
                    self.prompted = true;
                    actions.push(Action::StartRecording(app));
                }
                Prompt::Stop { .. } => actions.push(Action::StopRecording),
                Prompt::None => {}
            },
            Event::Decline => {
                if let Prompt::Start(_) = self.prompt {
                    self.handled = true;
                }
                self.prompt = Prompt::None;
            }
            Event::Recording(recording) => {
                self.recording = recording;
                // Whatever the meeting still open, it has been dealt with: don't ask to record it again.
                self.handled |= !self.streams.is_empty();
                if !recording {
                    self.prompted = false;
                } else if self.prompted {
                    // The meeting may have ended while the recording was starting.
                    actions.extend(self.prompt_to_stop(now));
                }
            }
        }
        actions.extend(self.tick(now));
        actions
    }

    /// Checks the timers and takes down prompts that no longer apply; the driver calls it often (every 100 ms
    /// or so).
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        match self.prompt {
            Prompt::None => {
                if !self.recording && !self.handled {
                    let due = self.streams.values().filter(|s| now.duration_since(s.opened_at) >= DEBOUNCE);
                    if let Some(app) = due.filter_map(|s| s.app).next() {
                        self.prompt = Prompt::Start(app);
                        actions.push(Action::ShowStartPrompt(app));
                    }
                }
            }
            Prompt::Start(_) => {
                if self.recording || !self.in_meeting() {
                    self.prompt = Prompt::None;
                    actions.push(Action::DismissPrompt);
                }
            }
            Prompt::Stop { deadline } => {
                if !self.recording || self.in_meeting() {
                    self.prompt = Prompt::None;
                    actions.push(Action::DismissPrompt);
                } else if now >= deadline {
                    self.prompt = Prompt::None;
                    actions.extend([Action::DismissPrompt, Action::StopRecording]);
                }
            }
        }
        actions
    }

    /// Whether the driver should send fresh `Windows`: only a browser stream still waiting for a meeting
    /// window needs them.
    pub fn wants_windows(&self) -> bool {
        self.streams.values().any(|s| s.app.is_none())
    }

    /// Shows the stop prompt, unless a meeting is still on or another prompt is showing.
    fn prompt_to_stop(&mut self, now: Instant) -> Option<Action> {
        if self.in_meeting() || !matches!(self.prompt, Prompt::None) {
            return None;
        }
        self.prompt = Prompt::Stop { deadline: now + AUTO_STOP };
        Some(Action::ShowStopPrompt)
    }

    /// Whether any open stream is known to be a meeting.
    fn in_meeting(&self) -> bool {
        self.streams.values().any(|s| s.app.is_some())
    }

    /// A browser stream stays confirmed once seen with a meeting window, even if the user switches tabs.
    fn confirm_browser_streams(&mut self) {
        for stream in self.streams.values_mut().filter(|s| s.app.is_none()) {
            stream.app = self
                .windows
                .iter()
                .filter(|w| browser(&w.class) == stream.browser)
                .find_map(|w| meeting_in_caption(&w.caption));
        }
    }
}

/// The window to take Meeting screenshots of: `current` while it is still open, otherwise a window of
/// `app` (any meeting app for a manual start) chosen the way detection recognises meetings: a native app's
/// window, preferring one whose caption names a meeting, or a browser window whose caption shows one. When
/// several match, the last wins; KWin lists windows bottom to top, so that is the most recently active.
pub fn meeting_window<'a>(windows: &'a [Window], app: Option<MeetingApp>, current: Option<&str>) -> Option<&'a Window> {
    if let Some(window) = current.and_then(|id| windows.iter().find(|w| w.id == id)) {
        return Some(window);
    }
    let wanted = |found: Option<MeetingApp>| found.is_some_and(|f| app.is_none_or(|a| a == f));
    let native = |w: &&Window| wanted(native_app(&w.class));
    let in_caption = |w: &&Window| wanted(meeting_in_caption(&w.caption));
    let browser_meeting = |w: &&Window| browser(&w.class).is_some() && in_caption(w);
    windows
        .iter()
        .filter(|w| native(w) && in_caption(w))
        .last()
        .or_else(|| windows.iter().filter(|w| native(w) || browser_meeting(w)).last())
}

/// Keeps streams from native meeting apps and browsers; anything else is not a meeting.
fn classify(props: &StreamProps, now: Instant) -> Option<Stream> {
    let names: Vec<&str> = [&props.binary, &props.app_name].into_iter().filter_map(|n| n.as_deref()).collect();
    if let Some(app) = names.iter().find_map(|n| native_app(n)) {
        return Some(Stream { opened_at: now, browser: None, app: Some(app) });
    }
    let browser = names.iter().find_map(|n| browser(n))?;
    Some(Stream { opened_at: now, browser: Some(browser), app: None })
}

/// Recognises a native meeting app from a process binary, application name or window class; the Teams
/// Flatpak's window class is its app id, `com.github.IsmaelMartinez.teams_for_linux`.
fn native_app(name: &str) -> Option<MeetingApp> {
    let name = name.to_lowercase();
    if name.contains("teams-for-linux") || name.contains("teams_for_linux") {
        Some(MeetingApp::Teams)
    } else if name.starts_with("zoom") {
        Some(MeetingApp::Zoom)
    } else {
        None
    }
}

/// Recognises a browser from a process binary, application name or window class, including Firefox forks
/// and installed web apps (`msedge-<id>-Default`, `chrome-<id>-Default`).
fn browser(name: &str) -> Option<Browser> {
    let name = name.to_lowercase();
    if ["firefox", "waterfox", "librewolf", "floorp"].iter().any(|b| name.contains(b)) {
        Some(Browser::Firefox)
    } else if name.contains("msedge") || name.contains("microsoft-edge") || name.contains("microsoft edge") {
        Some(Browser::Edge)
    } else if name.contains("chrome") || name.contains("chromium") {
        Some(Browser::Chrome)
    } else {
        None
    }
}

/// Which meeting a browser window's caption shows, if any.
fn meeting_in_caption(caption: &str) -> Option<MeetingApp> {
    let lower = caption.to_lowercase();
    if lower.contains("microsoft teams") {
        Some(MeetingApp::Teams)
    } else if lower.contains("google meet") || ["Meet - ", "Meet – ", "Meet — "].iter().any(|p| caption.starts_with(p)) {
        Some(MeetingApp::Meet)
    } else if lower.split(|c: char| !c.is_alphanumeric()).any(|word| word == "zoom") {
        Some(MeetingApp::Zoom)
    } else {
        None
    }
}

//! Runs meeting detection (ADR-0003): feeds the core `Detector` with PipeWire streams, KWin windows, prompt
//! answers and the recording state, and shows its Detection prompts as desktop notifications. Tested by hand.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::Connection;
use dbus::message::MatchRule;
use tiro_core::detection::{Action, Detector, Event, Window};
use tauri::{AppHandle, Manager};

use crate::kwin::WindowLister;
use crate::recorder::{self, AppState};
use crate::{streams, tray};

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
const DBUS_TIMEOUT: Duration = Duration::from_secs(2);
/// How often to refresh the window list while a browser stream waits for a meeting window.
const WINDOW_POLL: Duration = Duration::from_secs(2);
/// `NotificationClosed` reason for a notification the user dismissed.
const DISMISSED_BY_USER: u32 = 2;

enum Input {
    Detector(Event),
    Windows(Vec<Window>),
    ActionInvoked { id: u32, action: String },
    Dismissed { id: u32 },
}

#[derive(Clone, Copy)]
enum PromptKind {
    Start,
    Stop,
}

/// Starts detection on its own thread, which lives as long as the app.
pub fn spawn(app: AppHandle) {
    std::thread::Builder::new()
        .name("meeting-detection".into())
        .spawn(move || {
            if let Err(e) = run(&app) {
                tracing::warn!("meeting detection is off: {e}");
            }
        })
        .expect("spawn meeting detection");
}

fn run(app: &AppHandle) -> Result<(), String> {
    let conn = Connection::new_session().map_err(|e| format!("no D-Bus session: {e}"))?;
    let (tx, rx) = mpsc::channel();

    let (stream_tx, stream_rx) = mpsc::channel();
    streams::watch(stream_tx);
    forward(stream_rx, tx.clone(), Input::Detector);

    let (windows_tx, windows_rx) = mpsc::channel();
    let windows = WindowLister::new(&conn, "windows", windows_tx).map_err(|e| format!("could not write the KWin script: {e}"))?;
    forward(windows_rx, tx.clone(), Input::Windows);

    let actions = tx.clone();
    conn.add_match(MatchRule::new_signal(NOTIFICATIONS, "ActionInvoked"), move |(id, action): (u32, String), _, _| {
        let _ = actions.send(Input::ActionInvoked { id, action });
        true
    })
    .map_err(|e| e.to_string())?;
    let closed = tx;
    conn.add_match(MatchRule::new_signal(NOTIFICATIONS, "NotificationClosed"), move |(id, reason): (u32, u32), _, _| {
        if reason == DISMISSED_BY_USER {
            let _ = closed.send(Input::Dismissed { id });
        }
        true
    })
    .map_err(|e| e.to_string())?;

    let mut driver = Driver { app, conn: &conn, prompt: None };
    let mut detector = Detector::new();
    let mut recording = false;
    let mut windows_requested: Option<Instant> = None;
    let mut kwin_warned = false;
    loop {
        conn.process(Duration::from_millis(100)).map_err(|e| format!("D-Bus connection lost: {e}"))?;
        let now = Instant::now();
        let mut events = Vec::new();
        for input in rx.try_iter() {
            events.extend(driver.event_for(input));
        }
        let is_recording = app.state::<AppState>().active.lock().unwrap().is_some();
        if is_recording != recording {
            recording = is_recording;
            events.push(Event::Recording(recording));
        }
        // `handle` ticks after each event, so tick on its own only when nothing happened.
        let actions: Vec<Action> = if events.is_empty() {
            detector.tick(now)
        } else {
            events.into_iter().flat_map(|e| detector.handle(e, now)).collect()
        };
        for action in actions {
            driver.carry_out(action);
        }

        if detector.wants_windows() && windows_requested.is_none_or(|at| now.duration_since(at) >= WINDOW_POLL) {
            windows_requested = Some(now);
            if let Err(e) = windows.request(&conn) {
                if !kwin_warned {
                    tracing::warn!("cannot read window titles from KWin, so browser meetings go undetected: {e}");
                    kwin_warned = true;
                }
            }
        }
    }
}

/// Passes everything from `rx` on to `tx` as an Input, on a thread of its own.
fn forward<T: Send + 'static>(rx: Receiver<T>, tx: mpsc::Sender<Input>, wrap: fn(T) -> Input) {
    std::thread::spawn(move || {
        for item in rx {
            if tx.send(wrap(item)).is_err() {
                break;
            }
        }
    });
}

struct Driver<'a> {
    app: &'a AppHandle,
    conn: &'a Connection,
    /// The notification showing a prompt, if any.
    prompt: Option<(u32, PromptKind)>,
}

impl Driver<'_> {
    /// Turns an answer to the prompt showing into Accept or Decline; answers to older notifications are ignored.
    fn event_for(&mut self, input: Input) -> Option<Event> {
        match input {
            Input::Detector(event) => Some(event),
            Input::Windows(windows) => Some(Event::Windows(windows)),
            Input::ActionInvoked { id, action } => {
                let (_, kind) = self.prompt.filter(|(shown, _)| *shown == id)?;
                self.prompt = None;
                match (kind, action.as_str()) {
                    (PromptKind::Start, "record") | (PromptKind::Stop, "stop") => Some(Event::Accept),
                    (PromptKind::Stop, "keep") => Some(Event::Decline),
                    _ => None,
                }
            }
            Input::Dismissed { id } => {
                let (_, kind) = self.prompt.filter(|(shown, _)| *shown == id)?;
                self.prompt = None;
                // Closing the start prompt says no; closing the stop prompt is no answer, so it still auto-stops.
                matches!(kind, PromptKind::Start).then_some(Event::Decline)
            }
        }
    }

    fn carry_out(&mut self, action: Action) {
        match action {
            Action::ShowStartPrompt(app) => self.show(
                PromptKind::Start,
                "Meeting detected — record?",
                &format!("{} seems to be in a meeting.", app.name()),
                &["record", "Record"],
            ),
            Action::ShowStopPrompt => self.show(
                PromptKind::Stop,
                "Stop recording?",
                "The meeting seems to have ended. Recording stops in a minute unless you keep it going.",
                &["stop", "Stop now", "keep", "Keep recording"],
            ),
            Action::DismissPrompt => self.dismiss(),
            Action::StartRecording(app) => tray::start_recording(self.app, Some(app)),
            Action::StopRecording => recorder::stop(self.app, &self.app.state::<AppState>()),
        }
    }

    fn show(&mut self, kind: PromptKind, summary: &str, body: &str, actions: &[&str]) {
        self.dismiss();
        let mut hints = PropMap::new();
        hints.insert("urgency".into(), Variant(Box::new(1u8) as Box<dyn RefArg>));
        let proxy = self.conn.with_proxy(NOTIFICATIONS, NOTIFICATIONS_PATH, DBUS_TIMEOUT);
        // Timeout 0: the prompt stays until answered or taken down.
        let shown: Result<(u32,), _> = proxy.method_call(
            NOTIFICATIONS,
            "Notify",
            ("Tiro", 0u32, "media-record", summary, body, actions.to_vec(), hints, 0i32),
        );
        match shown {
            Ok((id,)) => self.prompt = Some((id, kind)),
            Err(e) => tracing::warn!("could not show the Detection prompt: {e}"),
        }
    }

    fn dismiss(&mut self) {
        if let Some((id, _)) = self.prompt.take() {
            let proxy = self.conn.with_proxy(NOTIFICATIONS, NOTIFICATIONS_PATH, DBUS_TIMEOUT);
            let _: Result<(), _> = proxy.method_call(NOTIFICATIONS, "CloseNotification", (id,));
        }
    }
}

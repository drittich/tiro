//! Captures Meeting screenshots while recording: polls the meeting window through KWin's ScreenShot2 and
//! writes the frames the core `KeepRule` keeps to `screenshots/` in the Meeting folder. Runs on its own thread
//! and never affects the recording. Tested by hand.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dbus::arg::{prop_cast, PropMap, RefArg, Variant};
use dbus::blocking::Connection;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tiro_core::detection::{meeting_window, MeetingApp, Window};
use tiro_core::screenshots::{file_name, into_bgra, mark_thinned, thin, Frame, KeepRule, DIR, POLL, WINDOW_REFRESH};
use tiro_core::store::Meeting;

use crate::kwin::WindowLister;

pub const SCREENSHOTS_EVENT: &str = "screenshots";

const NO_WINDOW: &str = "No meeting window found, so no screenshots.";
const NOT_AUTHORIZED: &str = "Screenshots aren't authorized; reinstall Tiro or see the README.";
const NOT_AUTHORIZED_ERROR: &str = "org.kde.KWin.ScreenShot2.Error.NoAuthorized";
const FAILING: &str = "Screenshots are failing, so none are being saved. Tiro's log says why: journalctl --user -t tiro";
/// Captures or saves that fail in a row before the Live view is told; one alone is usually a window closing.
const FAILURES_BEFORE_REPORT: u32 = 3;
/// How long a capture, or a window list, may take before that poll is given up.
const TIMEOUT: Duration = Duration::from_secs(5);
const WINDOW_LIST_WAIT: Duration = Duration::from_secs(2);

/// Tells the Live view why no screenshots are being taken, or that they are again (`problem: None`).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScreenshotsEvent<'a> {
    meeting_id: &'a str,
    problem: Option<&'static str>,
}

/// Screenshot capture for the Meeting being recorded; it ends at `stop`, within one poll.
pub struct Capture {
    stop: Arc<AtomicBool>,
}

impl Capture {
    /// `app` is the meeting app detected, `None` for a manual start. `zero` is the Meeting's time zero, taken
    /// as the pipeline starts reading audio, so screenshot times line up with Segment times.
    pub fn start(handle: AppHandle, meeting_id: String, dir: &Path, app: Option<MeetingApp>, zero: Instant) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let poller = Poller {
            handle,
            meeting_id,
            meeting_dir: dir.to_owned(),
            screenshots_dir: dir.join(DIR),
            app,
            zero,
            stop: stop.clone(),
            reported: None,
            warned: false,
            failures: 0,
        };
        let spawned = std::thread::Builder::new().name("meeting-screenshots".into()).spawn(move || poller.run());
        if let Err(e) = spawned {
            tracing::warn!("could not start screenshot capture: {e}");
        }
        Self { stop }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Thins, once, the Screenshots of Meetings recorded before ticket 26's rule, many of which show cameras alone.
/// Runs on its own thread, as it decodes every Screenshot of each Meeting.
pub fn thin_in_background(meetings: Vec<Meeting>) {
    let spawned = std::thread::Builder::new().name("thin-screenshots".into()).spawn(move || {
        for meeting in meetings {
            match thin(&meeting.dir) {
                Ok(0) => {}
                Ok(deleted) => tracing::info!("thinned {deleted} screenshots from {}", meeting.id),
                Err(e) => tracing::warn!("could not thin the screenshots of {}: {e}", meeting.id),
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("could not start thinning screenshots: {e}");
    }
}

struct Poller {
    handle: AppHandle,
    meeting_id: String,
    meeting_dir: PathBuf,
    screenshots_dir: PathBuf,
    app: Option<MeetingApp>,
    zero: Instant,
    stop: Arc<AtomicBool>,
    /// What the Live view was last told.
    reported: Option<Option<&'static str>>,
    /// A failed capture is logged once per Meeting.
    warned: bool,
    /// Captures or saves failed in a row since the last one that worked.
    failures: u32,
}

impl Poller {
    fn run(mut self) {
        let conn = match Connection::new_session() {
            Ok(conn) => conn,
            Err(e) => return tracing::warn!("no screenshots: no D-Bus session: {e}"),
        };
        let (windows_tx, windows_rx) = mpsc::channel();
        let lister = match WindowLister::new(&conn, "screenshots", windows_tx) {
            Ok(lister) => lister,
            Err(e) => return tracing::warn!("no screenshots: could not write the KWin script: {e}"),
        };

        let mut rule = KeepRule::default();
        let mut index = 0u32;
        let mut window: Option<String> = None;
        let mut listed: Option<Instant> = None;
        while !self.stop.load(Ordering::Relaxed) {
            let polled = Instant::now();
            if listed.is_none_or(|at| polled.duration_since(at) >= WINDOW_REFRESH) {
                listed = Some(polled);
                if let Some(windows) = list_windows(&conn, &lister, &windows_rx) {
                    window = meeting_window(&windows, self.app, window.as_deref()).map(|w| w.id.clone());
                    let problem = if window.is_none() { Some(NO_WINDOW) } else { self.failing() };
                    self.report(problem);
                }
            }

            if let Some(id) = &window {
                let at_ms = self.zero.elapsed().as_millis() as u64;
                match capture_window(&conn, id) {
                    Ok(capture) => match capture.keep(&mut rule, at_ms) {
                        None => self.succeeded(),
                        Some(webp) => {
                            // These Screenshots are kept by the current rule, so startup thinning skips them.
                            let marked = if index == 0 { mark_thinned(&self.meeting_dir) } else { Ok(()) };
                            match marked.and_then(|()| write_atomically(&self.screenshots_dir, &file_name(index, at_ms), &webp)) {
                                Ok(()) => {
                                    index += 1;
                                    self.succeeded();
                                }
                                Err(e) => self.failed(&format!("could not save a screenshot: {e}")),
                            }
                        }
                    },
                    Err(CaptureError::NotAuthorized) => {
                        self.report(Some(NOT_AUTHORIZED));
                        return;
                    }
                    Err(CaptureError::Failed(e)) => {
                        self.failed(&format!("a screenshot failed: {e}"));
                        // The window may have closed; look for the meeting window again at the next poll.
                        listed = None;
                    }
                }
            }
            std::thread::sleep(POLL.saturating_sub(polled.elapsed()));
        }
    }

    fn report(&mut self, problem: Option<&'static str>) {
        if self.reported == Some(problem) {
            return;
        }
        self.reported = Some(problem);
        let event = ScreenshotsEvent { meeting_id: &self.meeting_id, problem };
        let _ = self.handle.emit(SCREENSHOTS_EVENT, event);
    }

    /// Why the meeting window found gets no screenshots: a run of failed captures or saves, if there is one.
    fn failing(&self) -> Option<&'static str> {
        (self.failures >= FAILURES_BEFORE_REPORT).then_some(FAILING)
    }

    fn succeeded(&mut self) {
        self.failures = 0;
        self.report(None);
    }

    /// Logs the first failure of the Meeting, and tells the Live view once failures keep coming.
    fn failed(&mut self, message: &str) {
        if !std::mem::replace(&mut self.warned, true) {
            tracing::warn!("{message} (further screenshot errors in this Meeting are not logged)");
        }
        self.failures = self.failures.saturating_add(1);
        if let Some(problem) = self.failing() {
            self.report(Some(problem));
        }
    }
}

/// Asks KWin for the window list and waits for it; `None` if it doesn't come.
fn list_windows(conn: &Connection, lister: &WindowLister, rx: &mpsc::Receiver<Vec<Window>>) -> Option<Vec<Window>> {
    if let Err(e) = lister.request(conn) {
        tracing::warn!("could not list windows for screenshots: {e}");
        return None;
    }
    let deadline = Instant::now() + WINDOW_LIST_WAIT;
    while Instant::now() < deadline {
        conn.process(Duration::from_millis(50)).ok()?;
        if let Some(windows) = rx.try_iter().last() {
            return Some(windows);
        }
    }
    None
}

enum CaptureError {
    NotAuthorized,
    Failed(String),
}

impl From<io::Error> for CaptureError {
    fn from(e: io::Error) -> Self {
        CaptureError::Failed(e.to_string())
    }
}

struct Captured {
    data: Vec<u8>,
    width: usize,
    height: usize,
    stride: usize,
}

impl Captured {
    /// The frame as WebP if the rule keeps it, cropped to the window without its client-side shadow.
    fn keep(&self, rule: &mut KeepRule, at_ms: u64) -> Option<Vec<u8>> {
        let frame = Frame { data: &self.data, width: self.width, height: self.height, stride: self.stride };
        let frame = frame.crop(frame.opaque_bounds()?);
        rule.offer(frame.thumbnail(), at_ms)?;
        Some(frame.encode_webp())
    }
}

/// One ScreenShot2 capture of the window with KWin id `id`. KWin answers with the frame's size and format,
/// and writes the pixels to a pipe, which is read on a thread of its own until KWin closes it.
fn capture_window(conn: &Connection, id: &str) -> Result<Captured, CaptureError> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let read_end = unsafe { File::from_raw_fd(fds[0]) };
    let write_end = unsafe { dbus::arg::OwnedFd::new(fds[1]) };
    let reader = std::thread::spawn(move || read_to_close(read_end));

    let mut options = PropMap::new();
    for (key, on) in [("include-cursor", false), ("include-decoration", false), ("native-resolution", true)] {
        options.insert(key.into(), Variant(Box::new(on) as Box<dyn RefArg>));
    }
    let proxy = conn.with_proxy("org.kde.KWin", "/org/kde/KWin/ScreenShot2", TIMEOUT);
    // The call takes our end of the pipe with it, so the reader sees the pipe close once KWin closes its copy.
    let reply: Result<(PropMap,), dbus::Error> =
        proxy.method_call("org.kde.KWin.ScreenShot2", "CaptureWindow", (id, options, write_end));
    let data = reader.join().map_err(|_| CaptureError::Failed("the pipe reader panicked".into()))?;
    let (meta,) = reply.map_err(|e| match e.name() {
        Some(NOT_AUTHORIZED_ERROR) => CaptureError::NotAuthorized,
        _ => CaptureError::Failed(e.to_string()),
    })?;
    let mut data = data?;

    let field = |key: &str| prop_cast::<u32>(&meta, key).map(|v| *v as usize);
    let (Some(width), Some(height), Some(stride), Some(format)) = (field("width"), field("height"), field("stride"), field("format"))
    else {
        return Err(CaptureError::Failed("KWin's reply lacks the frame size".into()));
    };
    if width == 0 || height == 0 || stride < width * 4 || data.len() < stride * (height - 1) + width * 4 {
        return Err(CaptureError::Failed(format!("short frame: {} bytes for {width}×{height}", data.len())));
    }
    if !into_bgra(format as u32, &mut data, width, height, stride) {
        return Err(CaptureError::Failed(format!("unsupported image format {format}")));
    }
    Ok(Captured { data, width, height, stride })
}

/// Reads until the writer closes the pipe; gives up after `TIMEOUT`, so a failed call can't hang the reader.
fn read_to_close(mut file: File) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + TIMEOUT;
    let mut data = Vec::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "KWin did not finish sending the frame"));
        }
        let mut poll = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        match unsafe { libc::poll(&mut poll, 1, left.as_millis().max(1) as i32) } {
            0 => continue,
            n if n < 0 => {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
            _ => match file.read(&mut buffer)? {
                0 => return Ok(data),
                n => data.extend_from_slice(&buffer[..n]),
            },
        }
    }
}

/// Writes under a temporary name first, so a half-written file never looks like a screenshot.
fn write_atomically(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let partial = dir.join(format!("{name}.partial"));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(partial, dir.join(name))
}

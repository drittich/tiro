//! PipeWire capture through `pw-record` subprocesses: one per Track, raw 16 kHz mono s16le on stdout.
//! Tested by hand.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};

use tiro_core::transcript::Track;
use serde::Serialize;

pub const SAMPLE_RATE: u32 = 16_000;

/// Starts capturing a Track. You is `mic` (a PipeWire node name), or the default input when `None`;
/// Others is the default output's monitor.
pub fn spawn(track: Track, mic: Option<&str>) -> io::Result<(Child, ChildStdout)> {
    let properties = match track {
        Track::You => "{ node.name=tiro-you node.description=\"Tiro (You)\" }",
        Track::Others => {
            "{ node.name=tiro-others node.description=\"Tiro (Others)\" stream.capture.sink=true }"
        }
    };
    let mut command = Command::new("pw-record");
    command
        .args(["--raw", "--format", "s16", "--channels", "1", "--media-category", "Capture"])
        .args(["--rate", &SAMPLE_RATE.to_string()])
        .args(["-P", properties]);
    if let (Track::You, Some(mic)) = (track, mic) {
        command.args(["--target", mic]);
    }
    let mut child = command
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("could not start pw-record: {e}")))?;
    let stdout = child.stdout.take().expect("piped stdout");
    Ok((child, stdout))
}

const ECHO_CANCEL_PREFIX: &str = "tiro-echo-cancel";

/// A running echo-cancel instance and the source it provides.
pub struct EchoCancel {
    pub process: Child,
    /// Unique to this app process, so a source left behind by a crashed run is never mistaken for it.
    pub source: String,
}

/// Starts a private PipeWire instance running WebRTC echo-cancel on `mic` (or the default input), using the
/// default output's monitor as the echo reference so playback needs no rerouting. The source lives as long as
/// the returned process. Returns `None`, and the You Track records the plain mic, when the module is
/// unavailable or the source does not appear.
pub fn start_echo_cancel(mic: Option<&str>) -> Option<EchoCancel> {
    let source = format!("{ECHO_CANCEL_PREFIX}-{}", std::process::id());
    let target = mic.map(|m| format!("target.object = \"{m}\"")).unwrap_or_default();
    let config = format!(
        r#"context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}}
context.modules = [
    {{ name = libpipewire-module-protocol-native }}
    {{ name = libpipewire-module-client-node }}
    {{ name = libpipewire-module-adapter }}
    {{ name = libpipewire-module-echo-cancel
        args = {{
            library.name = aec/libspa-aec-webrtc
            monitor.mode = true
            audio.channels = 1
            capture.props = {{ node.name = "{source}-capture" {target} }}
            source.props = {{ node.name = "{source}" node.description = "Tiro (echo-cancelled mic)" }}
            sink.props = {{ node.name = "{source}-reference" }}
        }}
    }}
]
"#
    );
    let path = std::env::temp_dir().join(format!("{source}.conf"));
    if let Err(e) = std::fs::write(&path, config) {
        unavailable(&format!("could not write {}: {e}", path.display()));
        return None;
    }
    let mut command = Command::new("pipewire");
    command.arg("-c").arg(&path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    exit_with_parent(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            unavailable(&format!("could not start pipewire: {e}"));
            return None;
        }
    };
    let problem = wait_for_source(&mut child, &source);
    let _ = std::fs::remove_file(&path);
    let Some(problem) = problem else { return Some(EchoCancel { process: child, source }) };
    unavailable(problem);
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Has the kernel send SIGTERM to the child when this process dies, so a crash does not orphan it. Unlike
/// pw-record, which exits once its stdout pipe breaks, nothing else would tell it.
///
/// The signal actually follows the spawning *thread*. Callers run on the async runtime's worker threads or on
/// their own thread, which live as long as the app; spawning from a short-lived thread (e.g. `spawn_blocking`)
/// would kill the child early.
pub fn exit_with_parent(command: &mut Command) {
    let parent = std::process::id();
    // SAFETY: only async-signal-safe calls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(io::Error::last_os_error());
            }
            // The parent may have died before prctl took effect.
            if libc::getppid() as u32 != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
}

fn unavailable(problem: &str) {
    tracing::warn!("echo-cancel unavailable, recording the plain mic: {problem}");
}

/// Waits up to 3 s for the echo-cancel source; returns what went wrong if it never appears.
fn wait_for_source(child: &mut Child, source: &str) -> Option<&'static str> {
    for _ in 0..30 {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return Some("the echo-cancel module did not load");
        }
        if source_exists(source) {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Some("its source never appeared")
}

fn source_exists(name: &str) -> bool {
    Command::new("pactl")
        .args(["list", "short", "sources"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.split('\t').nth(1) == Some(name)))
}

#[derive(Debug, Clone, Serialize)]
pub struct Microphone {
    /// PipeWire node name, passed to `pw-record --target`.
    pub name: String,
    pub description: String,
}

/// Audio inputs, excluding output monitors.
pub fn list_microphones() -> io::Result<Vec<Microphone>> {
    let output = Command::new("pactl").args(["-f", "json", "list", "sources"]).stderr(Stdio::null()).output()?;
    if !output.status.success() {
        return Err(io::Error::other("pactl could not list audio inputs"));
    }
    let sources: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
    Ok(sources
        .iter()
        .filter(|s| s["properties"]["device.class"] != "monitor")
        .filter_map(|s| {
            let name = s["name"].as_str()?;
            if name.ends_with(".monitor") || name.starts_with(ECHO_CANCEL_PREFIX) {
                return None;
            }
            let description = s["description"].as_str().filter(|d| *d != "(null)").unwrap_or(name);
            Some(Microphone { name: name.into(), description: description.into() })
        })
        .collect())
}

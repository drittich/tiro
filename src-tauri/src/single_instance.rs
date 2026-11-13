//! Keeps a second copy of Tiro from starting, since only one process may record and own the Meeting store.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

/// Takes the instance lock, or returns `None` if another Tiro holds it. Keep the `File` open for the
/// life of the process; the kernel releases the lock when it closes, even after a crash.
pub fn acquire() -> Option<File> {
    let path = lock_path();
    let file = match OpenOptions::new().create(true).truncate(false).write(true).open(&path) {
        Ok(file) => file,
        Err(e) => {
            // Better to run unguarded than not at all.
            tracing::warn!("could not open the instance lock {}: {e}", path.display());
            return Some(File::open("/dev/null").expect("open /dev/null"));
        }
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Some(file)
    } else {
        None
    }
}

/// Tells the user Tiro is already running and waits for OK.
pub fn report_already_running() {
    rfd::MessageDialog::new()
        .set_title("Tiro")
        .set_description("Tiro is already running.")
        .set_level(rfd::MessageLevel::Info)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// `$XDG_RUNTIME_DIR/tiro.lock`, falling back to the temp folder.
fn lock_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
        .join("tiro.lock")
}

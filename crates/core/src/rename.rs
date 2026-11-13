//! Tiro was called moneypenny. Moves what moneypenny left behind to where Tiro looks for it.

use std::fs;
use std::io;
use std::path::Path;

use crate::autostart;

const OLD: &str = "moneypenny";
const NEW: &str = "tiro";

/// Moves `<config_home>/moneypenny` and `<data_home>/moneypenny` (settings, API key, Meetings) to their `tiro`
/// folders, each only while the `tiro` one does not exist yet, so this runs once. A moneypenny login autostart
/// entry is replaced with Tiro's.
pub fn move_from_moneypenny(config_home: &Path, data_home: &Path) -> io::Result<()> {
    for base in [config_home, data_home] {
        let (old, new) = (base.join(OLD), base.join(NEW));
        if old.is_dir() && !new.exists() {
            fs::rename(&old, &new)?;
        }
    }
    let autostart_dir = config_home.join("autostart");
    let old_entry = autostart_dir.join(format!("{OLD}.desktop"));
    if autostart::is_enabled(&old_entry) {
        autostart::set_enabled(&autostart_dir.join(format!("{NEW}.desktop")), true)?;
        autostart::set_enabled(&old_entry, false)?;
    }
    Ok(())
}

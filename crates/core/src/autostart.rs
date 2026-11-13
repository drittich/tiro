//! Login autostart: an XDG autostart entry whose presence is the setting.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::xdg;

/// Started at login, Tiro goes straight to the tray instead of opening its window.
const ENTRY: &str = "[Desktop Entry]
Type=Application
Name=Tiro
Comment=Records, transcribes and summarizes online meetings
Exec=tiro --hidden
Icon=tiro
Terminal=false
X-GNOME-Autostart-enabled=true
";

/// `$XDG_CONFIG_HOME/autostart/tiro.desktop`, defaulting to `~/.config/autostart/tiro.desktop`.
pub fn default_autostart_path() -> PathBuf {
    xdg::config_home().join("autostart").join("tiro.desktop")
}

pub fn is_enabled(path: &Path) -> bool {
    path.is_file()
}

/// Writes the entry atomically when `enabled`; otherwise removes it if it is there.
pub fn set_enabled(path: &Path, enabled: bool) -> std::io::Result<()> {
    if !enabled {
        return match fs::remove_file(path) {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    xdg::write_atomic(path, ENTRY.as_bytes())
}

//! Where files go: the XDG base directories, and replacing a file so a crash mid-write keeps the old version.

use std::fs;
use std::path::{Path, PathBuf};

/// `$HOME`.
pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// `$XDG_CONFIG_HOME`, defaulting to `~/.config`.
pub fn config_home() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME`, defaulting to `~/.local/share`.
pub fn data_home() -> PathBuf {
    base_dir("XDG_DATA_HOME", ".local/share")
}

/// The variable's path if it is absolute (as the XDG spec requires), otherwise `~/<default>`.
fn base_dir(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(default))
}

/// Replaces `path` with `contents` by writing `<path>.tmp` and renaming it over `path`.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    fs::write(&tmp, contents)?;
    fs::rename(tmp, path)
}

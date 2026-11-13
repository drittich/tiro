//! The OpenRouter API key file: the only place the key lives.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum KeyError {
    #[error("no OpenRouter API key: set one in Settings (it is stored in {0})")]
    Missing(PathBuf),
    #[error("{path} is readable by other users (mode {mode:o}); run `chmod 600 {path}`", path = .path.display())]
    InsecurePermissions { path: PathBuf, mode: u32 },
    #[error("could not access the key file: {0}")]
    Io(String),
}

/// `$XDG_CONFIG_HOME/tiro/openrouter.key`, defaulting to `~/.config/tiro/openrouter.key`.
pub fn default_key_path() -> PathBuf {
    crate::xdg::config_home().join("tiro").join("openrouter.key")
}

/// Reads the key, refusing a file that group or others can access.
pub fn read_key(path: &Path) -> Result<String, KeyError> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(KeyError::Missing(path.to_owned())),
        Err(e) => return Err(KeyError::Io(e.to_string())),
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(KeyError::InsecurePermissions { path: path.to_owned(), mode });
    }
    let key = fs::read_to_string(path).map_err(|e| KeyError::Io(e.to_string()))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(KeyError::Missing(path.to_owned()));
    }
    Ok(key.to_owned())
}

/// Writes the key with mode 0600, replacing any existing file atomically.
pub fn write_key(path: &Path, key: &str) -> Result<(), KeyError> {
    let io = |e: std::io::Error| KeyError::Io(e.to_string());
    if let Some(parent) = path.parent() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(parent).map_err(io)?;
    }
    let tmp = path.with_extension("key.tmp");
    let _ = fs::remove_file(&tmp);
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(io)?;
    file.write_all(format!("{}\n", key.trim()).as_bytes()).map_err(io)?;
    file.sync_all().map_err(io)?;
    fs::rename(&tmp, path).map_err(io)
}

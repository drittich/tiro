use std::fs;
use std::os::unix::fs::PermissionsExt;

use tiro_core::keyfile::{read_key, write_key, KeyError};

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_key_is_read_with_surrounding_whitespace_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openrouter.key");
    fs::write(&path, "  sk-or-v1-abc\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(read_key(&path).unwrap(), "sk-or-v1-abc");
}

#[test]
fn a_key_readable_by_group_or_others_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openrouter.key");
    fs::write(&path, "sk-or-v1-abc").unwrap();

    for loose in [0o640, 0o604, 0o644] {
        fs::set_permissions(&path, fs::Permissions::from_mode(loose)).unwrap();
        assert_eq!(read_key(&path), Err(KeyError::InsecurePermissions { path: path.clone(), mode: loose }));
    }
}

#[test]
fn a_missing_or_empty_key_file_is_reported_as_such() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("openrouter.key");
    assert_eq!(read_key(&path), Err(KeyError::Missing(path.clone())));

    fs::write(&path, " \n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read_key(&path), Err(KeyError::Missing(path.clone())));
}

#[test]
fn writing_the_key_creates_a_0600_file_and_tightens_an_existing_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config/tiro/openrouter.key");

    write_key(&path, " sk-or-v1-new \n").unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(read_key(&path).unwrap(), "sk-or-v1-new");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    write_key(&path, "sk-or-v1-newer").unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(read_key(&path).unwrap(), "sk-or-v1-newer");
}

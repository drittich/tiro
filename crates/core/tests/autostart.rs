use std::fs;

use tiro_core::autostart::{is_enabled, set_enabled};

#[test]
fn turning_autostart_on_writes_a_desktop_entry_that_starts_in_the_tray() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config/autostart/tiro.desktop");
    assert!(!is_enabled(&path));

    set_enabled(&path, true).unwrap();

    assert!(is_enabled(&path));
    let entry = fs::read_to_string(&path).unwrap();
    assert!(entry.starts_with("[Desktop Entry]\n"), "{entry}");
    assert!(entry.lines().any(|l| l == "Type=Application"), "{entry}");
    assert!(entry.lines().any(|l| l == "Exec=tiro --hidden"), "{entry}");
}

#[test]
fn turning_autostart_off_removes_the_entry_and_is_harmless_when_already_off() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("autostart/tiro.desktop");

    set_enabled(&path, true).unwrap();
    set_enabled(&path, false).unwrap();
    assert!(!path.exists());
    assert!(!is_enabled(&path));

    set_enabled(&path, false).unwrap();
    assert!(!is_enabled(&path));
}

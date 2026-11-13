use std::fs;

use tiro_core::rename::move_from_moneypenny;

#[test]
fn moneypenny_settings_and_meetings_move_to_tiro() {
    let dir = tempfile::tempdir().unwrap();
    let (config, data) = (dir.path().join("config"), dir.path().join("data"));
    fs::create_dir_all(config.join("moneypenny")).unwrap();
    fs::write(config.join("moneypenny/settings.json"), "{}").unwrap();
    fs::create_dir_all(data.join("moneypenny/meetings/m1")).unwrap();

    move_from_moneypenny(&config, &data).unwrap();

    assert_eq!(fs::read_to_string(config.join("tiro/settings.json")).unwrap(), "{}");
    assert!(data.join("tiro/meetings/m1").is_dir());
    assert!(!config.join("moneypenny").exists());
    assert!(!data.join("moneypenny").exists());
}

#[test]
fn existing_tiro_folders_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (config, data) = (dir.path().join("config"), dir.path().join("data"));
    fs::create_dir_all(config.join("moneypenny")).unwrap();
    fs::write(config.join("moneypenny/settings.json"), "old").unwrap();
    fs::create_dir_all(config.join("tiro")).unwrap();
    fs::write(config.join("tiro/settings.json"), "new").unwrap();

    move_from_moneypenny(&config, &data).unwrap();

    assert_eq!(fs::read_to_string(config.join("tiro/settings.json")).unwrap(), "new");
    assert_eq!(fs::read_to_string(config.join("moneypenny/settings.json")).unwrap(), "old");
}

#[test]
fn a_moneypenny_autostart_entry_becomes_tiros() {
    let dir = tempfile::tempdir().unwrap();
    let (config, data) = (dir.path().join("config"), dir.path().join("data"));
    fs::create_dir_all(config.join("autostart")).unwrap();
    fs::write(config.join("autostart/moneypenny.desktop"), "Exec=moneypenny --hidden\n").unwrap();

    move_from_moneypenny(&config, &data).unwrap();

    assert!(!config.join("autostart/moneypenny.desktop").exists());
    let entry = fs::read_to_string(config.join("autostart/tiro.desktop")).unwrap();
    assert!(entry.lines().any(|l| l == "Exec=tiro --hidden"), "{entry}");
}

#[test]
fn nothing_to_move_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    move_from_moneypenny(&dir.path().join("config"), &dir.path().join("data")).unwrap();
    assert!(!dir.path().join("config/autostart/tiro.desktop").exists());
}

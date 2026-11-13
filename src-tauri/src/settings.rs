//! `~/.config/tiro/settings.json`. The API key is not stored here: it lives only in the key file.

use std::path::{Path, PathBuf};

use tiro_core::ask::DEFAULT_ASK_MODEL;
use tiro_core::embeddings::DEFAULT_EMBEDDING_MODEL;
use tiro_core::export::ScreenshotsMode;
use tiro_core::openrouter::DEFAULT_STT_MODEL;
use tiro_core::summary::{SummarySettings, DEFAULT_PROMPT_TEMPLATE, DEFAULT_SUMMARY_MODEL};
use tiro_core::xdg::{self, home};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub stt_model: String,
    /// PipeWire node name of the microphone for the You Track; `None` follows the system default input.
    pub mic: Option<String>,
    pub summary_model: String,
    /// The model Passages and queries are embedded with for meaning search; blank turns meaning search off.
    pub embedding_model: String,
    /// The model that answers questions asked of the Meetings (Ask); blank turns asking off.
    pub ask_model: String,
    /// The summary prompt template; `None` uses the built-in one, so it follows updates.
    pub summary_prompt: Option<String>,
    /// How the user is named in the Summary and export (action-item owners); blank means "You".
    pub display_name: String,
    /// The folder of the last export, where the next Save dialog starts; `None` until the first export.
    pub last_export_dir: Option<PathBuf>,
    /// The last choice of how an export includes a Meeting's Screenshots, offered as the default next time.
    pub export_screenshots: ScreenshotsMode,
    /// Font family of the app's text, including Summaries and the Transcript.
    pub text_font: String,
    /// Size of the app's text in px; other sizes scale with it.
    pub text_size: u32,
    /// Font family of the Notes editor.
    pub notes_font: String,
    /// Size of the Notes editor's text in px.
    pub notes_size: u32,
    /// Light or dark colours, or whichever the system prefers.
    pub theme: Theme,
    /// Whether a recording Meeting captures screenshots of the meeting app's window. Only turned on after the
    /// user accepts the consent dialog in Settings.
    pub meeting_screenshots: bool,
}

/// The app's colours: whichever the system prefers, or always light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

pub const DEFAULT_TEXT_FONT: &str = "Inter";
pub const DEFAULT_NOTES_FONT: &str = "Noto Sans Mono";
pub const FONT_SIZES: std::ops::RangeInclusive<u32> = 8..=32;

impl Default for Settings {
    fn default() -> Self {
        Self {
            stt_model: DEFAULT_STT_MODEL.into(),
            mic: None,
            summary_model: DEFAULT_SUMMARY_MODEL.into(),
            embedding_model: DEFAULT_EMBEDDING_MODEL.into(),
            ask_model: DEFAULT_ASK_MODEL.into(),
            summary_prompt: None,
            display_name: String::new(),
            last_export_dir: None,
            export_screenshots: ScreenshotsMode::default(),
            text_font: DEFAULT_TEXT_FONT.into(),
            text_size: 15,
            notes_font: DEFAULT_NOTES_FONT.into(),
            notes_size: 14,
            theme: Theme::default(),
            meeting_screenshots: false,
        }
    }
}

impl Settings {
    pub fn summary(&self) -> SummarySettings {
        SummarySettings {
            template: self.summary_prompt.clone().unwrap_or_else(|| DEFAULT_PROMPT_TEMPLATE.into()),
            display_name: self.display_name.clone(),
        }
    }

    /// Records the folder of a file just exported to, so the next export starts there.
    pub fn remember_export(&mut self, saved: &Path) {
        self.last_export_dir = saved.parent().map(Path::to_path_buf);
    }
}

fn path() -> PathBuf {
    xdg::config_home().join("tiro").join("settings.json")
}

pub fn load() -> Settings {
    match std::fs::read_to_string(path()) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("ignoring unreadable settings: {e}");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let path = path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    xdg::write_atomic(&path, serde_json::to_string_pretty(settings)?.as_bytes())
}

/// `$XDG_DATA_HOME/tiro`, defaulting to `~/.local/share/tiro`.
pub fn data_dir() -> PathBuf {
    xdg::data_home().join("tiro")
}

/// The XDG Downloads folder (from `user-dirs.dirs`), defaulting to `~/Downloads`, or home if that is missing.
pub fn downloads_dir() -> PathBuf {
    let home = home();
    let configured = std::fs::read_to_string(xdg::config_home().join("user-dirs.dirs")).ok().and_then(|raw| {
        raw.lines().find_map(|line| {
            let value = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=")?.trim_matches('"');
            Some(PathBuf::from(value.replace("$HOME", &home.to_string_lossy())))
        })
    });
    [configured, Some(home.join("Downloads"))].into_iter().flatten().find(|p| p.is_dir()).unwrap_or(home)
}

/// Installed font families, sorted, for the font pickers. Empty when fontconfig cannot list them.
pub fn installed_fonts() -> Vec<String> {
    std::process::Command::new("fc-list")
        .args([":", "family"])
        .stderr(std::process::Stdio::null())
        .output()
        .map(|output| font_families(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

/// The primary family of each `fc-list : family` line (`Family,Localized name`), deduplicated and sorted.
fn font_families(fc_list: &str) -> Vec<String> {
    let families: std::collections::BTreeSet<String> = fc_list
        .lines()
        .filter_map(|line| line.split(',').next())
        .map(|family| family.replace('\\', "").trim().to_string())
        .filter(|family| !family.is_empty())
        .collect();
    families.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_export_remembers_its_folder_for_the_next_one() {
        let mut settings = Settings::default();
        settings.remember_export(Path::new("/home/d/Documents/meetings/Weekly sync.md"));
        let saved: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(saved.last_export_dir, Some(PathBuf::from("/home/d/Documents/meetings")));
    }

    #[test]
    fn settings_saved_before_any_export_have_no_export_folder() {
        let settings: Settings = serde_json::from_str(r#"{"displayName":"Darcy"}"#).unwrap();
        assert_eq!(settings.last_export_dir, None);
    }

    #[test]
    fn the_screenshots_export_choice_is_remembered_and_starts_as_a_folder() {
        let settings: Settings = serde_json::from_str(r#"{"displayName":"Darcy"}"#).unwrap();
        assert_eq!(settings.export_screenshots, ScreenshotsMode::Folder);
        let settings = Settings { export_screenshots: ScreenshotsMode::Embedded, ..Settings::default() };
        let saved: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(saved.export_screenshots, ScreenshotsMode::Embedded);
    }

    #[test]
    fn settings_saved_before_font_settings_use_the_default_fonts() {
        let settings: Settings = serde_json::from_str(r#"{"displayName":"Darcy"}"#).unwrap();
        assert_eq!((settings.text_font.as_str(), settings.text_size), ("Inter", 15));
        assert_eq!((settings.notes_font.as_str(), settings.notes_size), ("Noto Sans Mono", 14));
    }

    #[test]
    fn meeting_screenshots_are_off_until_turned_on() {
        let settings: Settings = serde_json::from_str(r#"{"displayName":"Darcy"}"#).unwrap();
        assert!(!settings.meeting_screenshots);
        assert!(!Settings::default().meeting_screenshots);
    }

    #[test]
    fn settings_saved_before_the_theme_setting_follow_the_system() {
        let settings: Settings = serde_json::from_str(r#"{"displayName":"Darcy","textFont":"Cantarell"}"#).unwrap();
        assert_eq!(settings.theme, Theme::System);
        assert_eq!(settings.text_font, "Cantarell");
    }

    #[test]
    fn a_chosen_theme_is_kept() {
        let saved = serde_json::to_string(&Settings { theme: Theme::Dark, ..Settings::default() }).unwrap();
        assert!(saved.contains(r#""theme":"dark""#));
        let settings: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(settings.theme, Theme::Dark);
    }

    #[test]
    fn settings_saved_before_meaning_search_use_the_default_embedding_model_and_blank_stays_off() {
        let settings: Settings = serde_json::from_str(r#"{"displayName":"Darcy"}"#).unwrap();
        assert_eq!(settings.embedding_model, DEFAULT_EMBEDDING_MODEL);
        let saved = serde_json::to_string(&Settings { embedding_model: String::new(), ..Settings::default() }).unwrap();
        let settings: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(settings.embedding_model, "");
    }

    #[test]
    fn settings_saved_before_ask_use_the_default_ask_model_and_blank_stays_off() {
        let settings: Settings = serde_json::from_str(r#"{"displayName":"Darcy"}"#).unwrap();
        assert_eq!(settings.ask_model, DEFAULT_ASK_MODEL);
        let saved = serde_json::to_string(&Settings { ask_model: String::new(), ..Settings::default() }).unwrap();
        let settings: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(settings.ask_model, "");
    }

    #[test]
    fn font_families_lists_each_primary_family_once_in_order() {
        let fc_list = "Roboto,Roboto Light\nCantarell\nRoboto\nSource Code Pro\\-ish\n\n";
        assert_eq!(font_families(fc_list), ["Cantarell", "Roboto", "Source Code Pro-ish"]);
    }
}

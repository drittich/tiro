//! The Markdown export: one file with the Summary, Notes and Transcript, rendered on demand (ADR-0005).

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::screenshots::{self, Screenshot};
use crate::store::{Meeting, MeetingStore, Result};
use crate::transcript::{cleaned, clock, display_name, speaker_label, Segment};

/// What goes into an export.
#[derive(Debug, Clone)]
pub struct ExportInput<'a> {
    pub meeting: &'a Meeting,
    /// The contents of `summary.md`, if the Meeting has been summarized.
    pub summary: Option<&'a str>,
    /// The contents of `notes.md`; blank when there are none.
    pub notes: &'a str,
    pub transcript: &'a [Segment],
    /// How the user is named in the Transcript; blank means "You".
    pub display_name: &'a str,
    pub screenshots: ExportScreenshots<'a>,
}

/// The choice offered before the Save dialog when a Meeting has Screenshots; the last one is remembered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenshotsMode {
    /// Without screenshots.
    None,
    /// In a `<md file stem>-screenshots/` folder next to the `.md`.
    #[default]
    Folder,
    /// Embedded in the `.md`.
    Embedded,
}

/// Whether and how the Meeting's Screenshots go into the Transcript section, in time order.
#[derive(Debug, Clone, Copy)]
pub enum ExportScreenshots<'a> {
    None,
    /// Linked from `folder` (a folder name next to the `.md`), under their own file names.
    Folder { folder: &'a str, screenshots: &'a [Screenshot] },
    /// Inlined as `data:` URLs, each with its WebP bytes.
    Embedded(&'a [(Screenshot, Vec<u8>)]),
}

impl ExportScreenshots<'_> {
    /// Each Screenshot's Meeting time (ms) and image URL.
    fn images(&self) -> Vec<(u64, String)> {
        match *self {
            ExportScreenshots::None => Vec::new(),
            ExportScreenshots::Folder { folder, screenshots } => {
                screenshots.iter().map(|s| (s.ms, format!("{}/{}", url_escaped(folder), s.file_name()))).collect()
            }
            ExportScreenshots::Embedded(screenshots) => screenshots
                .iter()
                .map(|(s, webp)| {
                    (s.ms, format!("data:image/webp;base64,{}", base64::engine::general_purpose::STANDARD.encode(webp)))
                })
                .collect(),
        }
    }
}

/// Renders the export. Headings in the Summary and Notes are shifted down so they nest under their section.
pub fn render_markdown(input: &ExportInput) -> String {
    let mut out = render_summary_markdown(input);
    let name = display_name(input.display_name);
    let shown = cleaned(input.transcript);
    let images = input.screenshots.images();
    let mut placed = images.iter().enumerate().map(|(n, (ms, url))| (screenshots::position(&shown, *ms), n, ms, url)).peekable();
    let mut transcript = Vec::new();
    for i in 0..=shown.len() {
        while let Some((_, n, ms, url)) = placed.next_if(|(at, ..)| *at <= i) {
            transcript.push(format!("![Screenshot {} at {}]({url})", n + 1, clock(*ms as f64 / 1000.0)));
        }
        if let Some(s) = shown.get(i) {
            transcript.push(format!("**[{}] {}:** {}", clock(s.start), speaker_label(s, name), s.text.trim()));
        }
    }
    section(&mut out, "Transcript", Some(transcript.join("\n\n")).filter(|t| !t.is_empty()), "_The Transcript is empty._");
    out
}

/// Renders the export without its Transcript section, and so without Screenshots: the title, Summary and Notes.
pub fn render_summary_markdown(input: &ExportInput) -> String {
    let m = input.meeting;
    let mut out = format!("# {}\n\n{}", m.title, m.started_at.format("%Y-%m-%d %H:%M"));
    if let Some(end) = m.ended_at {
        let minutes = ((end - m.started_at).num_seconds() as f64 / 60.0).round() as i64;
        out.push_str(&format!("–{} ({minutes} min)", end.format("%H:%M")));
    }
    out.push_str(&format!(" · {}\n", m.app));
    if let Some(tags) = m.tags.as_ref().filter(|t| !t.is_empty()) {
        out.push_str(&format!("\nTags: {}\n", tags.join(", ")));
    }

    section(&mut out, "Summary", input.summary.filter(|s| !s.trim().is_empty()).map(nest), "_No Summary yet._");
    section(&mut out, "Notes", Some(input.notes).filter(|n| !n.trim().is_empty()).map(nest), "_No Notes._");
    out
}

/// Writes the export of a stored Meeting, rendered from its files, to `path`. With [`ScreenshotsMode::Folder`] its
/// Screenshots are first copied, under their own names, to `<stem>-screenshots/` beside it, overwriting
/// same-named files and leaving others alone.
pub fn export_meeting(store: &MeetingStore, id: &str, display_name: &str, mode: ScreenshotsMode, path: &Path) -> Result<()> {
    let meeting = store.get(id)?;
    let summary = store.summary(&meeting)?;
    let notes = store.notes(&meeting)?;
    let transcript = store.transcript(&meeting)?;
    let shots = if mode == ScreenshotsMode::None { Vec::new() } else { store.screenshots(&meeting)? };
    let folder = format!("{}-screenshots", path.file_stem().unwrap_or_default().to_string_lossy());
    let embedded: Vec<(Screenshot, Vec<u8>)>;
    let screenshots = match mode {
        _ if shots.is_empty() => ExportScreenshots::None,
        ScreenshotsMode::None => unreachable!("no Screenshots are listed without them"),
        ScreenshotsMode::Folder => {
            let dir = path.with_file_name(&folder);
            std::fs::create_dir_all(&dir)?;
            for shot in &shots {
                std::fs::copy(&shot.path, dir.join(shot.file_name()))?;
            }
            ExportScreenshots::Folder { folder: &folder, screenshots: &shots }
        }
        ScreenshotsMode::Embedded => {
            embedded = shots.iter().map(|s| Ok((s.clone(), std::fs::read(&s.path)?))).collect::<Result<_>>()?;
            ExportScreenshots::Embedded(&embedded)
        }
    };
    let markdown = render_markdown(&ExportInput {
        meeting: &meeting,
        summary: summary.as_ref().map(|s| s.markdown.as_str()),
        notes: &notes,
        transcript: &transcript,
        display_name,
        screenshots,
    });
    Ok(std::fs::write(path, markdown)?)
}

/// Writes the export of a stored Meeting without its Transcript or Screenshots to `path`; nothing is written beside it.
pub fn export_summary(store: &MeetingStore, id: &str, display_name: &str, path: &Path) -> Result<()> {
    let meeting = store.get(id)?;
    let summary = store.summary(&meeting)?;
    let notes = store.notes(&meeting)?;
    let markdown = render_summary_markdown(&ExportInput {
        meeting: &meeting,
        summary: summary.as_ref().map(|s| s.markdown.as_str()),
        notes: &notes,
        transcript: &[],
        display_name,
        screenshots: ExportScreenshots::None,
    });
    Ok(std::fs::write(path, markdown)?)
}

/// Where the Save dialog starts: `<title>.md` in the last export's folder, or in `downloads` if there was none
/// or it no longer exists. Slashes and control characters in the title become `-`.
pub fn default_path(title: &str, last_dir: Option<&Path>, downloads: &Path) -> PathBuf {
    let name: String = title.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '-' } else { c }).collect();
    last_dir.filter(|d| d.is_dir()).unwrap_or(downloads).join(format!("{name}.md"))
}

/// The path chosen in the Save dialog, with `.md` added if the name was typed without an extension.
pub fn markdown_path(chosen: &Path) -> PathBuf {
    if chosen.extension().is_some() {
        chosen.to_path_buf()
    } else {
        chosen.with_extension("md")
    }
}

/// `text` with the characters that would end or break a Markdown link destination percent-encoded.
fn url_escaped(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            ' ' | '%' | '(' | ')' | '<' | '>' | '[' | ']' | '#' | '?' | '\\' => format!("%{:02X}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

fn section(out: &mut String, heading: &str, body: Option<String>, empty: &str) {
    out.push_str(&format!("\n## {heading}\n\n"));
    out.push_str(body.as_deref().map_or(empty, str::trim_end));
    out.push('\n');
}

/// Shifts ATX headings so the shallowest becomes level 3, skipping fenced code blocks.
fn nest(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut fenced = false;
    let mut levels = vec![None; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
        } else if !fenced {
            levels[i] = heading_level(line);
        }
    }
    let shift = levels.iter().flatten().min().map_or(0, |&min| 3usize.saturating_sub(min));
    lines
        .iter()
        .zip(levels)
        .map(|(line, level)| match level {
            Some(level) => format!("{}{}", "#".repeat((level + shift).min(6)), &line[level..]),
            None => (*line).to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The level of an ATX heading line (`## Title`), or `None`.
fn heading_level(line: &str) -> Option<usize> {
    let level = line.chars().take_while(|&c| c == '#').count();
    let rest = &line[level..];
    ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' '))).then_some(level)
}

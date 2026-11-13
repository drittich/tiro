//! The Summary (ADR-0006): one call to the summary model with the whole Transcript and the Notes.

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::spend::{Billed, SpendKind, SpendLedger};
use crate::store::MeetingStore;
use crate::transcript::{cleaned, clock, display_name, speaker_label, Segment};

pub const DEFAULT_SUMMARY_MODEL: &str = "deepseek/deepseek-v4.1-flash";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SummarizeError {
    /// The summary model has no zero-data-retention endpoint; the user must pick another model.
    #[error("{0}")]
    ZdrViolation(String),
    /// OpenRouter rejected the API key.
    #[error("{0}")]
    Unauthorized(String),
    /// Anything else: network trouble, server errors, an empty reply. Re-summarizing may help.
    #[error("{0}")]
    Failed(String),
}

/// The summary model boundary.
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Returns the model's reply to `prompt`, and what the request cost.
    async fn summarize(&self, prompt: &str) -> Billed<String, SummarizeError>;
}

/// The summary prompt. Settings can replace it; the placeholders are listed in `PLACEHOLDERS`.
pub const DEFAULT_PROMPT_TEMPLATE: &str = r#"You are summarizing a recorded meeting for {{display_name}}, who recorded it.

Meeting: {{title}}
Date: {{date}}
Length: {{duration}}

The Transcript below interleaves two sources. Lines labelled "{{display_name}}" are the user's own microphone; lines labelled "Others" are everyone else in the meeting. "{{display_name}}" is the user: write action items they own with their name, and write about them in the third person.

{{notes}}

Write the Summary in Markdown, using exactly this structure:

Title: <a short, specific title for this meeting, at most 8 words, on the first line>

## TL;DR
Two to four sentences: what the meeting was about and where it landed.

## Discussion
One `### <thread name>` subsection per Discussion thread: each topic the meeting explored, whether or not it led anywhere. For each thread, record the ideas raised (including half-formed ones and who raised them), the alternatives considered, the options set aside and why, and where the thread ended up. Keep the reasoning, not just the outcome.

## Key points
Facts, figures, constraints and context worth remembering.

## Decisions
What was actually decided, and by whom. Write "None." if nothing was decided.

## Action items
`- [ ] Owner: task (due date if one was given)`. Write "None." if there were none.

## Open questions
Questions raised and not answered. Write "None." if there were none.

Rules:
- Never drop exploratory discussion. Brainstorming, tangents, ideas that were floated and dropped, and alternatives that were set aside belong in Discussion even when they produced no decision or action item. Losing them is the worst failure for this Summary.
- The user's Notes take priority: where they disagree with the Transcript, follow the Notes, and make sure everything in them is reflected.
- Scale the length with the meeting. Aim for about {{length}} words in total, longer if that is what it takes to keep every Discussion thread.
- The Transcript is machine-made and may mishear words or names; correct obvious errors silently and do not invent details.
- Output only the Title line and the Markdown sections, with no preamble.

Transcript:
{{transcript}}"#;

/// The placeholders a summary prompt template can use.
pub const PLACEHOLDERS: [&str; 7] =
    ["{{title}}", "{{date}}", "{{duration}}", "{{display_name}}", "{{length}}", "{{notes}}", "{{transcript}}"];

/// Everything the summary prompt is built from.
#[derive(Debug, Clone)]
pub struct SummaryInput<'a> {
    pub title: &'a str,
    pub started_at: DateTime<FixedOffset>,
    /// How the user is named in the Transcript and in action items; blank means "You".
    pub display_name: &'a str,
    pub transcript: &'a [Segment],
    /// The contents of `notes.md`; blank when the user wrote none.
    pub notes: &'a str,
}

/// Fills `template`'s placeholders. A template without `{{transcript}}` gets the Transcript appended.
pub fn build_prompt(template: &str, input: &SummaryInput) -> String {
    let name = display_name(input.display_name);
    // Without fillers and audio-event tags, which also keeps them out of the target length.
    let shown = cleaned(input.transcript);
    let transcript = transcript_text(&shown, name);
    let words: usize = shown.iter().map(|s| s.text.split_whitespace().count()).sum();
    let minutes = (input.transcript.iter().map(|s| s.end).fold(0.0, f64::max) / 60.0).round().max(1.0) as u64;
    let notes = if input.notes.trim().is_empty() {
        "The user took no Notes for this Meeting.".to_owned()
    } else {
        format!("Below are the user's own notes, which take priority over the Transcript:\n\n{}", input.notes.trim())
    };

    let mut prompt = template
        .replace("{{title}}", input.title)
        .replace("{{date}}", &input.started_at.format("%Y-%m-%d %H:%M").to_string())
        .replace("{{duration}}", &format!("about {minutes} minutes"))
        .replace("{{display_name}}", name)
        .replace("{{length}}", &target_words(words).to_string())
        .replace("{{notes}}", &notes);
    if prompt.contains("{{transcript}}") {
        prompt = prompt.replace("{{transcript}}", &transcript);
    } else {
        prompt.push_str("\n\nTranscript:\n");
        prompt.push_str(&transcript);
    }
    prompt
}

fn transcript_text(segments: &[Segment], name: &str) -> String {
    segments.iter().map(|s| format!("[{}] {}: {}\n", clock(s.start), speaker_label(s, name), s.text)).collect()
}

/// About 15% of the Transcript's length, in steps of 50 words, between 150 and 4000.
fn target_words(transcript_words: usize) -> usize {
    let raw = transcript_words as f64 * 0.15;
    ((raw / 50.0).round() as usize * 50).clamp(150, 4000)
}

/// A generated Summary: the title the model gave, which writing it applies to the Meeting unless the user has renamed
/// it, and the Markdown sections that go in `summary.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub suggested_title: Option<String>,
    pub markdown: String,
}

/// Splits the model's reply into the `Title:` line and the rest. Tolerates a leading `<think>` block, a
/// surrounding code fence and Markdown decoration on the title line.
pub fn parse_response(reply: &str) -> Summary {
    let mut text = reply;
    if let Some(end) = text.find("</think>") {
        text = &text[end + "</think>".len()..];
    }
    let mut lines: Vec<&str> = text.trim().lines().collect();
    if lines.first().is_some_and(|l| l.trim_start().starts_with("```")) {
        lines.remove(0);
        if lines.last().is_some_and(|l| l.trim() == "```") {
            lines.pop();
        }
    }
    let first = lines.iter().position(|l| !l.trim().is_empty());
    let suggested_title = first.and_then(|i| title_of(lines[i]));
    if suggested_title.is_some() {
        lines.drain(..=first.expect("a title line"));
    }
    let body = lines.join("\n");
    let body = body.trim();
    Summary { suggested_title, markdown: if body.is_empty() { String::new() } else { format!("{body}\n") } }
}

fn title_of(line: &str) -> Option<String> {
    let line = line.trim().trim_start_matches('#').trim().replace("**", "").replace("__", "");
    let rest = line.strip_prefix("Title:").or_else(|| line.strip_prefix("title:"))?;
    let title = rest.trim().trim_matches(|c| matches!(c, '"' | '\'' | '*' | '_' | '`' | '“' | '”')).trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// The user's summary settings.
#[derive(Debug, Clone)]
pub struct SummarySettings {
    pub template: String,
    pub display_name: String,
}

/// Summarizes a Meeting from its Transcript and Notes and saves the result, replacing any earlier Summary. On
/// failure the earlier Summary is kept. What the request cost goes in `spend`, whether or not it succeeded.
pub async fn summarize_meeting(
    store: &MeetingStore,
    id: &str,
    summarizer: &dyn Summarizer,
    settings: &SummarySettings,
    spend: &SpendLedger,
) -> Result<Summary, SummarizeError> {
    let io = |e: crate::store::StoreError| SummarizeError::Failed(format!("could not read the Meeting: {e}"));
    let meeting = store.get(id).map_err(io)?;
    let transcript = store.transcript(&meeting).map_err(io)?;
    let notes = store.notes(&meeting).map_err(io)?;
    if transcript.is_empty() && notes.trim().is_empty() {
        return Err(SummarizeError::Failed("The Transcript is empty, so there is nothing to summarize.".into()));
    }
    let input = SummaryInput {
        title: &meeting.title,
        started_at: meeting.started_at,
        display_name: &settings.display_name,
        transcript: &transcript,
        notes: &notes,
    };
    let billed = summarizer.summarize(&build_prompt(&settings.template, &input)).await;
    spend.record(Some(id), SpendKind::Summary, billed.usage.as_ref());
    let reply = billed.result?;
    let summary = parse_response(&reply);
    store
        .write_summary(&meeting, &summary)
        .map_err(|e| SummarizeError::Failed(format!("could not save the Summary: {e}")))?;
    Ok(summary)
}

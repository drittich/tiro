//! Tags: the parts of the work a Meeting covered, generated from its Summary, drawn from the vocabulary of every Tag in use.

use std::path::Path;

use serde::Deserialize;

use crate::spend::{SpendKind, SpendLedger};
use crate::store::{Meeting, MeetingStore, StoreError};
use crate::summary::{SummarizeError, Summarizer};

/// Raised when the tagging rules change enough that every Meeting should be tagged again.
pub const RULES_VERSION: u32 = 2;
/// The most existing Tags the prompt lists, most used first.
pub const PROMPT_VOCABULARY: usize = 100;
/// The most Tags a Meeting gets.
pub const MAX_TAGS: usize = 5;
/// The most new Tags taken from one reply.
pub const MAX_NEW_TAGS: usize = 3;
/// Matches less confident than this are dropped.
pub const MIN_CONFIDENCE: f64 = 0.7;

#[derive(Debug, thiserror::Error)]
pub enum TagError {
    /// The request to the summary model failed.
    #[error(transparent)]
    Request(#[from] SummarizeError),
    /// The model's reply wasn't the JSON asked for.
    #[error("unexpected reply from the tagging model: {0}")]
    Reply(String),
    /// Only a Meeting with a Summary is tagged.
    #[error("the Meeting has no Summary to tag it from")]
    NoSummary,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Every Tag on these Meetings, most used first and alphabetical among equals, each spelled as first seen.
pub fn vocabulary(meetings: &[Meeting]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for tag in meetings.iter().flat_map(|m| m.tags.iter().flatten()) {
        match counts.iter_mut().find(|(t, _)| same_tag(t, tag)) {
            Some((_, n)) => *n += 1,
            None => counts.push((tag.clone(), 1)),
        }
    }
    counts.sort_by(|(a, m), (b, n)| n.cmp(m).then_with(|| a.to_lowercase().cmp(&b.to_lowercase())));
    counts.into_iter().map(|(t, _)| t).collect()
}

/// Clears every Meeting's Tags, once per [`RULES_VERSION`], so the startup backfill tags them all again under the
/// current rules, starting from an empty vocabulary. A marker file in `data_dir` records that it was done. Returns
/// whether it cleared them.
pub fn clear_outdated_tags(store: &MeetingStore, data_dir: &Path) -> Result<bool, StoreError> {
    let marker = data_dir.join(format!(".tags-v{RULES_VERSION}"));
    if marker.exists() {
        return Ok(false);
    }
    for meeting in store.list()?.into_iter().filter(|m| m.tags.is_some()) {
        store.clear_tags(&meeting.id)?;
    }
    std::fs::write(&marker, b"")?;
    Ok(true)
}

/// Tags a Meeting from its Summary, reusing the other Meetings' Tags where they fit, and saves the Tags. On failure
/// the Meeting is left as it was. What the request cost goes in `spend`, whether or not it succeeded.
pub async fn tag_meeting(
    store: &MeetingStore,
    id: &str,
    summarizer: &dyn Summarizer,
    spend: &SpendLedger,
) -> Result<Vec<String>, TagError> {
    let meeting = store.get(id)?;
    let summary = store.summary(&meeting)?.filter(|s| !s.markdown.trim().is_empty()).ok_or(TagError::NoSummary)?;
    let others: Vec<Meeting> = store.list()?.into_iter().filter(|m| m.id != id).collect();
    let vocabulary = vocabulary(&others);
    let billed = summarizer.summarize(&build_prompt(&meeting.title, &summary.markdown, &vocabulary)).await;
    spend.record(Some(id), SpendKind::Tags, billed.usage.as_ref());
    let reply = billed.result?;
    let tags = parse_response(&reply, &vocabulary)?;
    store.set_tags(id, &tags)?;
    Ok(tags)
}

/// The tagging prompt for a Meeting titled `title` with this Summary. `vocabulary` is every Tag in use, most used
/// first; only the first [`PROMPT_VOCABULARY`] go in.
pub fn build_prompt(title: &str, summary: &str, vocabulary: &[String]) -> String {
    let existing = &vocabulary[..vocabulary.len().min(PROMPT_VOCABULARY)];
    let existing = serde_json::to_string(existing).expect("a list of strings");
    format!(
        r#"Tag this meeting with the parts of the work it covers, using the existing Tags where they fit.

Meeting: {title}

Summary:
{summary}

Existing Tags:
{existing}

Respond with only a JSON object matching this schema:
{{
  "tags": [{{ "name": "TagName", "confidence": 0.9 }}],
  "suggested_new_tags": ["TagName"]
}}

Rules:
- Only use names from the existing Tags in "tags".
- A Tag names which part of the work the meeting was about: a product area or feature (e.g. "Billing", "MCP", "Mobile App"), a workstream (e.g. "Test Automation", "Bug Triage", "Release Process", "UX Review"), a business function (e.g. "Hiring", "M&A", "Go-to-Market"), or a customer.
- Name the area, not one aspect of it: "MCP", not "MCP Permissions", "MCP Authentication" or "Schema MCP"; "Test Automation", not "Smoke Tests", "E2E Tests" or "CI Test Selection". An area should be big enough to come up in many meetings.
- Don't copy phrases from the meeting title or headings. Ask which area each one belongs to.
- When the meeting is about a specific customer, tag the customer by name (e.g. "Acme Corp").
- Never use a Tag that describes what nearly every meeting at this organization is about, such as the organization's core activity or the way it works in general (e.g. "Product Development", "Software Engineering", "AI-Assisted Development", "Meetings", "Strategy"). Such a Tag separates nothing. The same goes for:
  - the organization's own product or codebase as a whole (name the part of it instead);
  - tools, process or governance in general (e.g. "AI Tools", "Engineering Governance", "Engineering Process"). Name the specific practice instead, e.g. "Code Review", "Release Process", "AI Costs".
- Reuse an existing Tag when the topic falls inside its area. Don't stretch an existing Tag over a different area: suggest a new one instead.
- Tag only the areas the meeting spent real time on: a section of the discussion, a decision or an action item. Something named only in passing gets no Tag, even when an existing Tag matches it. Most meetings need 2 to 4 Tags, never more than {MAX_TAGS}.
- Use the plain name the people in the meeting would use, not a formal or corporate one: "Login Problems", not "Identity and Access Management"; "Bug Triage", not "Defect Management"; "UX Review", not "Design Review".
- confidence runs from 0.0 to 1.0. Give below {MIN_CONFIDENCE} to an area the meeting only touched on.
- Suggest at most {MAX_NEW_TAGS} new Tags.
- Never suggest minor variants of an existing Tag: a plural, an abbreviation, or a different case or wording for the same topic.
"#
    )
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    tags: Vec<Match>,
    #[serde(default)]
    suggested_new_tags: Vec<String>,
}

#[derive(Deserialize)]
struct Match {
    name: String,
    confidence: f64,
}

/// The Meeting's Tags from the model's reply: the confident matches, in the vocabulary's spelling and most confident
/// first, then up to [`MAX_NEW_TAGS`] new ones (a new one already in the vocabulary becomes that Tag). Without
/// duplicates, ignoring case, and at most [`MAX_TAGS`]. Tolerates a leading `<think>` block and a code fence.
pub fn parse_response(reply: &str, vocabulary: &[String]) -> Result<Vec<String>, TagError> {
    let mut text = reply;
    if let Some(end) = text.find("</think>") {
        text = &text[end + "</think>".len()..];
    }
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return Err(TagError::Reply(reply.chars().take(300).collect()));
    };
    let reply: Reply = serde_json::from_str(&text[start..=end]).map_err(|e| TagError::Reply(e.to_string()))?;

    let known = |name: &str| vocabulary.iter().find(|t| same_tag(t, name));
    let mut matches: Vec<&Match> = reply.tags.iter().filter(|m| m.confidence >= MIN_CONFIDENCE).collect();
    matches.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    let matched = matches.into_iter().filter_map(|m| known(&m.name).cloned());
    let new = reply
        .suggested_new_tags
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .take(MAX_NEW_TAGS)
        .map(|t| known(t).cloned().unwrap_or_else(|| t.to_owned()));

    let mut tags: Vec<String> = Vec::new();
    for tag in matched.chain(new) {
        if tags.len() < MAX_TAGS && !tags.iter().any(|t| same_tag(t, &tag)) {
            tags.push(tag);
        }
    }
    Ok(tags)
}

/// Whether two Tags name the same topic: equal ignoring case and surrounding space.
pub fn same_tag(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

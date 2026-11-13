//! Transcript vocabulary: Tracks, words returned by a Transcriber, and Segments (ADR-0005 schema), and how a
//! Transcript is written out for the summary prompt and the export.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Track {
    /// The user's microphone.
    You,
    /// The default sink monitor: everything the remote participants say.
    Others,
}

impl Track {
    pub const ALL: [Track; 2] = [Track::You, Track::Others];

    pub fn as_str(self) -> &'static str {
        match self {
            Track::You => "you",
            Track::Others => "others",
        }
    }
}

/// One transcribed word. Times are seconds, relative to whatever the context says (Chunk or Meeting).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

impl Word {
    pub fn midpoint(&self) -> f64 {
        (self.start + self.end) / 2.0
    }
}

/// One line of `transcript.jsonl`. Times are seconds from the start of the Meeting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub track: Track,
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
}

/// What the user is called in the Transcript: their display name, or "You".
pub fn display_name(name: &str) -> &str {
    match name.trim() {
        "" => "You",
        name => name,
    }
}

/// How a Segment's speaker is labelled in the prompt and the export.
pub fn speaker_label(segment: &Segment, display_name: &str) -> String {
    match (segment.track, segment.speaker) {
        (Track::You, _) => display_name.to_owned(),
        (Track::Others, None) => "Others".to_owned(),
        (Track::Others, Some(n)) => format!("Others (Speaker {n})"),
    }
}

/// `mm:ss`, or `h:mm:ss` from an hour on.
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    let (h, m, s) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

/// Hesitations dropped from shown text. Backchannels ("Mm-hmm", "Uh-huh", "Hmm") are kept, like "Okay" (ADR-0002).
const FILLERS: [&str; 6] = ["um", "umm", "uh", "uhh", "uhm", "erm"];

/// The Transcript as shown in the UI, the export and the Summary prompt: each Segment's text without fillers
/// or audio-event tags, leaving out Segments with nothing else. The stored Segments stay verbatim (ADR-0005):
/// echo dedupe and "Retry failed Chunks" read them back.
pub fn cleaned(segments: &[Segment]) -> Vec<Segment> {
    segments
        .iter()
        .filter_map(|s| {
            let text = without_fillers(&s.text);
            (!text.is_empty()).then(|| Segment { text, ..s.clone() })
        })
        .collect()
}

/// Drops filler words (`um`, `uh`, …) and audio-event tags (`[laughs]`, `*cough cough*`) from one Segment's text,
/// moving or removing the punctuation around them so the sentence still reads.
pub fn without_fillers(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut kept: Vec<String> = Vec::new();
    let mut capitalize_next = false;
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        let dropped = if let Some(end) = event_end(&tokens, i) {
            i = end;
            tokens[end]
        } else if is_filler(token) {
            let opens_sentence = kept.last().is_none_or(|w| ends_sentence(w));
            if opens_sentence && token.chars().find(|c| c.is_alphabetic()).is_some_and(char::is_uppercase) {
                capitalize_next = true;
            }
            token
        } else {
            let mut word = token.to_owned();
            if std::mem::take(&mut capitalize_next) {
                word = capitalized(&word);
            }
            kept.push(word);
            i += 1;
            continue;
        };
        if let Some(previous) = kept.last_mut() {
            let is_last = i + 1 == tokens.len();
            if ends_sentence(dropped) {
                let end = dropped.chars().last().unwrap();
                trim_pause(previous);
                if !previous.ends_with(['.', '?', '!', '…']) {
                    previous.push(end);
                }
            } else if is_last || dropped.ends_with([',', '-', '…']) || dropped.ends_with("...") {
                trim_pause(previous);
            }
        }
        i += 1;
    }
    kept.join(" ")
}

/// If `tokens[start]` opens an audio-event tag (`[…]` or `*…*`), the index of the token that closes it.
fn event_end(tokens: &[&str], start: usize) -> Option<usize> {
    let close = match tokens[start].chars().next()? {
        '[' => ']',
        '*' => '*',
        _ => return None,
    };
    (start..tokens.len()).find(|&j| {
        let t = tokens[j].trim_end_matches(['.', ',', '?', '!']);
        // A lone `*` both opens and closes, so the opening token closes the tag only if it has more in it.
        t.ends_with(close) && (j > start || t.len() > 1)
    })
}

fn is_filler(token: &str) -> bool {
    let word = token.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    FILLERS.contains(&word.as_str())
}

/// Ends in `.`, `?` or `!`, but not an ellipsis.
fn ends_sentence(word: &str) -> bool {
    word.ends_with(['.', '?', '!']) && !word.ends_with("...")
}

fn trim_pause(word: &mut String) {
    while word.ends_with([',', ';', ':']) {
        word.pop();
    }
}

fn capitalized(word: &str) -> String {
    match word.char_indices().find(|(_, c)| c.is_alphabetic()) {
        Some((at, c)) => format!("{}{}{}", &word[..at], c.to_uppercase(), &word[at + c.len_utf8()..]),
        None => word.to_owned(),
    }
}

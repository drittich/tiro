//! Spend: what each OpenRouter request cost, kept in the append-only spend ledger (ADR-0005), and the totals derived
//! from it.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// What OpenRouter charged for one request, as it reported it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub model: String,
    /// USD.
    pub cost: f64,
    /// The cost was worked out by Tiro rather than reported by OpenRouter.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    /// Seconds of audio transcribed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_id: Option<String>,
}

/// A request's outcome and what OpenRouter charged for it. A request can be charged and still fail, as when the model
/// returns an empty Summary.
#[derive(Debug, Clone, PartialEq)]
pub struct Billed<T, E> {
    pub result: Result<T, E>,
    /// `None` when the request reported no cost, as when it never got an answer.
    pub usage: Option<Usage>,
}

impl<T, E> From<Result<T, E>> for Billed<T, E> {
    fn from(result: Result<T, E>) -> Self {
        Self { result, usage: None }
    }
}

/// What a request was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpendKind {
    Transcription,
    Summary,
    Tags,
    /// Embeddings of Passages and search queries, for meaning search.
    Embedding,
    /// Answers to questions asked of the Meetings (Ask), made for no Meeting.
    Answer,
    /// A kind written by a later version of Tiro.
    #[serde(other)]
    Other,
}

/// One line of the spend ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendEntry {
    pub at: DateTime<Utc>,
    /// The Meeting's folder id; absent for a request made for no Meeting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meeting_id: Option<String>,
    pub kind: SpendKind,
    #[serde(flatten)]
    pub usage: Usage,
}

type Listener = Box<dyn Fn(&SpendEntry) + Send + Sync>;

/// The spend ledger, `spend.jsonl` in the data folder: one [`SpendEntry`] per line, only ever appended to, so it
/// keeps the spend of deleted Meetings.
pub struct SpendLedger {
    path: PathBuf,
    /// Keeps lines written at the same time from interleaving.
    writing: Mutex<()>,
    listener: OnceLock<Listener>,
}

impl SpendLedger {
    pub fn new(path: PathBuf) -> Self {
        Self { path, writing: Mutex::new(()), listener: OnceLock::new() }
    }

    /// Calls `listener` with every entry recorded from now on. Only the first listener set is kept.
    pub fn on_record(&self, listener: impl Fn(&SpendEntry) + Send + Sync + 'static) {
        let _ = self.listener.set(Box::new(listener));
    }

    /// Appends a line for a request made now, when it reported a cost. A failed write is logged, never returned:
    /// the request it records has already been made.
    pub fn record(&self, meeting_id: Option<&str>, kind: SpendKind, usage: Option<&Usage>) {
        let Some(usage) = usage else { return };
        let entry = SpendEntry { at: Utc::now(), meeting_id: meeting_id.map(str::to_owned), kind, usage: usage.clone() };
        if let Err(e) = self.append(&entry) {
            tracing::warn!("could not record spend in {}: {e}", self.path.display());
        }
        if let Some(listener) = self.listener.get() {
            listener(&entry);
        }
    }

    fn append(&self, entry: &SpendEntry) -> std::io::Result<()> {
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        let _writing = self.writing.lock().unwrap();
        let mut file = OpenOptions::new().create(true).read(true).append(true).open(&self.path)?;
        // A line cut short by a crash has no newline; start a new one so this entry isn't lost with it.
        if file.seek(SeekFrom::End(0))? > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                line.insert(0, '\n');
            }
        }
        file.write_all(line.as_bytes())
    }

    /// Every entry recorded, oldest first; none when there is no ledger yet or it can't be read.
    pub fn entries(&self) -> Vec<SpendEntry> {
        match fs::read_to_string(&self.path) {
            Ok(text) => parse_ledger(&text),
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!("could not read {}: {e}", self.path.display());
                Vec::new()
            }
        }
    }
}

/// The ledger's entries, skipping any line that isn't one (such as a line cut short by a crash).
pub fn parse_ledger(text: &str) -> Vec<SpendEntry> {
    text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
}

/// A Meeting's spend: its total and what each kind of request cost.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSpend {
    pub total: f64,
    /// Only the kinds with entries, in the order of [`SpendKind`].
    pub by_kind: Vec<KindSpend>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindSpend {
    pub kind: SpendKind,
    pub cost: f64,
}

/// What the Meeting with this folder id has cost, from the ledger's entries.
pub fn meeting_spend(entries: &[SpendEntry], meeting_id: &str) -> MeetingSpend {
    let mine: Vec<&SpendEntry> = entries.iter().filter(|e| e.meeting_id.as_deref() == Some(meeting_id)).collect();
    let by_kind = [SpendKind::Transcription, SpendKind::Summary, SpendKind::Tags, SpendKind::Embedding, SpendKind::Answer, SpendKind::Other]
        .into_iter()
        .filter(|kind| mine.iter().any(|e| e.kind == *kind))
        .map(|kind| KindSpend { kind, cost: mine.iter().filter(|e| e.kind == kind).map(|e| e.usage.cost).sum() })
        .collect();
    MeetingSpend { total: mine.iter().map(|e| e.usage.cost).sum(), by_kind }
}

/// Spend across every Meeting, deleted ones included.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendSummary {
    /// Every month with an entry, newest first.
    pub months: Vec<MonthSpend>,
    /// Since the first entry.
    pub total: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthSpend {
    pub year: i32,
    /// 1 to 12.
    pub month: u32,
    pub total: f64,
}

/// Each calendar month's total in time zone `tz`, and the total to date, from the ledger's entries.
pub fn spend_summary<Tz: TimeZone>(entries: &[SpendEntry], tz: &Tz) -> SpendSummary {
    let mut months: Vec<MonthSpend> = Vec::new();
    for entry in entries {
        let local = entry.at.with_timezone(tz);
        let (year, month) = (local.year(), local.month());
        match months.iter_mut().find(|m| (m.year, m.month) == (year, month)) {
            Some(m) => m.total += entry.usage.cost,
            None => months.push(MonthSpend { year, month, total: entry.usage.cost }),
        }
    }
    months.sort_by(|a, b| (b.year, b.month).cmp(&(a.year, a.month)));
    SpendSummary { months, total: entries.iter().map(|e| e.usage.cost).sum() }
}

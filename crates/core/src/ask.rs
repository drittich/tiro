//! Ask: questions about the Meetings answered by the Ask model from the Passages Search retrieves for them, in a
//! conversation that lasts until a new one is started or Tiro quits. Nothing of it is kept but its Spend.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Serialize;

use crate::search::{rank_for_question, window_ranges, Hit, Meaning, Passage, Place};
use crate::spend::{Billed, SpendKind, SpendLedger};
use crate::store::MeetingStore;
use crate::transcript::{self, clock, speaker_label, Segment};

pub const DEFAULT_ASK_MODEL: &str = "deepseek/deepseek-v4.1-flash";

// The context budget, kept together so it can be tuned from the Spend ledger.
/// The most Passages one question sends, best-ranked first…
pub const PASSAGES_PER_TURN: usize = 25;
/// …and the most of them from any one Meeting, so a long Meeting can't crowd out the others.
pub const PASSAGES_PER_MEETING: usize = 6;
/// The most tokens of Passages one question sends; the lowest-ranked are dropped to fit, never cut.
pub const TURN_TOKENS: usize = 12_000;
/// The most tokens of the whole conversation: questions, answers and Passages. Past it, the oldest turns'
/// Passages are dropped, and their answers kept.
pub const CONVERSATION_TOKENS: usize = 40_000;
/// A rough count of tokens from characters, which is all the budget needs.
const CHARS_PER_TOKEN: usize = 4;

/// What a Meeting's Passages are sent from: its Passages as indexed, and its Transcript's shown Segments, which a
/// Transcript Passage is widened and labelled from.
#[derive(Debug, Clone)]
pub struct MeetingMaterial {
    pub meeting_id: String,
    pub title: String,
    pub started_at: DateTime<FixedOffset>,
    pub passages: Vec<Passage>,
    /// As [`crate::transcript::cleaned`] gives them.
    pub segments: Vec<Segment>,
}

/// A Passage as it was sent to the Ask model, numbered for citing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// Unique in its conversation, from 1.
    pub number: usize,
    pub meeting_id: String,
    pub title: String,
    pub started_at: DateTime<FixedOffset>,
    pub place: Place,
    /// What was sent: a Transcript Passage widened by its neighbours, each Segment with its time and speaker.
    #[serde(skip)]
    pub text: String,
    /// The Transcript windows it holds.
    #[serde(skip)]
    windows: Option<Range<usize>>,
}

impl Source {
    /// The Passage as the model reads it: its number, Meeting, date and place, then its text.
    pub fn block(&self) -> String {
        format!(
            "[{}] {} · {} · {}\n{}",
            self.number,
            self.title,
            self.started_at.format("%a %-d %b %Y"),
            place_label(&self.place),
            self.text
        )
    }

    fn tokens(&self) -> usize {
        tokens(&self.block())
    }
}

/// Where a Passage is, as Search results label it (src/lib/api.ts `placeLabel`).
fn place_label(place: &Place) -> String {
    match place {
        Place::Title => "Title".into(),
        Place::Summary { heading: Some(heading), .. } => format!("Summary · {heading}"),
        Place::Summary { heading: None, .. } => "Summary".into(),
        Place::Notes { .. } => "Notes".into(),
        Place::Transcript { ms } => format!("Transcript at {}", clock(*ms as f64 / 1000.0)),
    }
}

fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// One question in a conversation, the Passages sent with it and still in view, and its answer as the model wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub question: String,
    pub sources: Vec<Source>,
    pub answer: String,
}

impl Turn {
    pub fn tokens(&self) -> usize {
        tokens(&self.question) + tokens(&self.answer) + self.sources_tokens()
    }

    pub fn sources_tokens(&self) -> usize {
        self.sources.iter().map(Source::tokens).sum()
    }
}

/// The questions asked so far and their answers, kept in memory only.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    turns: Vec<Turn>,
    /// The last Passage number given, so numbers stay unique after Passages are dropped.
    numbered: usize,
}

impl Conversation {
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    pub fn push(&mut self, turn: Turn) {
        self.numbered = self.numbered.max(turn.sources.iter().map(|s| s.number).max().unwrap_or(0));
        self.turns.push(turn);
    }

    /// Every Passage still in view, oldest first.
    pub fn sources(&self) -> impl Iterator<Item = &Source> {
        self.turns.iter().flat_map(|t| &t.sources)
    }

    pub fn tokens(&self) -> usize {
        self.turns.iter().map(Turn::tokens).sum()
    }

    /// Drops the oldest turns' Passages, a turn at a time, until the conversation fits `budget` tokens or only
    /// questions and answers are left.
    pub fn cap(&mut self, budget: usize) {
        let mut total = self.tokens();
        for turn in &mut self.turns {
            if total <= budget {
                break;
            }
            total -= turn.sources_tokens();
            turn.sources.clear();
        }
    }
}

/// The Passages to send with a question, numbered on from the conversation's: from `ranked` (Search's ranking,
/// best first), at most [`PASSAGES_PER_TURN`] and [`PASSAGES_PER_MEETING`] from one Meeting, leaving out what the
/// conversation already has in view. A Transcript Passage is widened by the window on each side that isn't already
/// sent; the others go as they are. Then the lowest-ranked are dropped until the rest fit [`TURN_TOKENS`]. `material` gives a Meeting's material by id; a Meeting without any is skipped.
pub fn build_context(
    ranked: &[Hit],
    conversation: &Conversation,
    mut material: impl FnMut(&str) -> Option<MeetingMaterial>,
) -> Vec<Source> {
    let mut loaded: HashMap<String, Option<(MeetingMaterial, Vec<(Place, Range<usize>)>)>> = HashMap::new();
    let mut sent: HashSet<(String, Place)> = HashSet::new();
    let mut covered: HashMap<String, HashSet<usize>> = HashMap::new();
    for source in conversation.sources() {
        sent.insert((source.meeting_id.clone(), source.place.clone()));
        if let Some(windows) = &source.windows {
            covered.entry(source.meeting_id.clone()).or_default().extend(windows.clone());
        }
    }

    let mut sources: Vec<Source> = Vec::new();
    let (mut taken, mut per_meeting) = (0, HashMap::<&str, usize>::new());
    for hit in ranked {
        if taken == PASSAGES_PER_TURN {
            break;
        }
        let id = hit.meeting_id.as_str();
        if per_meeting.get(id).copied().unwrap_or(0) == PASSAGES_PER_MEETING
            || sent.contains(&(hit.meeting_id.clone(), hit.place.clone()))
        {
            continue;
        }
        let entry = loaded.entry(hit.meeting_id.clone()).or_insert_with(|| {
            material(id).map(|m| {
                let windows = window_ranges(&m.segments);
                (m, windows)
            })
        });
        let Some((meeting, windows)) = entry else { continue };
        let covered = covered.entry(hit.meeting_id.clone()).or_default();
        let (text, span) = match &hit.place {
            Place::Transcript { .. } => {
                let Some(at) = windows.iter().position(|(place, _)| *place == hit.place) else { continue };
                if covered.contains(&at) {
                    continue;
                }
                let first = if at > 0 && !covered.contains(&(at - 1)) { at - 1 } else { at };
                let last = if at + 1 < windows.len() && !covered.contains(&(at + 1)) { at + 1 } else { at };
                let segments = windows[first].1.start..windows[last].1.end;
                (labelled(&meeting.segments[segments]), Some(first..last + 1))
            }
            place => {
                let Some(passage) = meeting.passages.iter().find(|p| p.place == *place) else { continue };
                (passage_text(passage), None)
            }
        };
        taken += 1;
        *per_meeting.entry(id).or_default() += 1;
        let source = Source {
            number: 0,
            meeting_id: hit.meeting_id.clone(),
            title: meeting.title.clone(),
            started_at: meeting.started_at,
            place: hit.place.clone(),
            text,
            windows: span,
        };
        if let Some(span) = &source.windows {
            covered.extend(span.clone());
        }
        sent.insert((source.meeting_id.clone(), source.place.clone()));
        sources.push(source);
    }
    // A Passage's text never depends on one ranked below it, so dropping from the bottom leaves the rest whole.
    let mut used: usize = sources.iter().map(Source::tokens).sum();
    while used > TURN_TOKENS {
        used -= sources.pop().map_or(0, |s| s.tokens());
    }
    for (n, source) in sources.iter_mut().enumerate() {
        source.number = conversation.numbered + n + 1;
    }
    sources
}

/// Each Segment on a line of its own with its time and speaker: "Me" for the microphone, so the model can tell what
/// the user said.
fn labelled(segments: &[Segment]) -> String {
    segments.iter().map(|s| format!("[{}] {}: {}", clock(s.start), speaker_label(s, "Me"), s.text)).collect::<Vec<_>>().join("\n")
}

/// A Passage's text, after its Summary section's heading when it has one.
fn passage_text(passage: &Passage) -> String {
    match &passage.place {
        Place::Summary { heading: Some(heading), .. } if passage.text.is_empty() => heading.clone(),
        Place::Summary { heading: Some(heading), .. } => format!("{heading}\n{}", passage.text),
        _ => passage.text.clone(),
    }
}

/// Open and close a citation in an answer's text, around its Passage number: characters from Unicode's private use
/// area, which no answer has and Markdown leaves alone, for the Ask view to turn into links.
pub const CITE_OPEN: char = '\u{E000}';
pub const CITE_CLOSE: char = '\u{E001}';

/// The answer with each Passage number the model cited in brackets (`[3]`, `[1, 3]`, `[Passage 2]`) marked as a
/// citation between [`CITE_OPEN`] and [`CITE_CLOSE`], and the numbers cited, in the order first cited. A number not
/// in `numbers` is dropped, and so is a bracket left with none, with the space before it. Other brackets stay.
pub fn link_citations(answer: &str, numbers: &[usize]) -> (String, Vec<usize>) {
    let mut text = String::with_capacity(answer.len());
    let mut cited: Vec<usize> = Vec::new();
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let parsed = after.find(']').and_then(|close| Some((citation(&after[..close])?, close)));
        let Some((cites, close)) = parsed else {
            text.push('[');
            rest = after;
            continue;
        };
        let known: Vec<usize> = cites.into_iter().filter(|n| numbers.contains(n)).collect();
        if known.is_empty() && text.ends_with(' ') {
            text.pop();
        }
        for n in known {
            text.push(CITE_OPEN);
            text.push_str(&n.to_string());
            text.push(CITE_CLOSE);
            if !cited.contains(&n) {
                cited.push(n);
            }
        }
        rest = &after[close + 1..];
    }
    text.push_str(rest);
    (text, cited)
}

/// The numbers in a bracket's contents when it is a citation: numbers separated by commas, semicolons or spaces,
/// after "Passage" or "Passages" if it likes.
fn citation(inside: &str) -> Option<Vec<usize>> {
    let inside = inside.trim();
    let inside = ["Passages", "Passage"].iter().find_map(|w| inside.strip_prefix(w)).unwrap_or(inside);
    let numbers: Vec<usize> = inside
        .split([',', ';', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    (!numbers.is_empty()).then_some(numbers)
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AskError {
    /// The Ask model has no zero-data-retention endpoint; the user must pick another model.
    #[error("{0}")]
    ZdrViolation(String),
    /// OpenRouter rejected the API key.
    #[error("{0}")]
    Unauthorized(String),
    /// Anything else: network trouble, server errors, an empty answer, an unreadable index. Asking again may help.
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// One message of a chat request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// The Ask model boundary.
#[async_trait]
pub trait Answerer: Send + Sync {
    /// The model's answer to the conversation in `messages`, streamed: `on_text` gets each piece as it comes. With
    /// what the request cost.
    async fn answer(&self, messages: &[Message], on_text: &(dyn for<'s> Fn(&'s str) + Send + Sync)) -> Billed<String, AskError>;
}

const SYSTEM_PROMPT: &str = r#"You answer questions about the user's recorded meetings, only from the numbered Passages of their Meetings given in this conversation. Each Passage is headed by its number, its Meeting's title, the Meeting's date, and where in the Meeting it is: the Title, a Summary section, the user's Notes, or the Transcript at a time.

In Transcript Passages, each line is one thing said, with its time and speaker. "Me" is the user, the person asking you. "Others" is everyone else in the meeting, and "Others (Speaker 2)" one particular person among them. The Transcript is machine-made, so it may mishear words or names.

Rules:
- Answer only from the Passages. Never make up a Meeting, a decision, a name or a quote.
- Cite the Passage each claim comes from by its number in square brackets, like [3], or [3, 7] for several, right after the claim.
- If the Passages don't answer the question, say so plainly, and say what they do cover if that helps. A partial answer is fine when you say what is missing.
- Mention which Meeting and date something comes from when it matters, as when Meetings disagree or a decision changed over time.
- `tag:Name` in a question only chose the Meetings the Passages come from; it is not part of the question.
- Answer in Markdown, as briefly as the question allows.

Today is {{today}}."#;

/// The chat request for `question`: the instructions, each earlier turn with the Passages it still has in view and
/// its answer, then `sources` and the question.
pub fn messages(conversation: &Conversation, question: &str, sources: &[Source], today: NaiveDate) -> Vec<Message> {
    let mut messages =
        vec![Message { role: Role::System, content: SYSTEM_PROMPT.replace("{{today}}", &today.format("%A %-d %B %Y").to_string()) }];
    for turn in conversation.turns() {
        messages.push(Message { role: Role::User, content: turn_prompt(&turn.sources, &turn.question, false) });
        messages.push(Message { role: Role::Assistant, content: turn.answer.clone() });
    }
    messages.push(Message { role: Role::User, content: turn_prompt(sources, question, true) });
    messages
}

fn turn_prompt(sources: &[Source], question: &str, current: bool) -> String {
    let passages = match (sources.is_empty(), current) {
        (false, _) => format!("Passages:\n\n{}", sources.iter().map(Source::block).collect::<Vec<_>>().join("\n\n")),
        (true, true) => "No Passages beyond those already given matched this question.".to_owned(),
        // An earlier turn's Passages were dropped to keep the conversation short.
        (true, false) => "(This question's Passages are no longer shown.)".to_owned(),
    };
    format!("{passages}\n\nQuestion: {question}")
}

/// What asking needs: the Meetings, meaning search when it is on, the Ask model, the ledger its cost goes in, and
/// today's date, for questions like "last week".
pub struct Asking<'a> {
    pub store: &'a MeetingStore,
    pub meaning: Option<Meaning<'a>>,
    pub answerer: &'a dyn Answerer,
    pub spend: &'a SpendLedger,
    pub today: NaiveDate,
}

/// What an answer being written streams to the Ask view.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AskEvent {
    /// Before any text: every Passage the answer may cite, so its citations can be labelled and opened as they come.
    Passages { sources: Vec<Source> },
    /// The whole answer so far, its citations marked.
    Text { text: String },
}

/// An answer, its citations marked (see [`link_citations`]), with the Passages it cites in the order first cited.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Answer {
    pub text: String,
    pub sources: Vec<Source>,
}

/// Answers `question` as the next turn of `conversation`. Retrieves Passages with Search for the question and the
/// previous one, since a follow-up alone ("and what did Sam say?") seldom finds anything; caps the conversation;
/// streams the Passages in view and then the answer to `on_event`; records the request in
/// the Spend ledger as an `answer` for no Meeting; and adds the turn to `conversation`. On failure the
/// conversation is as it was.
pub async fn ask(
    asking: &Asking<'_>,
    conversation: &mut Conversation,
    question: &str,
    on_event: &(dyn Fn(AskEvent) + Send + Sync),
) -> Result<Answer, AskError> {
    let search = match conversation.turns().last() {
        Some(previous) => format!("{question} {}", previous.question),
        None => question.to_owned(),
    };
    let ranked = rank_for_question(asking.store, &search, asking.meaning.as_ref())
        .await
        .map_err(|e| AskError::Failed(format!("could not search the Meetings: {e}")))?;

    let mut next = conversation.clone();
    next.cap(CONVERSATION_TOKENS.saturating_sub(TURN_TOKENS + tokens(question)));
    let sources = build_context(&ranked, &next, |id| material(asking.store, id));
    let messages = messages(&next, question, &sources, asking.today);
    let numbers: Vec<usize> = next.sources().chain(&sources).map(|s| s.number).collect();
    on_event(AskEvent::Passages { sources: next.sources().chain(&sources).cloned().collect() });

    let so_far = Mutex::new(String::new());
    let billed = asking
        .answerer
        .answer(&messages, &|piece| {
            let mut text = so_far.lock().unwrap();
            text.push_str(piece);
            on_event(AskEvent::Text { text: link_citations(&text, &numbers).0 });
        })
        .await;
    asking.spend.record(None, SpendKind::Answer, billed.usage.as_ref());
    let reply = billed.result?;
    if reply.trim().is_empty() {
        return Err(AskError::Failed("The Ask model returned an empty answer.".into()));
    }

    let (text, cited) = link_citations(&reply, &numbers);
    let in_view: Vec<&Source> = next.sources().chain(&sources).collect();
    let cited = cited.iter().filter_map(|n| in_view.iter().find(|s| s.number == *n).map(|s| (*s).clone())).collect();
    next.push(Turn { question: question.to_owned(), sources, answer: reply });
    *conversation = next;
    Ok(Answer { text, sources: cited })
}

/// A Meeting's material from the store; `None`, logged, when it can't be read, as when it was just deleted.
fn material(store: &MeetingStore, id: &str) -> Option<MeetingMaterial> {
    let read = || -> Result<MeetingMaterial, crate::store::StoreError> {
        let meeting = store.get(id)?;
        Ok(MeetingMaterial {
            passages: store.passages(id)?,
            segments: transcript::cleaned(&store.transcript(&meeting)?),
            meeting_id: meeting.id,
            title: meeting.title,
            started_at: meeting.started_at,
        })
    };
    read().map_err(|e| tracing::warn!("could not read {id} to answer from it: {e}")).ok()
}

//! Search: Meetings split into Passages, the search box's query, and ranked Passages grouped into Meeting results.

use std::ops::Range;

use serde::Serialize;

use crate::embeddings::{Embedder, MeaningIndex};
use crate::spend::SpendLedger;
use crate::store::{MeetingStatus, MeetingStore, StoreError};
use crate::tags::same_tag;
use crate::transcript::{self, Segment};

/// The most Passages one search ranks.
const MAX_HITS: usize = 1000;
/// A result shows this many of its Meeting's best Passages.
const HITS_PER_MEETING: usize = 3;

/// A Transcript window closes once it spans this many seconds…
const WINDOW_MIN_SECS: f64 = 30.0;
/// …and never takes a Segment that would stretch it past this many.
const WINDOW_MAX_SECS: f64 = 60.0;

/// Where in its Meeting a Passage is, so a result can open the Meeting there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "source", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum Place {
    Title,
    /// Section 0 is the text before the first heading; section `n` starts at the `n`-th heading.
    Summary { section: usize, heading: Option<String> },
    /// The paragraph starting at this line of `notes.md`, from 0.
    Notes { line: usize },
    /// The window starting at this Meeting time, in ms: the start of its first Segment.
    Transcript { ms: u64 },
}

/// A searchable piece of a Meeting with its place in it (GLOSSARY.md). A Summary section's heading is in its
/// Place, not its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    pub meeting_id: String,
    pub place: Place,
    pub text: String,
}

/// Splits a Meeting into Passages: its title, its Summary's sections, its Notes' paragraphs and windows of its
/// Transcript. `segments` are as stored; the Passages hold their text as shown, without fillers.
pub fn split(meeting_id: &str, title: &str, summary: &str, notes: &str, segments: &[Segment]) -> Vec<Passage> {
    let mut places: Vec<(Place, String)> = vec![(Place::Title, title.trim().to_owned())];
    places.extend(summary_sections(summary));
    places.extend(notes_paragraphs(notes));
    places.extend(transcript_windows(&transcript::cleaned(segments)));
    places
        .into_iter()
        // A heading is searched on its own, so a Summary section with nothing under it still counts.
        .filter(|(place, text)| !text.is_empty() || matches!(place, Place::Summary { heading: Some(_), .. }))
        .map(|(place, text)| Passage { meeting_id: meeting_id.to_owned(), place, text })
        .collect()
}

fn summary_sections(summary: &str) -> Vec<(Place, String)> {
    let mut sections = vec![(Place::Summary { section: 0, heading: None }, Vec::new())];
    for line in summary.lines() {
        if let Some(heading) = heading(line) {
            let place = Place::Summary { section: sections.len(), heading: Some(plain(heading)) };
            sections.push((place, Vec::new()));
        } else {
            sections.last_mut().unwrap().1.push(line);
        }
    }
    sections.into_iter().map(|(place, lines)| (place, joined(&lines))).collect()
}

/// A Markdown heading's text, as `renderMarkdown` (src/lib/markdown.ts) finds headings.
fn heading(line: &str) -> Option<&str> {
    let rest = line.trim_end().trim_start_matches('#');
    let level = line.len() - line.trim_start_matches('#').len();
    ((1..=6).contains(&level) && rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

fn notes_paragraphs(notes: &str) -> Vec<(Place, String)> {
    let mut paragraphs: Vec<(Place, Vec<&str>)> = Vec::new();
    let mut in_paragraph = false;
    for (n, line) in notes.lines().enumerate() {
        if line.trim().is_empty() {
            in_paragraph = false;
        } else if in_paragraph {
            paragraphs.last_mut().unwrap().1.push(line);
        } else {
            paragraphs.push((Place::Notes { line: n }, vec![line]));
            in_paragraph = true;
        }
    }
    paragraphs.into_iter().map(|(place, lines)| (place, joined(&lines))).collect()
}

/// Runs of consecutive Segments spanning [`WINDOW_MIN_SECS`] to [`WINDOW_MAX_SECS`]; a Segment longer than that
/// is a window of its own.
fn transcript_windows(segments: &[Segment]) -> Vec<(Place, String)> {
    window_ranges(segments)
        .into_iter()
        .map(|(place, range)| (place, segments[range].iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ")))
        .collect()
}

/// The Transcript windows of `segments` (shown Segments, as [`transcript::cleaned`] gives them), each with the
/// range of Segments it holds, in order: the Places of a Meeting's Transcript Passages.
pub fn window_ranges(segments: &[Segment]) -> Vec<(Place, Range<usize>)> {
    let mut windows = Vec::new();
    let mut start = 0;
    let close = |start: &mut usize, end: usize, windows: &mut Vec<(Place, Range<usize>)>| {
        if end > *start {
            let ms = (segments[*start].start.max(0.0) * 1000.0).round() as u64;
            windows.push((Place::Transcript { ms }, *start..end));
        }
        *start = end;
    };
    for (n, segment) in segments.iter().enumerate() {
        if n > start && segment.end - segments[start].start > WINDOW_MAX_SECS {
            close(&mut start, n, &mut windows);
        }
        if segment.end - segments[start].start >= WINDOW_MIN_SECS {
            close(&mut start, n + 1, &mut windows);
        }
    }
    close(&mut start, segments.len(), &mut windows);
    windows
}

/// What was typed in the search box: Tag filters (`tag:MCP`, `tag:"Agent Permissions"`), quoted phrases and bare
/// keywords.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    /// A Meeting must have every one of them. Without duplicates, ignoring case.
    pub tags: Vec<String>,
    pub phrases: Vec<String>,
    pub keywords: Vec<String>,
}

impl Query {
    /// The FTS5 expression matching Passages with every phrase and keyword, or `None` when there are none. Each is
    /// quoted, so FTS5 syntax typed in the box (`OR`, `*`, `-`, `NEAR(`) is only ever text, never an operator.
    /// Terms without a letter or digit match nothing and are left out.
    pub fn fts(&self) -> Option<String> {
        self.fts_joined(" ")
    }

    /// Like [`Self::fts`], but matching Passages with any of the phrases and keywords, ranked by how many and how
    /// rare: a question asked in a sentence (Ask) seldom has all its words in one Passage.
    pub fn fts_any(&self) -> Option<String> {
        self.fts_joined(" OR ")
    }

    fn fts_joined(&self, separator: &str) -> Option<String> {
        let terms: Vec<String> = self
            .phrases
            .iter()
            .chain(&self.keywords)
            .filter(|t| t.chars().any(char::is_alphanumeric))
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect();
        (!terms.is_empty()).then(|| terms.join(separator))
    }

    /// The phrases and keywords as one text, which is what is embedded for meaning search.
    fn text(&self) -> String {
        self.phrases.iter().chain(&self.keywords).map(String::as_str).collect::<Vec<_>>().join(" ")
    }
}

/// Reads the search box. A quote opens a phrase that runs to the next quote, or to the end when there is none;
/// `tag:` (any case) before a word or a quoted phrase makes it a Tag filter. Never fails: anything else is a keyword.
pub fn parse_query(input: &str) -> Query {
    let mut query = Query::default();
    let mut rest = input.trim_start();
    while !rest.is_empty() {
        let is_tag = rest.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("tag:"));
        if is_tag {
            rest = &rest[4..];
        }
        let (term, quoted, after) = next_term(rest);
        rest = after.trim_start();
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        if is_tag {
            if !query.tags.iter().any(|t| same_tag(t, term)) {
                query.tags.push(term.to_owned());
            }
        } else if quoted {
            query.phrases.push(term.to_owned());
        } else {
            query.keywords.push(term.to_owned());
        }
    }
    query
}

/// The term at the start of `s`, whether it was quoted, and what follows it.
fn next_term(s: &str) -> (&str, bool, &str) {
    if let Some(inner) = s.strip_prefix('"') {
        return match inner.find('"') {
            Some(end) => (&inner[..end], true, &inner[end + 1..]),
            None => (inner, true, ""),
        };
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    (&s[..end], false, &s[end..])
}

/// A piece of a snippet: matched text is highlighted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fragment {
    pub text: String,
    pub matched: bool,
}

/// A Passage that matched, with the excerpt shown for it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub meeting_id: String,
    pub place: Place,
    pub snippet: Vec<Fragment>,
    /// Higher is a better match.
    pub score: f64,
}

/// A Meeting in the results, with its best Hits.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingResult {
    pub meeting_id: String,
    /// Its best Hit's score.
    pub score: f64,
    /// Best first.
    pub hits: Vec<Hit>,
}

/// Ranked Hits as Meeting results, best first: each Meeting ranks by its best Hit and keeps its `per_meeting` best.
/// Takes the Hits in any order, so another ranking can be merged into them first.
pub fn group_by_meeting(mut hits: Vec<Hit>, per_meeting: usize) -> Vec<MeetingResult> {
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut results: Vec<MeetingResult> = Vec::new();
    for hit in hits {
        match results.iter_mut().find(|r| r.meeting_id == hit.meeting_id) {
            Some(result) if result.hits.len() < per_meeting => result.hits.push(hit),
            Some(_) => {}
            None => results.push(MeetingResult { meeting_id: hit.meeting_id.clone(), score: hit.score, hits: vec![hit] }),
        }
    }
    results
}

/// Reciprocal Rank Fusion's `k`: how much a Passage's rank near the top counts over one further down.
const RRF_K: f64 = 60.0;

/// The keyword and meaning rankings of Passages, each best first, as one ranking by Reciprocal Rank Fusion: a
/// Passage scores `1 / (RRF_K + rank)` in each ranking it is in, ranks counted from 1. A Passage in both keeps the
/// keyword Hit's snippet, with its matches highlighted, and on equal scores a keyword Hit ranks first.
pub fn fuse(keyword: Vec<Hit>, meaning: Vec<Hit>) -> Vec<Hit> {
    let rrf = |rank: usize| 1.0 / (RRF_K + rank as f64 + 1.0);
    let mut fused: Vec<Hit> = keyword.into_iter().enumerate().map(|(rank, hit)| Hit { score: rrf(rank), ..hit }).collect();
    for (rank, hit) in meaning.into_iter().enumerate() {
        match fused.iter_mut().find(|h| h.meeting_id == hit.meeting_id && h.place == hit.place) {
            Some(found) => found.score += rrf(rank),
            None => fused.push(Hit { score: rrf(rank), ..hit }),
        }
    }
    // Stable, so on equal scores the keyword Hits, which come first, stay ahead.
    fused.sort_by(|a, b| b.score.total_cmp(&a.score));
    fused
}

/// Whether a search matched by meaning as well as by keywords.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MeaningSearch {
    On,
    /// There is no API key or no Embedding model.
    Off,
    /// Nothing is embedded yet, or the query couldn't be embedded, so only keywords matched.
    Unavailable,
}

/// What a search found.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    /// The Tag filters in the query: a Meeting must have all of them.
    pub tags: Vec<String>,
    /// The Meetings with matching Passages, best first; `None` when the query has no keywords or phrases, which
    /// leaves the list to show every Meeting with the Tags in its own order.
    pub meetings: Option<Vec<MeetingResult>>,
    pub meaning: MeaningSearch,
}

/// Searches the indexed Passages by keywords for what was typed in the search box.
pub fn search(store: &MeetingStore, input: &str) -> Result<SearchResults, StoreError> {
    keyword_search(store, &parse_query(input), |_| None)
}

/// What meaning search needs: the embedded Passages, the model that embedded them, and the ledger the query's
/// embedding is charged to.
pub struct Meaning<'a> {
    pub index: &'a MeaningIndex,
    pub embedder: &'a dyn Embedder,
    pub spend: &'a SpendLedger,
}

/// Searches the indexed Passages by keywords and by meaning for what was typed in the search box, merging the
/// two rankings. Everything but the Tag filters is embedded, once per query. When that fails, only keywords match,
/// and so do they for a query without a letter, such as a ticket number, which has no meaning to match.
pub async fn search_by_meaning(store: &MeetingStore, input: &str, meaning: &Meaning<'_>) -> Result<SearchResults, StoreError> {
    let query = parse_query(input);
    let (vector, status) = match query.fts() {
        None => (None, MeaningSearch::On),
        Some(_) => query_vector(&query, meaning).await,
    };
    let model = meaning.embedder.model();
    let results = keyword_search(store, &query, |among| vector.as_ref().map(|v| meaning.index.ranked(model, v, among)))?;
    Ok(SearchResults { meaning: status, ..results })
}

/// The vector of the query's text for meaning search, or why there is none. Nothing embedded yet means nothing to
/// compare with, so the query isn't sent then, nor is a text without a letter, which has no meaning to match.
async fn query_vector(query: &Query, meaning: &Meaning<'_>) -> (Option<Vec<f32>>, MeaningSearch) {
    let text = query.text();
    if !text.chars().any(char::is_alphabetic) {
        return (None, MeaningSearch::On);
    }
    if !meaning.index.has_model(meaning.embedder.model()) {
        return (None, MeaningSearch::Unavailable);
    }
    match meaning.index.query_vector(&text, meaning.embedder, meaning.spend).await {
        Ok(vector) => (Some(vector), MeaningSearch::On),
        Err(e) => {
            tracing::warn!("could not embed the query, so it matches by keywords only: {e}");
            (None, MeaningSearch::Unavailable)
        }
    }
}

/// The Passages for a question asked of the Meetings (Ask), best first, ranked as Search ranks them: by keywords,
/// matching any of the question's words, merged with the meaning ranking when `meaning` is given. Only Meetings
/// with every Tag filter in the question count.
pub async fn rank_for_question(store: &MeetingStore, question: &str, meaning: Option<&Meaning<'_>>) -> Result<Vec<Hit>, StoreError> {
    let query = parse_query(question);
    let Some(fts) = query.fts_any() else { return Ok(Vec::new()) };
    let tagged = tagged_meetings(store, &query.tags)?;
    let hits = store.matching_passages(&fts, tagged.as_deref(), MAX_HITS)?;
    let Some(meaning) = meaning else { return Ok(hits) };
    Ok(match query_vector(&query, meaning).await {
        (Some(vector), _) => fuse(hits, meaning.index.ranked(meaning.embedder.model(), &vector, tagged.as_deref())),
        (None, _) => hits,
    })
}

/// The ids of the Meetings with every one of `tags`; `None` when there are no Tag filters, which is every Meeting.
fn tagged_meetings(store: &MeetingStore, tags: &[String]) -> Result<Option<Vec<String>>, StoreError> {
    if tags.is_empty() {
        return Ok(None);
    }
    let meetings = store.list()?;
    Ok(Some(meetings.into_iter().filter(|m| has_tags(m.tags.as_deref().unwrap_or_default(), tags)).map(|m| m.id).collect()))
}

/// Keyword search for `query`, its ranking merged with the one `by_meaning` gives for the Tagged Meetings (all
/// when `None`), if it gives one.
fn keyword_search(
    store: &MeetingStore,
    query: &Query,
    by_meaning: impl FnOnce(Option<&[String]>) -> Option<Vec<Hit>>,
) -> Result<SearchResults, StoreError> {
    let tags = query.tags.clone();
    let Some(fts) = query.fts() else {
        // Words that were only FTS syntax match nothing; without any, the list shows the Tagged Meetings.
        let typed = !query.phrases.is_empty() || !query.keywords.is_empty();
        return Ok(SearchResults { tags, meetings: typed.then(Vec::new), meaning: MeaningSearch::Off });
    };
    let tagged = tagged_meetings(store, &query.tags)?;
    let mut hits = store.matching_passages(&fts, tagged.as_deref(), MAX_HITS)?;
    let meaning = match by_meaning(tagged.as_deref()) {
        Some(meaning_hits) => {
            hits = fuse(hits, meaning_hits);
            MeaningSearch::On
        }
        None => MeaningSearch::Off,
    };
    Ok(SearchResults { tags, meetings: Some(group_by_meeting(hits, HITS_PER_MEETING)), meaning })
}

/// How far indexing Meetings in the background has got.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct IndexProgress {
    pub done: usize,
    pub total: usize,
}

/// Indexes, oldest first, every stopped Meeting whose Passages are missing or older than its files, telling
/// `progress` before the first and after each. A Meeting that can't be indexed is logged and counted as done.
pub fn index_stale(store: &MeetingStore, mut progress: impl FnMut(IndexProgress)) -> Result<(), StoreError> {
    let mut stale = Vec::new();
    for meeting in store.list()?.into_iter().rev() {
        let busy = matches!(meeting.status, MeetingStatus::Recording | MeetingStatus::Transcribing);
        if !busy && !store.passages_current(&meeting)? {
            stale.push(meeting.id);
        }
    }
    let total = stale.len();
    progress(IndexProgress { done: 0, total });
    for (n, id) in stale.iter().enumerate() {
        if let Err(e) = store.index_passages(id) {
            tracing::warn!("could not index the Passages of {id}: {e}");
        }
        progress(IndexProgress { done: n + 1, total });
    }
    Ok(())
}

/// Whether a Meeting with Tags `tags` has every one of `wanted`, ignoring case.
pub fn has_tags(tags: &[String], wanted: &[String]) -> bool {
    wanted.iter().all(|w| tags.iter().any(|t| same_tag(t, w)))
}

/// The lines as one line of plain text: list markers, task boxes, bold and code marks dropped.
fn joined(lines: &[&str]) -> String {
    lines.iter().map(|l| plain(l)).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
}

/// One line without its list marker, task box, bold or code marks.
fn plain(line: &str) -> String {
    let mut line = line.trim();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            line = rest.trim_start();
            break;
        }
    }
    for task in ["[ ] ", "[x] ", "[X] "] {
        if let Some(rest) = line.strip_prefix(task) {
            line = rest.trim_start();
        }
    }
    line.replace("**", "").replace('`', "")
}

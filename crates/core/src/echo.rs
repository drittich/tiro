//! Echo dedupe (ADR-0002): without headphones the mic re-hears the remote voices, so the You Track
//! transcribes much of what Others says. You words that match Others words heard at about the same time are
//! dropped; Others is never filtered.

use std::collections::VecDeque;

use crate::transcript::Word;

/// Filters the You Track's final words against the Others Track's.
///
/// You words arrive in batches that each end at a Segment boundary (see `TrackAssembler::accept_words`). A
/// batch is held until Others can no longer produce a word that would match it, then released filtered.
pub struct EchoFilter {
    /// How far apart (seconds, centre to centre) a You word and its Others echo may be.
    window: f64,
    /// A gap this long (seconds) separates a You word from its neighbours.
    pause: f64,
    others: Vec<OthersWord>,
    others_watermark: f64,
    you_watermark: f64,
    held: VecDeque<Vec<Word>>,
}

struct OthersWord {
    midpoint: f64,
    end: f64,
    text: String,
}

impl EchoFilter {
    pub fn new(window: f64, pause: f64) -> Self {
        Self { window, pause, others: Vec::new(), others_watermark: 0.0, you_watermark: 0.0, held: VecDeque::new() }
    }

    /// Takes a batch of final You words and the You Track's watermark; returns the batches now released.
    pub fn push_you(&mut self, words: Vec<Word>, watermark: f64) -> Vec<Vec<Word>> {
        if !words.is_empty() {
            self.held.push_back(words);
        }
        self.you_watermark = watermark;
        self.release()
    }

    /// Takes final Others words and the Others Track's watermark; returns the You batches now released.
    pub fn push_others(&mut self, words: &[Word], watermark: f64) -> Vec<Vec<Word>> {
        self.others.extend(words.iter().map(|w| OthersWord { midpoint: w.midpoint(), end: w.end, text: normalize(&w.text) }));
        self.others_watermark = watermark;
        self.release()
    }

    /// Every You word released from now on starts at or after this time.
    pub fn watermark(&self) -> f64 {
        self.held.front().map_or(self.you_watermark, |batch| batch[0].start.min(self.you_watermark))
    }

    fn release(&mut self) -> Vec<Vec<Word>> {
        let mut released = Vec::new();
        while let Some(batch) = self.held.front() {
            let end = batch.iter().map(|w| w.end).fold(f64::NEG_INFINITY, f64::max);
            if self.others_watermark < end + self.window {
                break;
            }
            let batch = self.held.pop_front().expect("front exists");
            let kept = self.drop_echo(batch);
            if !kept.is_empty() {
                released.push(kept);
            }
        }
        // Only Others words that a held or future You word could still match are worth keeping.
        let oldest_needed = self.watermark() - self.window;
        self.others.retain(|o| o.end >= oldest_needed);
        released
    }

    /// A You word echoes Others if a similar Others word is centred within `window` of it. Echo comes in
    /// runs, so a run of two or more such words is dropped, and so is a single one standing on its own; a
    /// single match right next to unmatched words is a coincidence inside genuine speech and is kept.
    /// Punctuation-only words share the fate of the word before them.
    fn drop_echo(&self, words: Vec<Word>) -> Vec<Word> {
        let texts: Vec<String> = words.iter().map(|w| normalize(&w.text)).collect();
        let spoken: Vec<usize> = (0..words.len()).filter(|&i| !texts[i].is_empty()).collect();
        let matched: Vec<bool> = spoken
            .iter()
            .map(|&i| {
                let mid = words[i].midpoint();
                self.others.iter().any(|o| (o.midpoint - mid).abs() <= self.window && similar(&texts[i], &o.text))
            })
            .collect();

        let mut drop = vec![false; words.len()];
        let mut run_start = 0;
        while run_start < spoken.len() {
            if !matched[run_start] {
                run_start += 1;
                continue;
            }
            let run_end = (run_start..spoken.len()).find(|&k| !matched[k]).unwrap_or(spoken.len());
            let lone = run_end - run_start == 1;
            let close = |a: usize, b: usize| words[spoken[b]].start - words[spoken[a]].end < self.pause;
            let beside_genuine = (run_start > 0 && close(run_start - 1, run_start))
                || (run_end < spoken.len() && close(run_start, run_end));
            if !(lone && beside_genuine) {
                for k in run_start..run_end {
                    drop[spoken[k]] = true;
                }
            }
            run_start = run_end;
        }
        // Punctuation-only words follow the word before them (or after, at the start of the batch).
        for i in 0..words.len() {
            if texts[i].is_empty() {
                let before = (0..i).rev().find(|&j| !texts[j].is_empty());
                let after = (i + 1..words.len()).find(|&j| !texts[j].is_empty());
                drop[i] = before.or(after).is_some_and(|j| drop[j]);
            }
        }
        words.into_iter().zip(drop).filter(|(_, d)| !d).map(|(w, _)| w).collect()
    }
}

/// Lower-case letters and digits only, so "It's," and "its" compare equal.
fn normalize(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The same word, allowing for the two Tracks hearing it slightly differently ("quick" / "quack") or splitting
/// it differently ("goodwill" / "good will", "reread" / "read"). Short words must match exactly, so "not" is
/// not "nothing".
fn similar(a: &str, b: &str) -> bool {
    const MIN_PART: usize = 4;
    let (short, long) = if a.chars().count() <= b.chars().count() { (a, b) } else { (b, a) };
    let short_len = short.chars().count();
    short == long
        || (short_len >= MIN_PART && (long.starts_with(short) || long.ends_with(short)))
        || (short_len >= MIN_PART && edit_distance(short, long) <= 1)
}

/// Levenshtein distance, in characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let substitute = diagonal + usize::from(ca != cb);
            diagonal = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

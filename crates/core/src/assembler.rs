//! Turning one Track's per-Chunk words into final Segments.

use crate::transcript::{Segment, Track, Word};

#[derive(Debug, Clone, Copy)]
pub struct AssemblerConfig {
    /// The most (seconds) a Chunk can overlap the previous one. After a silent gap there is no overlap.
    pub overlap: f64,
    /// A gap between words at least this long (seconds) ends a Segment.
    pub pause: f64,
    /// Segments longer than this (seconds) are split even without a pause.
    pub max_segment: f64,
}

/// Where a Chunk sits on the Meeting timeline, in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkSpan {
    pub index: u32,
    pub start: f64,
    pub end: f64,
}

/// Assembles one Track's Chunks, which must arrive in order.
///
/// Overlap dedupe: the overlap between consecutive Chunks (if any) is cut at its midpoint, and each word belongs to
/// whichever Chunk its centre falls in. Words that may still change or extend (anything not followed by a
/// pause that the current Chunk can vouch for) are held back until the next Chunk or `flush`.
pub struct TrackAssembler {
    track: Track,
    config: AssemblerConfig,
    /// Words not yet emitted, in Meeting time.
    pending: Vec<Word>,
    /// Where the next Chunk starts: none of its words can start earlier.
    next_start: f64,
    /// Where the previous Chunk ended, once there is one.
    prev_end: Option<f64>,
}

impl TrackAssembler {
    pub fn new(track: Track, config: AssemblerConfig) -> Self {
        Self { track, config, pending: Vec::new(), next_start: 0.0, prev_end: None }
    }

    /// Accepts a Chunk's words (times relative to the Chunk) and returns the Segments that are now final.
    pub fn accept(&mut self, span: ChunkSpan, words: Vec<Word>) -> Vec<Segment> {
        let done = self.accept_words(span, words);
        segments(self.track, &done, &self.config)
    }

    /// Like `accept`, but returns the final words (in Meeting time) instead of Segments. They always end at a
    /// Segment boundary, so `segments` turns each batch into the same Segments `accept` would have.
    pub fn accept_words(&mut self, span: ChunkSpan, words: Vec<Word>) -> Vec<Word> {
        let half = self.config.overlap / 2.0;
        let cut_in = self.prev_end.map_or(f64::NEG_INFINITY, |prev_end| span.start + (prev_end - span.start).max(0.0) / 2.0);
        self.prev_end = Some(span.end);
        self.pending.retain(|w| w.midpoint() < cut_in);
        self.pending.extend(
            words
                .into_iter()
                .map(|w| Word { text: w.text, start: w.start + span.start, end: w.end + span.start })
                .filter(|w| w.midpoint() >= cut_in),
        );
        self.pending.sort_by(|a, b| a.start.total_cmp(&b.start));
        self.next_start = span.end - self.config.overlap;

        let groups = group_ranges(&self.pending, &self.config);
        // The next Chunk cuts its overlap with this one no earlier than this.
        let cut_out = span.end - half;
        let mut finished = 0;
        for pair in groups.windows(2) {
            let (this, next) = (&pair[0], &pair[1]);
            let this_end = self.pending[this.end - 1].end;
            let next_word = &self.pending[next.start];
            // The gap after `this` is final if this Chunk owns the word that ends it, or if it is a pause even
            // before the next Chunk (whose words may start before the cut) gets a say.
            let vouched = next_word.midpoint() < cut_out || self.next_start - this_end >= self.config.pause;
            if !vouched {
                break;
            }
            finished = this.end;
        }
        self.pending.drain(..finished).collect()
    }

    /// The Track's next Chunk starts no earlier than `until`; returns the words that makes final.
    pub fn advance(&mut self, until: f64) -> Vec<Word> {
        self.next_start = self.next_start.max(until);
        match self.pending.last() {
            Some(last) if self.next_start - last.end >= self.config.pause => std::mem::take(&mut self.pending),
            _ => vec![],
        }
    }

    /// The Track has stopped: everything held back is final.
    pub fn flush(&mut self) -> Vec<Segment> {
        let done = self.flush_words();
        segments(self.track, &done, &self.config)
    }

    /// Like `flush`, but returns the words instead of Segments.
    pub fn flush_words(&mut self) -> Vec<Word> {
        self.next_start = f64::INFINITY;
        std::mem::take(&mut self.pending)
    }

    /// Every Segment this Track emits from now on starts at or after this time.
    pub fn watermark(&self) -> f64 {
        self.pending.first().map_or(self.next_start, |w| w.start.min(self.next_start))
    }
}

/// Splits ordered words into Segment-sized index ranges: at pauses, and when a Segment would get too long.
fn group_ranges(words: &[Word], config: &AssemblerConfig) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0;
    for i in 1..words.len() {
        let pause = words[i].start - words[i - 1].end >= config.pause;
        let too_long = words[i].end - words[start].start > config.max_segment;
        if pause || too_long {
            groups.push(start..i);
            start = i;
        }
    }
    if start < words.len() {
        groups.push(start..words.len());
    }
    groups
}

/// Builds Segments from one Track's final words, splitting them at pauses and at `max_segment`.
pub fn segments(track: Track, words: &[Word], config: &AssemblerConfig) -> Vec<Segment> {
    group_ranges(words, config)
        .into_iter()
        .map(|range| {
            let group = &words[range];
            Segment {
                track,
                start: group[0].start,
                end: group[group.len() - 1].end,
                text: group.iter().map(|w| w.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" "),
                speaker: None,
            }
        })
        .collect()
}

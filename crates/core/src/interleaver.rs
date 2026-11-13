//! Merging both Tracks' Segments onto one timeline.

use crate::transcript::{Segment, Track};

/// Holds Segments back until no Track can still produce an earlier one, then releases them in start order.
///
/// Each Track reports a watermark with its Segments: every Segment it emits later starts at or after it
/// (see `TrackAssembler::watermark`). A finished Track reports `f64::INFINITY`.
#[derive(Default)]
pub struct Interleaver {
    held: Vec<Segment>,
    you: f64,
    others: f64,
}

impl Interleaver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, track: Track, segments: Vec<Segment>, watermark: f64) -> Vec<Segment> {
        match track {
            Track::You => self.you = watermark,
            Track::Others => self.others = watermark,
        }
        self.held.extend(segments);
        self.held.sort_by(|a, b| a.start.total_cmp(&b.start));

        let safe = self.you.min(self.others);
        let ready = self.held.partition_point(|s| s.start <= safe);
        self.held.drain(..ready).collect()
    }
}

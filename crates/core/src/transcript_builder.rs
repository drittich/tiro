//! Turning both Tracks' transcribed Chunks into one Transcript.
//!
//! ```text
//! You words    ─▶ TrackAssembler ─▶ EchoFilter ─▶ Segments ─┐
//!                                       ▲                     ├─▶ Interleaver ─▶ Transcript, in time order
//! Others words ─▶ TrackAssembler ───────┴──────▶ Segments ─┘
//! ```

use std::collections::HashMap;

use crate::assembler::{segments, AssemblerConfig, ChunkSpan, TrackAssembler};
use crate::echo::EchoFilter;
use crate::interleaver::Interleaver;
use crate::transcript::{Segment, Track, Word};

/// Builds one Meeting's Transcript as its Chunks are transcribed.
pub struct TranscriptBuilder {
    config: AssemblerConfig,
    assemblers: HashMap<Track, TrackAssembler>,
    echo: EchoFilter,
    interleaver: Interleaver,
}

impl TranscriptBuilder {
    /// `tracks` are the Tracks being recorded; `echo_window` is how far apart (seconds) a You word and the
    /// Others word it echoes may be.
    pub fn new(tracks: &[Track], config: AssemblerConfig, echo_window: f64) -> Self {
        let mut builder = Self {
            config,
            assemblers: tracks.iter().map(|&t| (t, TrackAssembler::new(t, config))).collect(),
            echo: EchoFilter::new(echo_window, config.pause),
            interleaver: Interleaver::new(),
        };
        // A Track without an input never reports, so it must not hold the other back.
        for track in Track::ALL.into_iter().filter(|t| !tracks.contains(t)) {
            builder.route(track, vec![], f64::INFINITY);
        }
        builder
    }

    /// Accepts a transcribed Chunk (word times relative to the Chunk; Chunks in order per Track) and returns
    /// the Segments that are now final, in time order.
    pub fn accept(&mut self, track: Track, span: ChunkSpan, words: Vec<Word>) -> Vec<Segment> {
        let assembler = self.assemblers.get_mut(&track).expect("a recorded track");
        let done = assembler.accept_words(span, words);
        let watermark = assembler.watermark();
        self.route(track, done, watermark)
    }

    /// The Track has no speech before `until` beyond the Chunks already accepted: returns whatever that releases.
    pub fn silent(&mut self, track: Track, until: f64) -> Vec<Segment> {
        let Some(assembler) = self.assemblers.get_mut(&track) else { return vec![] };
        let done = assembler.advance(until);
        let watermark = assembler.watermark();
        self.route(track, done, watermark)
    }

    /// The Track has no more Chunks: returns whatever that releases.
    pub fn drain(&mut self, track: Track) -> Vec<Segment> {
        let Some(mut assembler) = self.assemblers.remove(&track) else { return vec![] };
        let done = assembler.flush_words();
        self.route(track, done, f64::INFINITY)
    }

    /// Every recorded Track has been drained.
    pub fn is_done(&self) -> bool {
        self.assemblers.is_empty()
    }

    fn route(&mut self, track: Track, words: Vec<Word>, watermark: f64) -> Vec<Segment> {
        let mut released = Vec::new();
        let you_words = match track {
            Track::Others => {
                let others = segments(Track::Others, &words, &self.config);
                released.extend(self.interleaver.push(Track::Others, others, watermark));
                self.echo.push_others(&words, watermark)
            }
            Track::You => self.echo.push_you(words, watermark),
        };
        let you: Vec<Segment> = you_words.iter().flat_map(|batch| segments(Track::You, batch, &self.config)).collect();
        released.extend(self.interleaver.push(Track::You, you, self.echo.watermark()));
        released
    }
}

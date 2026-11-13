//! Cutting a Track's audio into Chunks for transcription.

use crate::vad::VoiceDetector;

/// A slice of one Track's audio. `start` is seconds from the start of the Meeting.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub index: u32,
    pub start: f64,
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

impl Chunk {
    pub fn end(&self) -> f64 {
        self.start + self.samples.len() as f64 / self.sample_rate as f64
    }
}

/// Cuts a stream of samples into Chunks. Issue 05 replaces the fixed cutter with VAD-driven boundaries.
pub trait Chunker: Send {
    /// Feed captured samples; returns any Chunks that are now complete.
    fn push(&mut self, samples: &[i16]) -> Vec<Chunk>;
    /// The Track has stopped; returns the remaining partial Chunk, if it holds any new audio.
    fn finish(&mut self) -> Option<Chunk>;
    /// Every Chunk emitted from now on starts at or after this time (seconds from the start of the Meeting).
    fn horizon(&self) -> f64;
}

/// Fixed-length Chunks, each starting `overlap` before the previous one ended.
pub struct FixedChunker {
    sample_rate: u32,
    chunk_len: usize,
    overlap_len: usize,
    next_index: u32,
    /// Absolute sample position of `buffer[0]`.
    buffer_start: usize,
    buffer: Vec<i16>,
    /// Absolute sample position up to which audio has already been emitted.
    emitted_until: usize,
}

impl FixedChunker {
    pub fn new(sample_rate: u32, chunk_secs: f64, overlap_secs: f64) -> Self {
        let chunk_len = (chunk_secs * sample_rate as f64).round() as usize;
        let overlap_len = (overlap_secs * sample_rate as f64).round() as usize;
        assert!(overlap_len < chunk_len, "overlap must be shorter than the chunk");
        Self {
            sample_rate,
            chunk_len,
            overlap_len,
            next_index: 0,
            buffer_start: 0,
            buffer: Vec::with_capacity(chunk_len),
            emitted_until: 0,
        }
    }

    fn emit(&mut self, len: usize) -> Chunk {
        let chunk = Chunk {
            index: self.next_index,
            start: self.buffer_start as f64 / self.sample_rate as f64,
            sample_rate: self.sample_rate,
            samples: self.buffer[..len].to_vec(),
        };
        self.next_index += 1;
        self.emitted_until = self.buffer_start + len;
        chunk
    }
}

impl Chunker for FixedChunker {
    fn push(&mut self, mut samples: &[i16]) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        while !samples.is_empty() {
            let take = (self.chunk_len - self.buffer.len()).min(samples.len());
            self.buffer.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.buffer.len() == self.chunk_len {
                chunks.push(self.emit(self.chunk_len));
                let advance = self.chunk_len - self.overlap_len;
                self.buffer.drain(..advance);
                self.buffer_start += advance;
            }
        }
        chunks
    }

    fn finish(&mut self) -> Option<Chunk> {
        let buffer_end = self.buffer_start + self.buffer.len();
        if buffer_end <= self.emitted_until {
            return None;
        }
        let chunk = self.emit(self.buffer.len());
        self.buffer.clear();
        self.buffer_start = buffer_end;
        Some(chunk)
    }

    fn horizon(&self) -> f64 {
        self.buffer_start as f64 / self.sample_rate as f64
    }
}

/// Where `VadChunker` cuts. Times are seconds; probabilities come from the `VoiceDetector`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadConfig {
    /// A Chunk is cut at the first pause once it is at least this long (seconds)…
    pub min_chunk: f64,
    /// …and at its quietest moment if it reaches this length without one.
    pub max_chunk: f64,
    /// Silence at least this long (seconds) is a pause worth cutting at.
    pub min_pause: f64,
    /// Silence this long (seconds) ends the Chunk; nothing is sent until speech resumes.
    pub close_silence: f64,
    /// Audio kept (seconds) before speech starts and after it ends.
    pub padding: f64,
    /// A frame at or above this probability starts speech…
    pub threshold: f32,
    /// …and speech carries on until a frame falls below this one.
    pub neg_threshold: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            min_chunk: 20.0,
            max_chunk: 40.0,
            min_pause: 0.4,
            close_silence: 2.0,
            padding: 0.3,
            threshold: 0.5,
            neg_threshold: 0.35,
        }
    }
}

/// Chunks bounded by pauses: silent spans are skipped, and long speech is cut at a pause between `min_chunk`
/// and `max_chunk`, each continuation starting `overlap` before the cut.
///
/// All positions are absolute sample offsets from the start of the Track.
pub struct VadChunker<D: VoiceDetector> {
    detector: D,
    sample_rate: u32,
    config: VadConfig,
    frame_len: usize,
    overlap: usize,
    padding: usize,
    min_chunk: usize,
    max_chunk: usize,
    min_pause: usize,
    close_silence: usize,
    next_index: u32,
    /// Position of `buffer[0]`.
    buffer_start: usize,
    buffer: Vec<i16>,
    /// Audio up to here has been scored.
    scored: usize,
    speaking: bool,
    /// Where the current run of silent frames began.
    silence_start: Option<usize>,
    last_speech_end: usize,
    open: Option<OpenChunk>,
}

struct OpenChunk {
    start: usize,
    /// Whether the Chunk holds speech that no earlier Chunk has sent.
    has_new_speech: bool,
    /// The least speech-like frame (probability, position) at least `min_chunk` in: where to cut at `max_chunk`.
    quietest: Option<(f32, usize)>,
}

impl<D: VoiceDetector> VadChunker<D> {
    pub fn new(detector: D, sample_rate: u32, overlap_secs: f64, config: VadConfig) -> Self {
        let samples = |secs: f64| (secs * sample_rate as f64).round() as usize;
        Self {
            frame_len: detector.frame_len(),
            detector,
            sample_rate,
            config,
            overlap: samples(overlap_secs),
            padding: samples(config.padding),
            min_chunk: samples(config.min_chunk),
            max_chunk: samples(config.max_chunk),
            min_pause: samples(config.min_pause),
            close_silence: samples(config.close_silence),
            next_index: 0,
            buffer_start: 0,
            buffer: Vec::new(),
            scored: 0,
            speaking: false,
            silence_start: None,
            last_speech_end: 0,
            open: None,
        }
    }

    fn score_frame(&mut self) -> Option<Chunk> {
        let frame_start = self.scored;
        let from = frame_start - self.buffer_start;
        let p = self.detector.speech_probability(&self.buffer[from..from + self.frame_len]);
        self.scored += self.frame_len;
        if p >= self.config.threshold {
            self.speaking = true;
        } else if p < self.config.neg_threshold {
            self.speaking = false;
        }

        if self.speaking {
            self.silence_start = None;
            self.last_speech_end = self.scored;
            let start = frame_start.saturating_sub(self.padding).max(self.buffer_start);
            self.open.get_or_insert(OpenChunk { start, has_new_speech: false, quietest: None }).has_new_speech = true;
        } else {
            self.silence_start.get_or_insert(frame_start);
        }
        let Some(open) = &mut self.open else {
            // Idle: keep just enough audio to pad the start of the next speech.
            self.trim_to(self.scored.saturating_sub(self.padding));
            return None;
        };
        if frame_start >= open.start + self.min_chunk && open.quietest.is_none_or(|(q, _)| p < q) {
            open.quietest = Some((p, frame_start));
        }
        let length = self.scored - open.start;
        if let Some(silence_start) = self.silence_start {
            let silence = self.scored - silence_start;
            if silence >= self.close_silence {
                return self.close();
            }
            if length >= self.min_chunk && silence >= self.min_pause {
                return Some(self.cut(silence_start + self.min_pause / 2));
            }
        }
        if length >= self.max_chunk {
            let at = open.quietest.map_or(self.scored, |(_, at)| at);
            return Some(self.cut(at));
        }
        None
    }

    /// Ends the open Chunk at `at` and carries on with one starting `overlap` earlier.
    fn cut(&mut self, at: usize) -> Chunk {
        let open = self.open.as_mut().expect("open chunk");
        let start = open.start;
        let next_start = at - self.overlap;
        *open = OpenChunk { start: next_start, has_new_speech: self.last_speech_end > at, quietest: None };
        let chunk = self.emit(start, at);
        self.trim_to(next_start);
        chunk
    }

    /// Ends the open Chunk after its last speech, unless everything in it has already been sent.
    fn close(&mut self) -> Option<Chunk> {
        let open = self.open.take()?;
        let chunk = open.has_new_speech.then(|| self.emit(open.start, self.last_speech_end + self.padding));
        self.trim_to(self.scored.saturating_sub(self.padding));
        chunk
    }

    fn emit(&mut self, start: usize, end: usize) -> Chunk {
        let end = end.min(self.buffer_start + self.buffer.len());
        let chunk = Chunk {
            index: self.next_index,
            start: start as f64 / self.sample_rate as f64,
            sample_rate: self.sample_rate,
            samples: self.buffer[start - self.buffer_start..end - self.buffer_start].to_vec(),
        };
        self.next_index += 1;
        chunk
    }

    /// Drops buffered audio before `pos`.
    fn trim_to(&mut self, pos: usize) {
        if pos > self.buffer_start {
            self.buffer.drain(..pos - self.buffer_start);
            self.buffer_start = pos;
        }
    }
}

impl<D: VoiceDetector> Chunker for VadChunker<D> {
    fn push(&mut self, samples: &[i16]) -> Vec<Chunk> {
        self.buffer.extend_from_slice(samples);
        let mut chunks = Vec::new();
        while self.buffer_start + self.buffer.len() - self.scored >= self.frame_len {
            chunks.extend(self.score_frame());
        }
        chunks
    }

    fn finish(&mut self) -> Option<Chunk> {
        self.close()
    }

    fn horizon(&self) -> f64 {
        let next_start = match &self.open {
            Some(open) => open.start,
            None => self.scored.saturating_sub(self.padding).max(self.buffer_start),
        };
        next_start as f64 / self.sample_rate as f64
    }
}

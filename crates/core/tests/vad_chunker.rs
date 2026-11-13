//! The VAD-driven Chunker, with a scripted detector: a frame's speech probability is its first sample / 1000.

use tiro_core::chunker::{Chunk, Chunker, VadChunker, VadConfig};
use tiro_core::vad::VoiceDetector;

const RATE: u32 = 16_000;

/// Speech probability is the frame's first sample divided by 1000, so tests script it with sample values.
struct Scripted;

impl VoiceDetector for Scripted {
    fn frame_len(&self) -> usize {
        512
    }
    fn speech_probability(&mut self, frame: &[i16]) -> f32 {
        frame[0] as f32 / 1000.0
    }
}

const SPEECH: i16 = 1000;
const SILENCE: i16 = 0;

/// Builds a signal from (seconds, level) spans.
fn signal(spans: &[(f64, i16)]) -> Vec<i16> {
    spans.iter().flat_map(|&(secs, level)| std::iter::repeat_n(level, (secs * RATE as f64) as usize)).collect()
}

fn chunker() -> VadChunker<Scripted> {
    VadChunker::new(Scripted, RATE, 2.0, VadConfig::default())
}

fn feed(chunker: &mut impl Chunker, samples: &[i16]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for part in samples.chunks(1_000) {
        chunks.extend(chunker.push(part));
    }
    chunks.extend(chunker.finish());
    chunks
}

fn spans(chunks: &[Chunk]) -> Vec<(f64, f64)> {
    chunks.iter().map(|c| (round(c.start), round(c.end()))).collect()
}

fn round(secs: f64) -> f64 {
    (secs * 100.0).round() / 100.0
}

#[test]
fn silence_produces_no_chunks() {
    let chunks = feed(&mut chunker(), &signal(&[(90.0, SILENCE)]));
    assert!(chunks.is_empty(), "{:?}", spans(&chunks));
}

#[test]
fn speech_between_silences_is_one_padded_chunk_holding_that_audio() {
    let samples = signal(&[(10.24, SILENCE), (5.12, SPEECH), (10.24, SILENCE)]);
    let chunks = feed(&mut chunker(), &samples);

    // Speech is 10.24–15.36 s, padded by 0.3 s each side.
    assert_eq!(spans(&chunks), vec![(9.94, 15.66)]);
    let chunk = &chunks[0];
    let from = (chunk.start * RATE as f64).round() as usize;
    assert_eq!(chunk.samples, samples[from..from + chunk.samples.len()]);
    assert_eq!(chunk.index, 0);
}

#[test]
fn a_long_silence_ends_the_chunk_and_the_next_speech_starts_a_new_one() {
    let samples = signal(&[(5.12, SPEECH), (5.12, SILENCE), (5.12, SPEECH), (1.0, SILENCE)]);
    let chunks = feed(&mut chunker(), &samples);

    let indexes: Vec<u32> = chunks.iter().map(|c| c.index).collect();
    assert_eq!(spans(&chunks), vec![(0.0, 5.42), (9.94, 15.66)]);
    assert_eq!(indexes, vec![0, 1]);
}

#[test]
fn long_speech_is_cut_in_the_first_pause_after_min_chunk_and_the_next_chunk_overlaps_by_2s() {
    let samples = signal(&[
        (10.24, SPEECH),
        (0.64, SILENCE), // too early to cut
        (11.52, SPEECH),
        (0.64, SILENCE), // 22.40–23.04: cut 0.2 s into it
        (6.96, SPEECH),
        (3.0, SILENCE),
    ]);
    let chunks = feed(&mut chunker(), &samples);
    assert_eq!(spans(&chunks), vec![(0.0, 22.6), (20.6, 30.32)]);
}

#[test]
fn a_continuation_holding_only_already_sent_audio_is_not_emitted() {
    let chunks = feed(&mut chunker(), &signal(&[(20.48, SPEECH), (5.0, SILENCE)]));
    assert_eq!(spans(&chunks), vec![(0.0, 20.68)]);
}

#[test]
fn speech_without_a_pause_is_cut_at_its_quietest_moment_once_it_reaches_max_chunk() {
    let samples = signal(&[
        (25.024, SPEECH),
        (0.32, 450), // still speech, but quieter
        (4.672, SPEECH),
        (0.32, 400), // the quietest moment after 20 s: 30.016 s
        (14.656, SPEECH),
        (3.0, SILENCE),
    ]);
    let chunks = feed(&mut chunker(), &samples);
    assert_eq!(spans(&chunks), vec![(0.0, 30.02), (28.02, 45.29)]);
}

#[test]
fn a_track_that_stops_mid_speech_ends_its_chunk_there() {
    let chunks = feed(&mut chunker(), &signal(&[(1.0, SILENCE), (3.0, SPEECH)]));
    assert_eq!(spans(&chunks), vec![(0.72, 4.0)]);
}

#[test]
fn the_horizon_follows_silence_and_holds_at_the_start_of_an_open_chunk() {
    let mut chunker = chunker();

    chunker.push(&signal(&[(10.24, SILENCE)]));
    assert_eq!(round(chunker.horizon()), 9.94, "speech now would be padded back 0.3 s");

    chunker.push(&signal(&[(5.0, SPEECH)]));
    assert_eq!(round(chunker.horizon()), 9.94, "the open Chunk starts there");
}

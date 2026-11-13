mod common;

use tiro_core::chunker::{Chunk, Chunker, FixedChunker};

const RATE: u32 = 16_000;

fn feed(chunker: &mut impl Chunker, samples: &[i16], piece: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for part in samples.chunks(piece) {
        chunks.extend(chunker.push(part));
    }
    chunks.extend(chunker.finish());
    chunks
}

#[test]
fn a_62s_track_becomes_three_30s_chunks_overlapping_by_2s() {
    let samples = common::fixture_samples("others-62s.wav");
    let mut chunker = FixedChunker::new(RATE, 30.0, 2.0);

    let chunks = feed(&mut chunker, &samples, 1_600);

    let spans: Vec<(u32, f64, f64)> = chunks.iter().map(|c| (c.index, c.start, c.end())).collect();
    assert_eq!(spans, vec![(0, 0.0, 30.0), (1, 28.0, 58.0), (2, 56.0, 62.0)]);
}

#[test]
fn chunk_audio_is_the_exact_slice_of_the_track() {
    let samples = common::fixture_samples("you-62s.wav");
    let mut chunker = FixedChunker::new(RATE, 30.0, 2.0);

    let chunks = feed(&mut chunker, &samples, 4_321);

    for chunk in &chunks {
        let from = (chunk.start * RATE as f64) as usize;
        assert_eq!(chunk.samples, samples[from..from + chunk.samples.len()], "chunk {}", chunk.index);
    }
}

#[test]
fn stopping_mid_overlap_does_not_emit_a_chunk_that_only_repeats_the_overlap() {
    // 31 s: the second Chunk would hold only 28–31 s, which is all overlap except 1 s, so it is still emitted.
    // 29 s: everything after 28 s is already in the first Chunk, so nothing more is emitted.
    let mut chunker = FixedChunker::new(RATE, 30.0, 2.0);
    let chunks = feed(&mut chunker, &vec![0i16; 29 * RATE as usize], 16_000);
    let spans: Vec<(f64, f64)> = chunks.iter().map(|c| (c.start, c.end())).collect();
    assert_eq!(spans, vec![(0.0, 29.0)]);

    let mut chunker = FixedChunker::new(RATE, 30.0, 2.0);
    let chunks = feed(&mut chunker, &vec![0i16; 31 * RATE as usize], 16_000);
    let spans: Vec<(f64, f64)> = chunks.iter().map(|c| (c.start, c.end())).collect();
    assert_eq!(spans, vec![(0.0, 30.0), (28.0, 31.0)]);
}

#[test]
fn finishing_an_empty_track_emits_nothing() {
    let mut chunker = FixedChunker::new(RATE, 30.0, 2.0);
    assert!(chunker.finish().is_none());
}

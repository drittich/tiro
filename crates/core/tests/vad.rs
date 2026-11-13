//! Silero VAD driving the Chunker over real audio.

mod common;

use tiro_core::chunker::{Chunk, Chunker, VadChunker, VadConfig};
use tiro_core::vad::SileroVad;

const RATE: u32 = 16_000;

fn chunks_of(samples: &[i16]) -> Vec<Chunk> {
    let mut chunker = VadChunker::new(SileroVad::new().unwrap(), RATE, 2.0, VadConfig::default());
    let mut chunks = Vec::new();
    for part in samples.chunks(1_600) {
        chunks.extend(chunker.push(part));
    }
    chunks.extend(chunker.finish());
    chunks
}

/// RMS level of the audio in `[from, to)` seconds.
fn rms(samples: &[i16], from: f64, to: f64) -> f64 {
    let range = &samples[(from * RATE as f64) as usize..((to * RATE as f64) as usize).min(samples.len())];
    (range.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / range.len() as f64).sqrt()
}

#[test]
fn silence_and_quiet_room_noise_produce_no_chunks() {
    let mut seed = 1u32;
    let noise = std::iter::repeat_with(|| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((seed >> 16) as i16 % 120) - 60
    });
    let samples: Vec<i16> = std::iter::repeat_n(0, 30 * RATE as usize).chain(noise.take(60 * RATE as usize)).collect();

    let chunks = chunks_of(&samples);

    assert!(chunks.is_empty(), "{:?}", chunks.iter().map(|c| (c.start, c.end())).collect::<Vec<_>>());
}

#[test]
fn chunk_boundaries_in_meeting_speech_land_in_pauses() {
    let samples = common::fixture_samples("others-62s.wav");
    let speech_level = rms(&samples, 0.0, 62.0);

    let chunks = chunks_of(&samples);

    assert!(chunks.len() >= 2, "expected the 62 s of speech to be split");
    for (i, chunk) in chunks.iter().enumerate() {
        let length = chunk.end() - chunk.start;
        assert!(length <= 40.0, "chunk {} is {length:.1} s", chunk.index);
        // A continuation deliberately starts 2 s before the cut, so only a fresh start must be in a pause.
        let continues = i > 0 && chunk.start < chunks[i - 1].end();
        let edges = if continues { vec![chunk.end()] } else { vec![chunk.start, chunk.end()] };
        for edge in edges {
            if edge > 0.0 && edge < 61.9 {
                let level = rms(&samples, edge - 0.1, edge + 0.1);
                assert!(level < speech_level * 0.1, "chunk {} edge at {edge:.2} s is not in a pause ({level:.0} vs {speech_level:.0})", chunk.index);
            }
        }
    }
}

#[test]
fn no_speech_falls_outside_every_chunk() {
    for name in ["others-62s.wav", "you-62s.wav"] {
        let samples = common::fixture_samples(name);
        let speech_level = rms(&samples, 0.0, 62.0);
        let chunks = chunks_of(&samples);

        let mut t = 0.0;
        while t + 0.2 <= 62.0 {
            if rms(&samples, t, t + 0.2) > speech_level * 0.5 {
                assert!(chunks.iter().any(|c| c.start <= t && t + 0.2 <= c.end()), "{name}: speech at {t:.1} s is in no chunk");
            }
            t += 0.2;
        }
    }
}

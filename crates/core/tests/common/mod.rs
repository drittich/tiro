#![allow(dead_code)]

use std::path::PathBuf;

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// 62 s of 16 kHz mono s16 synthetic speech (espeak-ng): Others is a two-voice conversation, You is a quiet
/// echo of it plus a few interjections, as a mic picks up without headphones.
pub fn fixture_samples(name: &str) -> Vec<i16> {
    hound::WavReader::open(fixture(name))
        .unwrap()
        .into_samples::<i16>()
        .map(Result::unwrap)
        .collect()
}

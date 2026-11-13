mod common;

use std::path::Path;
use std::process::Command;

use tiro_core::encoder::write_ogg_opus;

fn probe(path: &Path, entries: &str) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", entries, "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .expect("ffprobe is installed");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Seconds of audio the file decodes to. (ffprobe's container duration counts the pre-skip, so it reads long.)
fn decoded_seconds(path: &Path) -> f64 {
    let out = Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(path).args(["-f", "s16le", "-ar", "48000", "-"]).output().unwrap();
    out.stdout.len() as f64 / 2.0 / 48_000.0
}

fn decode_errors(path: &Path) -> String {
    let out = Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(path).args(["-f", "null", "-"]).output().unwrap();
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_30s_chunk_round_trips_as_mono_opus_of_the_same_length_at_about_24kbps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("others-0.ogg");
    let samples = common::fixture_samples("others-62s.wav");

    write_ogg_opus(&path, 16_000, &samples[..30 * 16_000]).unwrap();

    assert_eq!(probe(&path, "stream=codec_name,channels"), "opus\n1");
    let duration = decoded_seconds(&path);
    assert!((duration - 30.0).abs() < 0.001, "duration {duration}");
    let bytes = std::fs::metadata(&path).unwrap().len();
    assert!((60_000..130_000).contains(&bytes), "{bytes} bytes for 30 s");
    assert_eq!(decode_errors(&path), "");
}

#[test]
fn a_short_final_chunk_that_is_not_a_whole_number_of_frames_keeps_its_exact_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("you-2.ogg");
    let samples = common::fixture_samples("you-62s.wav");

    // 6.01 s: 300 whole 20 ms frames plus 10 ms.
    write_ogg_opus(&path, 16_000, &samples[..96_160]).unwrap();

    let duration = decoded_seconds(&path);
    assert!((duration - 6.01).abs() < 0.001, "duration {duration}");
    assert_eq!(decode_errors(&path), "");
}

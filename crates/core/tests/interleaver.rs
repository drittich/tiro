use tiro_core::interleaver::Interleaver;
use tiro_core::transcript::{Segment, Track};

fn seg(track: Track, start: f64, text: &str) -> Segment {
    Segment { track, start, end: start + 1.0, text: text.into(), speaker: None }
}

fn texts(segments: &[Segment]) -> Vec<&str> {
    segments.iter().map(|s| s.text.as_str()).collect()
}

#[test]
fn segments_wait_until_the_lagging_track_has_caught_up() {
    let mut transcript = Interleaver::new();

    // Others transcribed 0–29 s; You has not reported anything yet.
    let out = transcript.push(Track::Others, vec![seg(Track::Others, 3.0, "Hi all"), seg(Track::Others, 12.0, "Agenda")], 29.0);
    assert!(out.is_empty());

    // You reports up to 10 s: only what started by then is safe to release.
    let out = transcript.push(Track::You, vec![seg(Track::You, 5.0, "Hello")], 10.0);
    assert_eq!(texts(&out), vec!["Hi all", "Hello"]);

    let out = transcript.push(Track::You, vec![seg(Track::You, 20.0, "Sounds good")], 40.0);
    assert_eq!(texts(&out), vec!["Agenda", "Sounds good"]);
}

#[test]
fn finishing_both_tracks_releases_everything_in_time_order() {
    let mut transcript = Interleaver::new();

    let mut out = transcript.push(Track::You, vec![seg(Track::You, 40.0, "Bye"), seg(Track::You, 2.0, "Morning")], f64::INFINITY);
    out.extend(transcript.push(Track::Others, vec![seg(Track::Others, 1.0, "Hey"), seg(Track::Others, 39.0, "Cheers")], f64::INFINITY));

    assert_eq!(texts(&out), vec!["Hey", "Morning", "Cheers", "Bye"]);
}

#[test]
fn a_finished_track_no_longer_holds_the_other_back() {
    let mut transcript = Interleaver::new();
    transcript.push(Track::You, vec![], f64::INFINITY);

    let out = transcript.push(Track::Others, vec![seg(Track::Others, 100.0, "Still talking")], 120.0);

    assert_eq!(texts(&out), vec!["Still talking"]);
}

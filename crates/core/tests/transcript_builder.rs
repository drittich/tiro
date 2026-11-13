mod common;

use tiro_core::assembler::{AssemblerConfig, ChunkSpan};
use tiro_core::transcript::{Segment, Track, Word};
use tiro_core::transcript_builder::TranscriptBuilder;

fn w(text: &str, start: f64, end: f64) -> Word {
    Word { text: text.into(), start, end }
}

fn config() -> AssemblerConfig {
    AssemblerConfig { overlap: 2.0, pause: 1.0, max_segment: 30.0 }
}

fn span(index: u32, start: f64, end: f64) -> ChunkSpan {
    ChunkSpan { index, start, end }
}

fn builder() -> TranscriptBuilder {
    TranscriptBuilder::new(&Track::ALL, config(), 1.0)
}

fn texts(segments: &[Segment], track: Track) -> Vec<&str> {
    segments.iter().filter(|s| s.track == track).map(|s| s.text.as_str()).collect()
}

/// Words in the shape scribe-v2 returns for a recording without headphones: You re-hears nearly all of
/// Others, plus a few genuine interjections from the user.
fn echo_fixture() -> (Vec<Word>, Vec<Word>) {
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(common::fixture("echo-words.json")).unwrap()).unwrap();
    let words = |track: &str| serde_json::from_value(json[track].clone()).unwrap();
    (words("you"), words("others"))
}

#[test]
fn the_echo_fixture_loses_its_duplicate_you_speech_and_keeps_the_genuine_interjections() {
    let (you, others) = echo_fixture();
    let others_text: Vec<&str> = others.iter().map(|w| w.text.as_str()).collect();
    let others_text = others_text.join(" ");
    let mut b = builder();

    let mut out = b.accept(Track::You, span(0, 0.0, 62.0), you);
    out.extend(b.accept(Track::Others, span(0, 0.0, 62.0), others));
    out.extend(b.drain(Track::You));
    out.extend(b.drain(Track::Others));

    assert_eq!(texts(&out, Track::You).join(" "), "Great ... Nice. Yeah. Okay. Okay.");
    assert_eq!(texts(&out, Track::Others).join(" "), others_text, "Others is never filtered");
    assert!(out.windows(2).all(|p| p[0].start <= p[1].start), "the Transcript is in time order");
}

#[test]
fn you_speech_waits_until_others_has_been_heard_past_it() {
    let mut b = builder();

    let out = b.accept(Track::You, span(0, 0.0, 20.0), vec![w("Sounds", 3.0, 3.4), w("good.", 3.5, 3.9), w("Next", 12.0, 12.4)]);
    assert!(out.is_empty(), "Others might still be saying the same thing");

    // Others now can't say anything earlier than 8 s, well clear of "Sounds good." at 3–3.9 s.
    let out = b.accept(Track::Others, span(0, 0.0, 20.0), vec![w("Right,", 8.0, 8.4)]);
    assert_eq!(texts(&out, Track::You), vec!["Sounds good."]);
}

#[test]
fn a_genuine_word_that_happens_to_match_others_is_kept_inside_genuine_speech() {
    let mut b = builder();

    // Both say "that" at about the same moment, but only one word of You matches.
    let mut out = b.accept(Track::Others, span(0, 0.0, 20.0), vec![w("Is", 5.0, 5.2), w("that", 5.3, 5.5), w("done?", 5.6, 6.0)]);
    out.extend(b.accept(Track::You, span(0, 0.0, 20.0), vec![w("I", 5.0, 5.1), w("think", 5.15, 5.25), w("that", 5.3, 5.5), w("works.", 5.6, 6.0)]));
    out.extend(b.drain(Track::Others));
    out.extend(b.drain(Track::You));

    assert_eq!(texts(&out, Track::You), vec!["I think that works."]);
}

#[test]
fn a_lone_echoed_word_is_dropped() {
    let mut b = builder();

    let mut out = b.accept(Track::Others, span(0, 0.0, 20.0), vec![w("Yes.", 5.3, 5.6)]);
    out.extend(b.accept(Track::You, span(0, 0.0, 20.0), vec![w("yes", 5.2, 5.5)]));
    out.extend(b.drain(Track::Others));
    out.extend(b.drain(Track::You));

    assert_eq!(texts(&out, Track::You), Vec::<&str>::new());
}

#[test]
fn without_an_others_track_you_passes_straight_through() {
    let mut b = TranscriptBuilder::new(&[Track::You], config(), 1.0);

    let out = b.accept(Track::You, span(0, 0.0, 20.0), vec![w("Testing.", 3.0, 3.5), w("Again", 12.0, 12.4)]);

    assert_eq!(texts(&out, Track::You), vec!["Testing."]);
}

#[test]
fn a_silent_track_does_not_hold_the_other_back() {
    let mut b = builder();

    let out = b.accept(Track::Others, span(0, 0.0, 20.0), vec![w("Morning.", 3.0, 3.5), w("So", 12.0, 12.2)]);
    assert!(out.is_empty(), "You might still say something earlier");

    let out = b.silent(Track::You, 30.0);
    assert_eq!(texts(&out, Track::Others), vec!["Morning."]);
}

#[test]
fn silence_after_a_track_s_last_words_closes_its_segment() {
    let mut b = builder();
    b.silent(Track::Others, 60.0);

    let out = b.accept(Track::You, span(0, 0.0, 6.0), vec![w("One", 3.0, 3.5), w("more.", 3.6, 4.0)]);
    assert!(out.is_empty(), "the next Chunk might continue the Segment");

    let out = b.silent(Track::You, 8.0);
    assert_eq!(texts(&out, Track::You), vec!["One more."]);
}

/// Splits a Track's words (Meeting time) into overlapping Chunks, as a transcriber would return them.
fn chunked(words: &[Word], spans: &[(f64, f64)]) -> Vec<(ChunkSpan, Vec<Word>)> {
    spans
        .iter()
        .enumerate()
        .map(|(i, &(start, end))| {
            let inside = words.iter().filter(|w| w.start >= start && w.end <= end);
            (span(i as u32, start, end), inside.map(|w| self::w(&w.text, w.start - start, w.end - start)).collect())
        })
        .collect()
}

#[test]
fn the_echo_fixture_is_deduped_the_same_when_it_arrives_in_vad_chunks() {
    let (you, others) = echo_fixture();
    // The Chunks Silero VAD cuts from the two 62 s fixtures.
    let you = chunked(&you, &[(0.0, 21.61), (19.61, 39.21), (37.21, 58.79), (56.79, 62.0)]);
    let others = chunked(&others, &[(0.0, 21.77), (19.77, 39.46), (37.46, 59.02), (57.02, 62.0)]);
    let mut b = builder();

    let mut out = Vec::new();
    for ((you_span, you_words), (others_span, others_words)) in you.into_iter().zip(others) {
        out.extend(b.accept(Track::You, you_span, you_words));
        out.extend(b.accept(Track::Others, others_span, others_words));
    }
    out.extend(b.drain(Track::You));
    out.extend(b.drain(Track::Others));

    assert_eq!(texts(&out, Track::You).join(" "), "Great ... Nice. Yeah. Okay. Okay.");
    assert!(out.windows(2).all(|p| p[0].start <= p[1].start), "the Transcript is in time order");
}

#[test]
fn a_misheard_echo_word_does_not_leak_through() {
    let mut b = builder();

    let mut out = b.accept(Track::Others, span(0, 0.0, 20.0), vec![w("the", 5.0, 5.2), w("quick", 5.3, 5.6), w("fox.", 5.7, 6.0)]);
    out.extend(b.accept(Track::You, span(0, 0.0, 20.0), vec![w("the", 5.0, 5.2), w("quack", 5.3, 5.6)]));
    out.extend(b.drain(Track::Others));
    out.extend(b.drain(Track::You));

    assert_eq!(texts(&out, Track::You), Vec::<&str>::new());
}

#[test]
fn short_words_inside_longer_others_words_are_not_echo() {
    let mut b = builder();

    // "not" is inside "nothing" and "the" inside "other", but these are different words.
    let mut out = b.accept(Track::Others, span(0, 0.0, 20.0), vec![w("nothing", 5.0, 5.4), w("other.", 5.5, 5.9)]);
    out.extend(b.accept(Track::You, span(0, 0.0, 20.0), vec![w("not", 5.0, 5.2), w("the", 5.5, 5.7)]));
    out.extend(b.drain(Track::Others));
    out.extend(b.drain(Track::You));

    assert_eq!(texts(&out, Track::You), vec!["not the"]);
}

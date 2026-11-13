use tiro_core::assembler::{AssemblerConfig, ChunkSpan, TrackAssembler};
use tiro_core::transcript::{Segment, Track, Word};

fn w(text: &str, start: f64, end: f64) -> Word {
    Word { text: text.into(), start, end }
}

fn config() -> AssemblerConfig {
    AssemblerConfig { overlap: 2.0, pause: 1.0, max_segment: 30.0 }
}

fn span(index: u32, start: f64, end: f64) -> ChunkSpan {
    ChunkSpan { index, start, end }
}

fn texts(segments: &[Segment]) -> Vec<&str> {
    segments.iter().map(|s| s.text.as_str()).collect()
}

#[test]
fn words_are_offset_by_the_chunk_start_and_split_into_segments_at_pauses() {
    let mut asm = TrackAssembler::new(Track::Others, config());

    let mut out = asm.accept(span(1, 28.0, 58.0), vec![]);
    out.extend(asm.accept(
        span(2, 56.0, 86.0),
        vec![w("Good", 2.0, 2.25), w(" morning.", 2.5, 3.0), w("Shall", 4.5, 4.75), w("we", 5.0, 5.25)],
    ));
    out.extend(asm.flush());

    assert_eq!(
        out,
        vec![
            Segment { track: Track::Others, start: 58.0, end: 59.0, text: "Good morning.".into(), speaker: None },
            Segment { track: Track::Others, start: 60.5, end: 61.25, text: "Shall we".into(), speaker: None },
        ]
    );
}

#[test]
fn words_repeated_in_the_overlap_appear_once() {
    let mut asm = TrackAssembler::new(Track::You, config());

    // Chunk 0 is 0–30 s and chunk 1 is 28–58 s; both hear "the overlap region" around 28.2–29.6 s.
    let mut out = asm.accept(
        span(0, 0.0, 30.0),
        vec![w("This", 27.0, 27.3), w("is", 27.4, 27.6), w("the", 28.2, 28.4), w("overlap", 28.5, 29.0), w("region", 29.1, 29.6)],
    );
    out.extend(asm.accept(
        span(1, 28.0, 58.0),
        vec![w("the", 0.2, 0.4), w("overlap", 0.5, 1.0), w("region", 1.1, 1.6), w("ends.", 1.7, 2.0)],
    ));
    out.extend(asm.flush());

    assert_eq!(texts(&out), vec!["This is the overlap region ends."]);
}

#[test]
fn the_overlap_is_cut_at_its_midpoint_by_word_centre() {
    let mut asm = TrackAssembler::new(Track::You, config());

    // The cut between 0–30 and 28–58 is 29 s. Chunk 0 mishears the word centred before the cut,
    // chunk 1 mishears the word centred after it; each Chunk is trusted on its own side.
    let mut out = asm.accept(span(0, 0.0, 30.0), vec![w("alpha", 28.3, 28.9), w("BRAVO?", 29.1, 29.7)]);
    out.extend(asm.accept(span(1, 28.0, 58.0), vec![w("ALFA?", 0.3, 0.9), w("bravo", 1.1, 1.7)]));
    out.extend(asm.flush());

    assert_eq!(texts(&out), vec!["alpha bravo"]);
}

#[test]
fn a_segment_is_emitted_live_once_a_pause_before_the_overlap_closes_it() {
    let mut asm = TrackAssembler::new(Track::Others, config());

    let out = asm.accept(
        span(0, 0.0, 30.0),
        vec![w("First", 5.0, 5.4), w("thought.", 5.5, 6.0), w("Second", 27.5, 28.0), w("thought", 28.5, 29.5)],
    );

    // "Second thought" may continue into the next Chunk, so it is held back.
    assert_eq!(texts(&out), vec!["First thought."]);
    assert_eq!(asm.watermark(), 27.5);

    let out = asm.accept(span(1, 28.0, 58.0), vec![w("thought", 0.5, 1.5), w("continues.", 1.6, 2.4)]);
    assert!(out.is_empty(), "still no pause after it");

    let out = asm.flush();
    assert_eq!(texts(&out), vec!["Second thought continues."]);
    assert_eq!(asm.watermark(), f64::INFINITY);
}

#[test]
fn a_pause_that_reaches_past_the_cut_does_not_close_a_segment_yet() {
    let mut asm = TrackAssembler::new(Track::Others, config());

    // Chunk 0 hears a 1.1 s gap after "Hello", but the gap runs past the 29 s cut, so chunk 1 has the final say.
    let out = asm.accept(span(0, 0.0, 30.0), vec![w("Hello", 27.5, 28.25), w("Later", 29.375, 29.75)]);
    assert!(out.is_empty());

    let out = asm.accept(span(1, 28.0, 58.0), vec![w("there.", 1.0, 1.25), w("Next", 10.0, 10.5)]);
    assert_eq!(texts(&out), vec!["Hello there."]);
}

#[test]
fn a_pause_is_only_trusted_up_to_where_the_next_chunk_starts() {
    let mut asm = TrackAssembler::new(Track::Others, config());

    // Chunk 0 hears 1.25 s of silence before the 29 s cut, but chunk 1 starts at 28 s and may hear a word
    // there that is centred past the cut ("there", 28.0–30.5 s), so "Hello" cannot be closed yet.
    let out = asm.accept(span(0, 0.0, 30.0), vec![w("Hello", 27.0, 27.75), w("Later", 29.5, 30.0)]);
    assert!(out.is_empty());

    let mut out = asm.accept(span(1, 28.0, 58.0), vec![w("there.", 0.0, 2.5)]);
    out.extend(asm.flush());
    assert_eq!(texts(&out), vec!["Hello there."]);
}

#[test]
fn the_watermark_after_a_silent_chunk_is_where_the_next_chunk_starts() {
    // The next Chunk keeps words by their centre, so one may start before the cut, but none before the Chunk.
    let mut asm = TrackAssembler::new(Track::You, config());
    asm.accept(span(0, 0.0, 30.0), vec![]);
    assert_eq!(asm.watermark(), 28.0);
}

#[test]
fn a_long_monologue_without_pauses_is_split_at_the_maximum_segment_length() {
    let mut asm = TrackAssembler::new(Track::Others, AssemblerConfig { max_segment: 10.0, ..config() });

    let words: Vec<Word> = (0..25).map(|i| w("word", i as f64, i as f64 + 0.9)).collect();
    let mut out = asm.accept(span(0, 0.0, 30.0), words);
    out.extend(asm.flush());

    let spans: Vec<(f64, f64)> = out.iter().map(|s| (s.start, s.end)).collect();
    assert_eq!(spans, vec![(0.0, 9.9), (10.0, 19.9), (20.0, 24.9)]);
}

#[test]
fn a_chunk_after_a_silent_gap_keeps_its_first_words() {
    let mut asm = TrackAssembler::new(Track::You, config());

    // VAD skipped 10–15 s, so chunk 1 does not overlap chunk 0 at all.
    let mut out = asm.accept(span(0, 0.0, 10.0), vec![w("Hi.", 1.0, 1.5)]);
    out.extend(asm.accept(span(1, 15.0, 25.0), vec![w("Okay,", 0.3, 0.6), w("so.", 0.7, 0.9)]));
    out.extend(asm.flush());

    assert_eq!(texts(&out), vec!["Hi.", "Okay, so."]);
}

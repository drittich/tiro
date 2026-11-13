use tiro_core::transcript::{cleaned, without_fillers, Segment, Track};

#[track_caller]
fn check(raw: &str, clean: &str) {
    assert_eq!(without_fillers(raw), clean, "from {raw:?}");
}

#[test]
fn fillers_and_audio_event_tags_are_dropped_from_real_segments() {
    check(
        "Um, I can start with a quick status update. [clears throat] We got the designs back",
        "I can start with a quick status update. We got the designs back",
    );
    check("a number of, uh, sample files", "a number of sample files");
    check("So yes, you're up next. [chuckles] Um, as usual", "So yes, you're up next. As usual");
    check("[laughs] [laughs] I know Sam can cook.", "I know Sam can cook.");
}

#[test]
fn a_segment_of_only_fillers_or_tags_becomes_empty() {
    for raw in ["[clicking]", "[ ]", "[]", "Um", "Uh,", "*ahem*"] {
        check(raw, "");
    }
}

#[test]
fn a_sentence_end_on_a_dropped_filler_moves_to_the_previous_word() {
    check("it was, um.", "it was.");
    check("Really, um? Yes", "Really? Yes");
    check("We were done. [laughs]. Next", "We were done. Next");
}

#[test]
fn a_comma_before_a_dropped_filler_goes_when_the_filler_trails_off() {
    check("Let me, uh... check", "Let me check");
    check("And, um-", "And");
    check("uh, which is better. Um,", "which is better.");
    check("Wait, uh… what", "Wait what");
}

#[test]
fn only_a_capitalized_filler_opening_a_sentence_capitalizes_the_next_word() {
    check("Done. Uh, so next", "Done. So next");
    check("Done. uh, so next", "Done. so next");
    check("Done. [laughs] so next", "Done. so next");
}

#[test]
fn asterisk_events_are_dropped_like_bracketed_ones() {
    check("the *cough cough* bank", "the bank");
}

#[test]
fn backchannels_and_words_that_start_like_fillers_are_kept() {
    for raw in ["Mm-hmm.", "Mhm.", "Uh-huh.", "Hmm.", "Umbrella", "Erm-like"] {
        check(raw, raw);
    }
}

#[test]
fn an_unclosed_bracket_is_left_alone() {
    check("see [the note", "see [the note");
}

#[test]
fn spacing_is_normalized_to_single_spaces() {
    check("  so   um  yes ", "so yes");
}

#[test]
fn cleaning_a_transcript_drops_segments_that_become_empty_and_keeps_the_rest() {
    let seg = |start: f64, text: &str| Segment { track: Track::Others, start, end: start + 1.0, text: text.into(), speaker: Some(2) };
    let raw = vec![seg(1.0, "[laughs]"), seg(2.0, "Um, okay."), seg(3.0, "Uh,")];
    assert_eq!(cleaned(&raw), vec![seg(2.0, "Okay.")]);
    assert_eq!(raw[1].text, "Um, okay.", "the input is untouched");
}

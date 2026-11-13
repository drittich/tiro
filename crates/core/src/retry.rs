//! "Retry failed Chunks" on a stopped Meeting: Chunks left in `chunks/` (rejected, blocked, or still queued when
//! the app quit) are transcribed again and merged into the Transcript.

use std::collections::HashMap;
use std::fs;

use serde::Serialize;

use crate::assembler::{segments, AssemblerConfig, ChunkSpan, TrackAssembler};
use crate::echo::EchoFilter;
use crate::pipeline::PipelineConfig;
use crate::spend::{SpendKind, SpendLedger};
use crate::store::{MeetingStatus, MeetingStore};
use crate::transcriber::{ChunkAudio, TranscribeError, Transcriber};
use crate::transcript::{Segment, Track, Word};

/// The name a Chunk is written under: its Track, index and span in milliseconds, so it can be placed on the
/// timeline again after a restart.
pub fn chunk_file_name(track: Track, span: ChunkSpan) -> String {
    let ms = |s: f64| (s * 1000.0).round() as u64;
    format!("{}-{:05}-{:09}-{:09}.ogg", track.as_str(), span.index, ms(span.start), ms(span.end))
}

/// The inverse of `chunk_file_name`.
pub fn parse_chunk_file_name(name: &str) -> Option<(Track, ChunkSpan)> {
    let stem = name.strip_suffix(".ogg").or_else(|| name.strip_suffix(".flac"))?;
    let parts: Vec<&str> = stem.split('-').collect();
    let [track, index, start, end] = parts[..] else { return None };
    let track = Track::ALL.into_iter().find(|t| t.as_str() == track)?;
    let secs = |s: &str| s.parse::<u64>().ok().map(|ms| ms as f64 / 1000.0);
    Some((track, ChunkSpan { index: index.parse().ok()?, start: secs(start)?, end: secs(end)? }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryReport {
    /// Chunks transcribed this time.
    pub recovered: usize,
    /// Chunks still on disk.
    pub remaining: usize,
    /// The last error, if any Chunk failed again.
    pub error: Option<String>,
}

/// Tries each leftover Chunk once, in time order, and merges what it recovers into `transcript.jsonl`. A ZDR
/// violation or a rejected key stops the retry. The Meeting is complete once no Chunks remain. What each request
/// cost goes in `spend`.
pub async fn retry_failed_chunks(
    store: &MeetingStore,
    id: &str,
    transcriber: &dyn Transcriber,
    config: &PipelineConfig,
    spend: &SpendLedger,
) -> RetryReport {
    let mut report = RetryReport { recovered: 0, remaining: 0, error: None };
    let meeting = match store.get(id) {
        Ok(meeting) => meeting,
        Err(e) => return RetryReport { error: Some(format!("could not read the Meeting: {e}")), ..report },
    };

    let mut chunks = Vec::new();
    for entry in fs::read_dir(meeting.chunks_dir()).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match parse_chunk_file_name(&name) {
            Some((track, span)) => chunks.push((track, span, entry.path())),
            None => {
                tracing::warn!("cannot place {name} on the timeline; leaving it");
                report.remaining += 1;
            }
        }
    }
    chunks.sort_by(|a, b| a.1.start.total_cmp(&b.1.start).then(a.1.index.cmp(&b.1.index)));

    let mut recovered: HashMap<Track, Vec<(ChunkSpan, Vec<Word>)>> = HashMap::new();
    let mut pending = chunks.into_iter();
    for (track, span, path) in pending.by_ref() {
        let audio = ChunkAudio { path: path.clone(), duration: span.end - span.start };
        let billed = transcriber.transcribe(&audio).await;
        spend.record(Some(id), SpendKind::Transcription, billed.usage.as_ref());
        match billed.result {
            Ok(words) => {
                if let Err(e) = fs::remove_file(&path) {
                    tracing::warn!("could not delete {}: {e}", path.display());
                }
                recovered.entry(track).or_default().push((span, words));
                report.recovered += 1;
            }
            Err(error) => {
                report.remaining += 1;
                let blocking = matches!(error, TranscribeError::ZdrViolation(_) | TranscribeError::Unauthorized(_));
                report.error = Some(error.to_string());
                if blocking {
                    break;
                }
            }
        }
    }
    report.remaining += pending.count();

    if report.recovered > 0 {
        let merged = store.transcript(&meeting).map(|existing| merge(existing, recovered, config));
        if let Err(e) = merged.and_then(|transcript| store.replace_transcript(&meeting, &transcript)) {
            report.error = Some(format!("could not save the Transcript: {e}"));
            return report;
        }
    }
    let status = if report.remaining == 0 { MeetingStatus::Complete } else { MeetingStatus::Incomplete };
    if let Err(e) = store.set_status(id, status, None) {
        report.error = Some(format!("could not update the Meeting: {e}"));
    }
    report
}

/// Adds the recovered Chunks' words to the existing Transcript: overlap within the recovered Chunks is cut as
/// during recording, words already in the Transcript (from the overlap with a neighbouring Chunk) are dropped,
/// and recovered You words that echo Others are dropped.
fn merge(existing: Vec<Segment>, mut recovered: HashMap<Track, Vec<(ChunkSpan, Vec<Word>)>>, config: &PipelineConfig) -> Vec<Segment> {
    let assembler_config =
        AssemblerConfig { overlap: config.overlap_secs, pause: config.pause_secs, max_segment: config.max_segment_secs };
    let mut words_of = |track: Track| -> Vec<Word> {
        let mut assembler = TrackAssembler::new(track, assembler_config);
        let mut words = Vec::new();
        for (span, chunk_words) in recovered.remove(&track).unwrap_or_default() {
            words.extend(assembler.accept_words(span, chunk_words));
        }
        words.extend(assembler.flush_words());
        let covered: Vec<(f64, f64)> = existing.iter().filter(|s| s.track == track).map(|s| (s.start, s.end)).collect();
        words.retain(|w| !covered.iter().any(|&(start, end)| (start..=end).contains(&w.midpoint())));
        words
    };
    let others = words_of(Track::Others);
    let you = words_of(Track::You);

    // Echo dedupe against every Others word, recovered or already in the Transcript (whose word times are
    // estimated by spreading each Segment's words evenly).
    let mut echo = EchoFilter::new(config.echo_window_secs, config.pause_secs);
    for batch in split_at_pauses(you, config.pause_secs) {
        echo.push_you(batch, 0.0);
    }
    let mut all_others = others.clone();
    all_others.extend(existing.iter().filter(|s| s.track == Track::Others).flat_map(spread_words));
    let you = echo.push_others(&all_others, f64::INFINITY);

    let mut transcript = existing;
    transcript.extend(segments(Track::Others, &others, &assembler_config));
    transcript.extend(you.iter().flat_map(|batch| segments(Track::You, batch, &assembler_config)));
    transcript.sort_by(|a, b| a.start.total_cmp(&b.start));
    transcript
}

fn split_at_pauses(words: Vec<Word>, pause: f64) -> Vec<Vec<Word>> {
    let mut batches: Vec<Vec<Word>> = Vec::new();
    for word in words {
        match batches.last_mut() {
            Some(batch) if word.start - batch.last().expect("non-empty").end < pause => batch.push(word),
            _ => batches.push(vec![word]),
        }
    }
    batches
}

fn spread_words(segment: &Segment) -> Vec<Word> {
    let tokens: Vec<&str> = segment.text.split_whitespace().collect();
    let step = (segment.end - segment.start) / tokens.len().max(1) as f64;
    tokens
        .iter()
        .enumerate()
        .map(|(i, t)| Word { text: (*t).to_owned(), start: segment.start + i as f64 * step, end: segment.start + (i + 1) as f64 * step })
        .collect()
}

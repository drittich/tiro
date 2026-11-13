use chrono::{DateTime, FixedOffset, Utc};
use tiro_core::spend::{meeting_spend, parse_ledger, spend_summary, MeetingSpend, SpendKind, SpendLedger, Usage};

/// Sums of USD amounts, to within a millionth of a cent.
fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-8, "{actual} != {expected}");
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().to_utc()
}

const LEDGER: &str = r#"{"at":"2026-09-30T23:30:00Z","meeting_id":"2026-09-30-1900-manual","kind":"transcription","model":"elevenlabs/scribe-v2","seconds":6.0,"cost":0.0002}
{"at":"2026-10-01T12:00:00Z","meeting_id":"2026-10-01-0800-manual","kind":"summary","model":"deepseek/deepseek-v4.1-flash","prompt_tokens":12000,"completion_tokens":3000,"cost":0.12}
{"at":"2026-10-01T12:01:00Z","meeting_id":"2026-10-01-0800-manual","kind":"tags","model":"deepseek/deepseek-v4.1-flash","cost":0.01}
{"at":"2026-10-22T09:00:00Z","kind":"rerank","model":"cohere/rerank-v4","cost":0.003,"estimated":true}
{"at":"2026-10-22T09:05:00Z","meeting_id":"2026-10-01-0800-ma"#;

#[test]
fn a_ledger_parses_to_its_entries_skipping_a_line_cut_short() {
    let entries = parse_ledger(LEDGER);

    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].at, at("2026-09-30T23:30:00Z"));
    assert_eq!(entries[0].meeting_id.as_deref(), Some("2026-09-30-1900-manual"));
    assert_eq!(entries[0].kind, SpendKind::Transcription);
    assert_eq!(entries[0].usage.model, "elevenlabs/scribe-v2");
    assert_eq!(entries[0].usage.seconds, Some(6.0));
    assert_eq!(entries[0].usage.cost, 0.0002);
    assert!(!entries[0].usage.estimated);
    assert_eq!(entries[1].usage.prompt_tokens, Some(12000));
    assert_eq!(entries[1].usage.completion_tokens, Some(3000));
    assert_eq!(entries[2].kind, SpendKind::Tags);
    // A kind this version doesn't know is still money spent.
    assert_eq!(entries[3].kind, SpendKind::Other);
    assert_eq!(entries[3].meeting_id, None);
    assert!(entries[3].usage.estimated);
}

#[test]
fn blank_and_garbled_lines_are_skipped() {
    let entries = parse_ledger(&format!("\n{{not json}}\n{{\"at\":\"x\"}}\n{}\n", LEDGER.lines().next().unwrap()));
    assert_eq!(entries.len(), 1);
}

#[test]
fn a_meetings_spend_is_its_entries_summed_by_kind_in_a_fixed_order() {
    let mut ledger = LEDGER.to_owned();
    ledger.push_str("\n{\"at\":\"2026-10-02T10:00:00Z\",\"meeting_id\":\"2026-10-01-0800-manual\",\"kind\":\"summary\",\"model\":\"m\",\"cost\":0.05}");
    ledger.push_str("\n{\"at\":\"2026-10-01T09:00:00Z\",\"meeting_id\":\"2026-10-01-0800-manual\",\"kind\":\"transcription\",\"model\":\"m\",\"cost\":0.0004}");

    let spend = meeting_spend(&parse_ledger(&ledger), "2026-10-01-0800-manual");

    assert_close(spend.total, 0.1804);
    let kinds: Vec<SpendKind> = spend.by_kind.iter().map(|k| k.kind).collect();
    assert_eq!(kinds, [SpendKind::Transcription, SpendKind::Summary, SpendKind::Tags]);
    assert_close(spend.by_kind[0].cost, 0.0004);
    assert_close(spend.by_kind[1].cost, 0.17);
    assert_close(spend.by_kind[2].cost, 0.01);
}

#[test]
fn a_meeting_with_no_entries_has_spent_nothing() {
    assert_eq!(meeting_spend(&parse_ledger(LEDGER), "2026-10-05-1000-manual"), MeetingSpend { total: 0.0, by_kind: vec![] });
}

#[test]
fn monthly_totals_are_calendar_months_in_local_time_newest_first() {
    let toronto = FixedOffset::west_opt(4 * 3600).unwrap();

    let summary = spend_summary(&parse_ledger(LEDGER), &toronto);

    let months: Vec<(i32, u32)> = summary.months.iter().map(|m| (m.year, m.month)).collect();
    assert_eq!(months, [(2026, 10), (2026, 9)]);
    assert_close(summary.months[0].total, 0.133);
    assert_close(summary.months[1].total, 0.0002);
    assert_close(summary.total, 0.1332);
}

#[test]
fn a_late_evening_in_utc_can_be_the_next_month_locally() {
    let berlin = FixedOffset::east_opt(2 * 3600).unwrap();

    let summary = spend_summary(&parse_ledger(LEDGER), &berlin);

    let months: Vec<(i32, u32)> = summary.months.iter().map(|m| (m.year, m.month)).collect();
    assert_eq!(months, [(2026, 10)]);
    assert_close(summary.total, 0.1332);
}

#[test]
fn an_empty_ledger_has_no_months_and_nothing_spent() {
    let summary = spend_summary(&[], &Utc);
    assert!(summary.months.is_empty());
    assert_eq!(summary.total, 0.0);
}

fn usage(cost: f64) -> Usage {
    Usage {
        model: "elevenlabs/scribe-v2".into(),
        cost,
        estimated: false,
        prompt_tokens: None,
        completion_tokens: None,
        seconds: Some(6.0),
        generation_id: Some("gen-stt-1".into()),
    }
}

#[test]
fn recorded_requests_are_appended_and_read_back_with_their_meeting_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SpendLedger::new(dir.path().join("spend.jsonl"));
    assert!(ledger.entries().is_empty(), "no ledger yet");

    let before = Utc::now();
    ledger.record(Some("2026-10-22-0900-manual"), SpendKind::Transcription, Some(&usage(0.0002)));
    ledger.record(None, SpendKind::Summary, None);
    ledger.record(None, SpendKind::Tags, Some(&usage(0.01)));

    let entries = SpendLedger::new(dir.path().join("spend.jsonl")).entries();
    assert_eq!(entries.len(), 2, "a request with no reported cost isn't recorded");
    assert_eq!(entries[0].meeting_id.as_deref(), Some("2026-10-22-0900-manual"));
    assert_eq!(entries[0].kind, SpendKind::Transcription);
    assert_eq!(entries[0].usage, usage(0.0002));
    assert!(entries[0].at >= before && entries[0].at <= Utc::now());
    assert_eq!(entries[1].meeting_id, None);
    assert_eq!(entries[1].kind, SpendKind::Tags);
}

#[test]
fn appending_keeps_whats_already_there_even_after_a_line_cut_short() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spend.jsonl");
    std::fs::write(&path, LEDGER).unwrap();

    SpendLedger::new(path.clone()).record(Some("m"), SpendKind::Summary, Some(&usage(0.5)));

    let entries = SpendLedger::new(path).entries();
    assert_eq!(entries.len(), 5, "the four earlier entries and the new one");
    assert_eq!(entries[4].meeting_id.as_deref(), Some("m"));
}

#[test]
fn a_ledger_that_cannot_be_written_does_not_fail_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SpendLedger::new(dir.path().join("missing-dir").join("spend.jsonl"));
    ledger.record(Some("m"), SpendKind::Summary, Some(&usage(0.5)));
}

#[test]
fn listeners_hear_each_recorded_entry() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SpendLedger::new(dir.path().join("spend.jsonl"));
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = heard.clone();
    ledger.on_record(move |entry| sink.lock().unwrap().push(entry.meeting_id.clone()));

    ledger.record(Some("m"), SpendKind::Summary, Some(&usage(0.5)));
    ledger.record(Some("m"), SpendKind::Tags, None);

    assert_eq!(*heard.lock().unwrap(), [Some("m".to_owned())]);
}

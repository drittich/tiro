//! The events the UI receives must match `PipelineEvent` in src/lib/api.ts.

use tiro_core::pipeline::PipelineEvent;
use tiro_core::transcript::Track;
use serde_json::json;

#[test]
fn pipeline_events_reach_the_ui_with_camel_case_fields() {
    let retrying =
        PipelineEvent::Retrying { meeting_id: "m".into(), track: Track::You, message: "offline".into(), delay_secs: 4 };
    assert_eq!(
        serde_json::to_value(retrying).unwrap(),
        json!({ "kind": "retrying", "meetingId": "m", "track": "you", "message": "offline", "delaySecs": 4 })
    );
    assert_eq!(
        serde_json::to_value(PipelineEvent::Finished { meeting_id: "m".into() }).unwrap(),
        json!({ "kind": "finished", "meetingId": "m" })
    );
}

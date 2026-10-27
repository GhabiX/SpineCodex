use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::CodexHarnessMetadata;
use crate::CompactedItem;
use crate::GuardianHistoryCheckpoint;
use crate::ResponseItemEnvelope;
use crate::RolloutItem;
use crate::RolloutLine;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

#[test]
fn legacy_spine_records_keep_their_wire_shape() -> Result<()> {
    // Artificial old-format records: no personal session data or credentials.
    for (kind, payload) in [
        (
            "spine_sampling_started",
            json!({"thread": "thread-1", "epoch": 3}),
        ),
        (
            "spine_transition",
            json!({"type": "sampling_shadow_v1", "record": {"digest": "abc"}}),
        ),
    ] {
        let legacy = json!({
            "timestamp": "2026-09-01T12:00:00.000Z",
            "ordinal": 7,
            "type": kind,
            "payload": {"version": 1, "payload": payload},
        });
        let decoded: RolloutLine = serde_json::from_value(legacy.clone())?;
        assert_eq!(serde_json::to_value(decoded)?, legacy);
    }
    Ok(())
}

#[test]
fn compaction_beside_spine_records_preserves_harness_metadata_and_review_history() -> Result<()> {
    let item = ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "client instruction".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let checkpoint = CompactedItem {
        message: "compacted".to_string(),
        replacement_history: Some(vec![ResponseItemEnvelope {
            item: item.clone(),
            metadata: Some(CodexHarnessMetadata {
                client_authored: true,
                fallback_token_limit_override: Some(2048),
            }),
        }]),
        guardian_history: Some(GuardianHistoryCheckpoint(vec![item])),
        mcp_resource_origins: None,
        window_number: Some(3),
        first_window_id: Some("window-1".to_string()),
        previous_window_id: Some("window-2".to_string()),
        window_id: Some("window-3".to_string()),
        compaction_response_id: Some("response-3".to_string()),
        latest_token_usage_record: None,
    };
    let items = vec![
        RolloutItem::SpineSamplingStarted(crate::SpineSamplingStartedItem {
            sdk_config: None,
            replay_seed: None,
            version: 1,
            payload: json!({"schema": "spine.sampling.started"}),
        }),
        RolloutItem::Compacted(checkpoint.clone()),
        RolloutItem::SpineTransition(crate::SpineTransitionItem {
            version: 1,
            payload: json!({"schema": "spine.sampling.commit"}),
        }),
    ];
    let encoded = serde_json::to_value(&items)?;
    let decoded: Vec<RolloutItem> = serde_json::from_value(encoded.clone())?;
    assert_eq!(serde_json::to_value(&decoded)?, encoded);
    let RolloutItem::Compacted(restored) = &decoded[1] else {
        panic!("compaction must retain its position between the Spine records");
    };
    assert_eq!(restored, &checkpoint);
    Ok(())
}

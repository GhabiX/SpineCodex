use super::*;
use codex_protocol::models::FunctionCallOutputContentItem;
use pretty_assertions::assert_eq;

fn produced_payload(tool: SpineToolResponse) -> FunctionCallOutputPayload {
    let output = tool.success();
    let success = output.success;
    FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(output.into_text()),
        success,
    }
}

#[test]
fn success_carriers_round_trip_through_persisted_payloads() {
    for tool in [
        SpineToolResponse::Open,
        SpineToolResponse::Close,
        SpineToolResponse::Next,
        SpineToolResponse::Trim,
    ] {
        let live = produced_payload(tool);
        assert_eq!(live.success, Some(true));
        assert_eq!(
            SpineToolResponse::outcome(&tool.qualified_name(), &live),
            ToolOutcome::Succeeded
        );

        let encoded = serde_json::to_string(&live).expect("serialize tool output");
        let restored: FunctionCallOutputPayload =
            serde_json::from_str(&encoded).expect("deserialize tool output");
        assert_eq!(restored.success, None);
        assert_eq!(
            SpineToolResponse::outcome(&tool.qualified_name(), &restored),
            ToolOutcome::Succeeded
        );
    }
}

#[test]
fn explicit_success_metadata_takes_precedence_over_carrier() {
    let mut payload = produced_payload(SpineToolResponse::Open);
    payload.success = Some(false);
    assert_eq!(
        SpineToolResponse::outcome("spine.open", &payload),
        ToolOutcome::Failed
    );

    payload.success = Some(true);
    payload.body = FunctionCallOutputBody::Text("not a carrier".to_string());
    assert_eq!(
        SpineToolResponse::outcome("spine.open", &payload),
        ToolOutcome::Succeeded
    );
}

#[test]
fn missing_success_metadata_requires_exact_registered_text_carrier() {
    let unknown_payloads = [
        (
            "spine.open",
            FunctionCallOutputBody::Text("Spine open accepted".to_string()),
        ),
        (
            "spine.unknown",
            FunctionCallOutputBody::Text("Spine unknown accepted.".to_string()),
        ),
        (
            "other.open",
            FunctionCallOutputBody::Text("Spine open accepted.".to_string()),
        ),
        (
            "spine.open",
            FunctionCallOutputBody::ContentItems(vec![FunctionCallOutputContentItem::InputText {
                text: "Spine open accepted.".to_string(),
            }]),
        ),
    ];

    for (name, body) in unknown_payloads {
        assert_eq!(
            SpineToolResponse::outcome(
                name,
                &FunctionCallOutputPayload {
                    body,
                    success: None,
                }
            ),
            ToolOutcome::Unknown
        );
    }
}

#[test]
fn persisted_open_carrier_snapshot_is_stable() {
    assert_eq!(
        produced_payload(SpineToolResponse::Open).body,
        FunctionCallOutputBody::Text("Spine open accepted.".to_string())
    );
}

use super::*;
use anyhow::Result;
use codex_protocol::protocol::TurnAbortReason;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn client_response_payload_serializes_without_an_intermediate_json_value() -> Result<()> {
    let payload = ClientResponsePayload::ThreadArchive(v2::ThreadArchiveResponse {});
    assert_eq!(serde_json::to_string(&payload)?, "{}");
    let Some(ClientResponse::ThreadArchive {
        request_id,
        response: _,
    }) = payload.into_client_response(RequestId::Integer(7))
    else {
        panic!("expected thread/archive client response");
    };
    assert_eq!(request_id, RequestId::Integer(7));
    Ok(())
}

#[test]
fn interrupt_conversation_payload_stays_jsonrpc_only() -> Result<()> {
    let payload = ClientResponsePayload::InterruptConversation(v1::InterruptConversationResponse {
        abort_reason: TurnAbortReason::Interrupted,
    });
    assert_eq!(
        serde_json::to_value(&payload)?,
        json!({
            "abortReason": "interrupted",
        })
    );
    assert!(
        payload
            .into_client_response(RequestId::Integer(8))
            .is_none()
    );
    Ok(())
}

#[test]
fn spine_feedback_request_preserves_thread_serialization_and_wire_shape() -> Result<()> {
    let request = ClientRequest::SpineFeedbackUpload {
        request_id: RequestId::Integer(9),
        params: v2::SpineFeedbackUploadParams {
            thread_id: "thread-1".to_string(),
            note: Some("note".to_string()),
            screenshots: Some(vec![v2::SpineFeedbackScreenshot {
                png_base64: "cG5n".to_string(),
            }]),
        },
    };
    assert_eq!(
        request.serialization_scope(),
        Some(ClientRequestSerializationScope::Thread {
            thread_id: "thread-1".to_string(),
        })
    );
    assert_eq!(
        serde_json::to_value(request)?,
        json!({
            "method": "feedback/spineUpload",
            "id": 9,
            "params": {
                "threadId": "thread-1",
                "note": "note",
                "screenshots": [{"pngBase64": "cG5n"}],
            },
        })
    );

    let params: v2::SpineFeedbackUploadParams = serde_json::from_value(json!({
        "threadId": "thread-1",
    }))?;
    assert_eq!(
        params,
        v2::SpineFeedbackUploadParams {
            thread_id: "thread-1".to_string(),
            note: None,
            screenshots: None,
        }
    );
    let params: v2::SpineFeedbackUploadParams = serde_json::from_value(json!({
        "threadId": "thread-1",
        "screenshots": null,
    }))?;
    assert_eq!(
        params,
        v2::SpineFeedbackUploadParams {
            thread_id: "thread-1".to_string(),
            note: None,
            screenshots: None,
        }
    );
    Ok(())
}

#[test]
fn spine_notification_methods_keep_stable_and_experimental_wire_shapes() -> Result<()> {
    let rolled_back = ServerNotification::ThreadRolledBack(v2::ThreadRolledBackNotification {
        thread_id: "thread-1".to_string(),
    });
    assert_eq!(
        serde_json::to_value(rolled_back)?,
        json!({
            "method": "thread/rolledBack",
            "params": {"threadId": "thread-1"},
        })
    );

    let tree = ServerNotification::SpineTreeUpdated(v2::SpineTreeUpdatedNotification {
        thread_id: "thread-1".to_string(),
        turn_id: "turn-1".to_string(),
        snapshot_seq: 7,
        active_node_id: "1.2".to_string(),
        nodes: Vec::new(),
        settled_spawn_call_ids: vec!["spawn-1".to_string()],
    });
    assert_eq!(
        serde_json::to_value(tree)?,
        json!({
            "method": "turn/spineTree/updated",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "snapshotSeq": 7,
                "activeNodeId": "1.2",
                "nodes": [],
                "settledSpawnCallIds": ["spawn-1"],
            },
        })
    );

    let progress =
        ServerNotification::SpineSpawnProgressUpdated(v2::SpineSpawnProgressUpdatedNotification {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            call_id: "spawn-1".to_string(),
            tasks: Vec::new(),
        });
    assert_eq!(
        serde_json::to_value(progress)?,
        json!({
            "method": "turn/spineSpawnProgress/updated",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "callId": "spawn-1",
                "tasks": [],
            },
        })
    );
    Ok(())
}

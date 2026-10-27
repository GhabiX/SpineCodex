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
fn client_response_jsonrpc_parts_preserve_payloads_and_request_ids() -> Result<()> {
    let payloads = [
        (
            ClientResponsePayload::GetAuthStatus(v1::GetAuthStatusResponse {
                auth_method: Some(AuthMode::Chatgpt),
                auth_token: None,
                requires_openai_auth: Some(true),
            }),
            json!({
                "authMethod": "chatgpt",
                "authToken": null,
                "requiresOpenaiAuth": true,
            }),
        ),
        (
            ClientResponsePayload::GetAccount(v2::GetAccountResponse {
                account: Some(v2::Account::ApiKey {}),
                requires_openai_auth: false,
                workspace_routing: None,
            }),
            json!({
                "account": { "type": "apiKey" },
                "requiresOpenaiAuth": false,
                "workspaceRouting": null,
            }),
        ),
    ];

    for (payload, expected_result) in payloads {
        for request_id in [RequestId::Integer(7), RequestId::String("request-7".into())] {
            let expected = (request_id.clone(), expected_result.clone());
            let response = payload
                .clone()
                .into_client_response(request_id.clone())
                .expect("request-backed payload has a typed response");

            assert_eq!(response.into_jsonrpc_parts()?, expected);
            assert_eq!(payload.to_jsonrpc_parts(request_id.clone())?, expected);
            assert_eq!(payload.clone().into_jsonrpc_parts(request_id)?, expected);
        }
    }
    Ok(())
}

#[test]
fn client_response_jsonrpc_parts_preserve_legacy_interrupt() -> Result<()> {
    let request_id = RequestId::String("interrupt-7".into());
    let payload = ClientResponsePayload::InterruptConversation(v1::InterruptConversationResponse {
        abort_reason: TurnAbortReason::Interrupted,
    });
    let expected = (request_id.clone(), json!({ "abortReason": "interrupted" }));

    assert!(
        payload
            .clone()
            .into_client_response(request_id.clone())
            .is_none()
    );
    assert_eq!(payload.to_jsonrpc_parts(request_id.clone())?, expected);
    assert_eq!(payload.into_jsonrpc_parts(request_id)?, expected);
    Ok(())
}

#[cfg(unix)]
#[test]
fn client_response_jsonrpc_parts_preserve_serialization_errors() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let request_id = RequestId::Integer(7);
    let response = v1::GetConversationSummaryResponse {
        summary: v1::ConversationSummary {
            conversation_id: codex_protocol::ThreadId::from_u128(/*value*/ 7),
            path: PathBuf::from(OsString::from_vec(vec![0xff])),
            preview: String::new(),
            timestamp: None,
            updated_at: None,
            model_provider: String::new(),
            cwd: PathBuf::new(),
            cli_version: String::new(),
            source: codex_protocol::protocol::SessionSource::Exec,
            git_info: None,
        },
    };
    let describe = |error: serde_json::Error| (error.classify(), error.to_string());
    let expected = describe(serde_json::to_value(&response).unwrap_err());
    let payload = ClientResponsePayload::GetConversationSummary(response.clone());
    let typed = ClientResponse::GetConversationSummary {
        request_id: request_id.clone(),
        response,
    };

    assert_eq!(describe(typed.into_jsonrpc_parts().unwrap_err()), expected);
    assert_eq!(
        describe(payload.to_jsonrpc_parts(request_id.clone()).unwrap_err()),
        expected
    );
    assert_eq!(
        describe(payload.into_jsonrpc_parts(request_id).unwrap_err()),
        expected
    );
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
    let reverted = ServerNotification::ThreadReverted(v2::ThreadRevertedNotification {
        thread_id: "thread-1".to_string(),
    });
    assert_eq!(
        serde_json::to_value(reverted)?,
        json!({
            "method": "thread/reverted",
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
        settled_spawn_thread_ids: vec!["child-1".to_string()],
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
                "settledSpawnThreadIds": ["child-1"],
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

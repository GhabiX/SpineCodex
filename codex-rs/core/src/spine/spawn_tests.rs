use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_spine_core::SPINE_SPAWN_RESULT_SCHEMA;
use codex_spine_core::SpawnOutcome;
use codex_spine_core::SpawnResult;
use pretty_assertions::assert_eq;

#[test]
fn task_arguments_require_two_exact_non_empty_tasks() {
    let tasks = parse_tasks(
        r#"{"tasks":[{"summary":"one","prompt":"first"},{"summary":" two ","prompt":" second "}]}"#,
    )
    .unwrap();
    assert_eq!(
        tasks,
        vec![
            codex_spine_core::SpawnTask {
                summary: "one".to_string(),
                prompt: "first".to_string(),
            },
            codex_spine_core::SpawnTask {
                summary: " two ".to_string(),
                prompt: " second ".to_string(),
            },
        ]
    );

    for arguments in [
        r#"{"tasks":[]}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first"}]}"#,
        r#"{"tasks":[{"summary":" ","prompt":"first"},{"summary":"two","prompt":"second"}]}"#,
        r#"{"tasks":[{"summary":"one","prompt":""},{"summary":"two","prompt":"second"}]}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first","extra":true},{"summary":"two","prompt":"second"}]}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first"},{"summary":"two","prompt":"second"}],"extra":true}"#,
    ] {
        assert!(parse_tasks(arguments).is_err(), "accepted {arguments}");
    }
}

fn call(call_id: &str, namespace: Option<&str>, name: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: namespace.map(str::to_string),
        arguments: r#"{"tasks":[]}"#.to_string(),
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    })
}

fn message(role: &str, text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

fn output(call_id: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("done".to_string()),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    })
}

fn reasoning() -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Reasoning {
        id: None,
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "thinking".to_string(),
        }],
        content: None,
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

#[test]
fn exact_receipt_codec_preserves_all_semantic_fields() {
    let receipt = SpawnReceipt {
        schema: SPINE_SPAWN_RESULT_SCHEMA.to_string(),
        results: vec![SpawnResult {
            ordinal: 0,
            outcome: SpawnOutcome::Errored,
            memory_body: "truthful memory".to_string(),
            diagnostic: Some("child error".to_string()),
            execution_ref: Some("child-ref".to_string()),
        }],
    };

    assert_eq!(
        decode_receipt(&encode_receipt(&receipt).unwrap()).unwrap(),
        receipt
    );
    assert!(
        decode_receipt(r#"{"schema":"spine.spawn.result.v1","results":[],"extra":true}"#).is_err()
    );
}

#[test]
fn coordinator_helpers_keep_safe_names_and_truthful_terminal_results() {
    assert_eq!(transaction_task_name("Call-ID.42", 3), "spawn_callid42_3");

    let thread_id = codex_protocol::ThreadId::new();
    let completed = result_from_status(
        0,
        thread_id,
        AgentStatus::Completed(Some("final memory".to_string())),
    );
    assert_eq!(completed.outcome, SpawnOutcome::Completed);
    assert_eq!(completed.memory_body, "final memory");
    assert_eq!(completed.diagnostic, None);

    let missing = result_from_status(1, thread_id, AgentStatus::Completed(None));
    assert_eq!(missing.outcome, SpawnOutcome::Errored);
    assert!(missing.diagnostic.is_some());
    assert!(!missing.memory_body.trim().is_empty());

    assert!(is_spawn_terminal(&AgentStatus::Interrupted));
    let interrupted = result_from_status(2, thread_id, AgentStatus::Interrupted);
    assert_eq!(interrupted.outcome, SpawnOutcome::Aborted);
}

#[test]
fn call_only_preflight_accepts_flat_and_namespaced_spawn_calls() {
    assert!(validate_call_only(&[call("spawn", None, "spine.spawn")], "spawn").is_ok());
    assert!(validate_call_only(&[call("spawn", Some("spine"), "spawn")], "spawn").is_ok());
}

#[test]
fn call_only_preflight_uses_native_response_group_boundaries() {
    let rollout = [
        message("user", "first turn"),
        call("previous", None, "shell"),
        output("previous"),
        message("user", "spawn now"),
        call("spawn", Some("spine"), "spawn"),
        output("later"),
    ];
    assert!(validate_call_only(&rollout, "spawn").is_ok());
}

#[test]
fn call_only_preflight_rejects_text_reasoning_and_sibling_calls() {
    for rollout in [
        vec![
            message("assistant", "extra"),
            call("spawn", None, "spine.spawn"),
        ],
        vec![reasoning(), call("spawn", None, "spine.spawn")],
        vec![
            call("spawn", None, "spine.spawn"),
            call("shell", None, "shell"),
        ],
    ] {
        assert!(validate_call_only(&rollout, "spawn").is_err());
    }
}

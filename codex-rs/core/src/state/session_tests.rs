use super::*;
use crate::session::tests::make_session_configuration_for_tests;
use crate::state::AutoCompactWindowSnapshot;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::CreditsSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpendControlLimitSnapshot;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use pretty_assertions::assert_eq;

fn response_message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn response_text(item: &ResponseItem) -> &str {
    let ResponseItem::Message { content, .. } = item else {
        panic!("expected message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected text");
    };
    text
}

fn spawn_call_and_output() -> (ResponseItem, ResponseItem) {
    let arguments = serde_json::json!({
        "tasks": [
            {"summary": "first", "prompt": "inspect first"},
            {"summary": "second", "prompt": "inspect second"}
        ]
    })
    .to_string();
    let receipt = serde_json::json!({
        "schema": codex_spine_core::SPINE_SPAWN_RESULT_SCHEMA,
        "results": [
            {"ordinal": 0, "outcome": "completed", "memory_body": "first memory"},
            {"ordinal": 1, "outcome": "completed", "memory_body": "second memory"}
        ]
    })
    .to_string();
    (
        ResponseItem::FunctionCall {
            id: None,
            name: "spawn".to_string(),
            namespace: Some("spine".to_string()),
            arguments,
            call_id: "spawn".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "spawn".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text(receipt),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        },
    )
}

fn trim_candidate_text(fragment: &str) -> String {
    assert!(!fragment.is_empty());
    let minimum_bytes = codex_spine_core::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES + 1;
    fragment.repeat(minimum_bytes.div_ceil(fragment.len()))
}

fn token_count(input_tokens: i64) -> RolloutItem {
    RolloutItem::EventMsg(codex_protocol::protocol::EventMsg::TokenCount(
        TokenCountEvent {
            info: Some(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage {
                    input_tokens,
                    total_tokens: input_tokens,
                    ..TokenUsage::default()
                },
                model_context_window: Some(200_000),
            }),
            rate_limits: None,
        },
    ))
}

#[tokio::test]
async fn spine_feature_off_clones_native_history_unchanged() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    session_configuration.disable_spine_jit_for_test();
    let mut state = SessionState::new(session_configuration);
    let message = response_message("user", "request");
    state.record_items(std::iter::once(&message), TruncationPolicy::Tokens(10_000));
    state.append_spine_rollout_items(&[RolloutItem::ResponseItem(message.clone())]);

    assert_eq!(state.clone_history().raw_items(), &[message]);
}

#[tokio::test]
async fn spine_jit_is_enabled_in_default_session_state() {
    let session_configuration = make_session_configuration_for_tests().await;
    assert!(session_configuration.spine_jit_enabled());
    assert!(
        SessionState::new(session_configuration)
            .spine_tree_update()
            .is_some()
    );
}

#[tokio::test]
async fn spine_feature_on_projects_live_native_rollout_at_clone_boundary() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    session_configuration.enable_spine_jit_for_test();
    let mut state = SessionState::new(session_configuration);
    let call = ResponseItem::FunctionCall {
        id: None,
        name: "spine.open".to_string(),
        namespace: None,
        arguments: r#"{"summary":"task"}"#.to_string(),
        call_id: "open".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let output = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "open".to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("Spine open accepted.".to_string()),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    };
    state.record_items([&call, &output], TruncationPolicy::Tokens(10_000));
    state.append_spine_rollout_items(&[
        RolloutItem::ResponseItem(call),
        RolloutItem::ResponseItem(output),
    ]);

    let projected = state.clone_history();
    assert_eq!(projected.raw_items().len(), 3);
    assert!(response_text(&projected.raw_items()[0]).starts_with("<spine_node"));
}

#[tokio::test]
async fn spine_projection_reuses_host_truncated_tool_output() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    session_configuration.enable_spine_jit_for_test();
    let mut state = SessionState::new(session_configuration);
    let call = ResponseItem::FunctionCall {
        id: None,
        name: "shell".to_string(),
        namespace: None,
        arguments: r#"{"cmd":"large-output"}"#.to_string(),
        call_id: "large-output".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let output = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "large-output".to_string(),
        output: FunctionCallOutputPayload::from_text("x".repeat(50_000)),
        internal_chat_message_metadata_passthrough: None,
    };
    state.record_items([&call, &output], TruncationPolicy::Tokens(50));
    let native_output = state.history.raw_items()[1].clone();
    state.append_spine_rollout_items(&[
        RolloutItem::ResponseItem(call),
        RolloutItem::ResponseItem(output),
    ]);

    let projected = state.clone_history();
    assert_eq!(projected.raw_items()[1], native_output);
    let ResponseItem::FunctionCallOutput { output, .. } = &projected.raw_items()[1] else {
        panic!("expected function output");
    };
    assert!(output.body.to_text().unwrap().len() < 1_000);
}

#[tokio::test]
async fn spawn_context_install_is_atomic_and_independently_feature_gated() {
    let mut enabled = make_session_configuration_for_tests().await;
    enabled.enable_spine_jit_for_test();
    enabled.enable_spine_spawn_for_test();
    let mut state = SessionState::new(enabled);
    let (call, output) = spawn_call_and_output();

    state.record_items([&call], TruncationPolicy::Tokens(10_000));
    state.append_spine_rollout_items(&[RolloutItem::ResponseItem(call.clone())]);
    assert_eq!(state.clone_history().raw_items(), &[call.clone()]);
    assert_eq!(
        state.spine_tree_update().expect("tree enabled").nodes.len(),
        1
    );

    state.record_items([&output], TruncationPolicy::Tokens(10_000));
    state.append_spine_rollout_items(&[RolloutItem::ResponseItem(output.clone())]);
    let projected = state.clone_history();
    assert_eq!(projected.raw_items().len(), 4);
    assert!(response_text(&projected.raw_items()[0]).contains("spine_spawn_evidence"));
    assert!(response_text(&projected.raw_items()[1]).contains("first memory"));
    assert_eq!(
        state.spine_tree_update().expect("tree enabled").nodes.len(),
        3
    );

    let mut disabled = make_session_configuration_for_tests().await;
    disabled.enable_spine_jit_for_test();
    let mut disabled_state = SessionState::new(disabled);
    disabled_state.record_items([&call, &output], TruncationPolicy::Tokens(10_000));
    disabled_state.append_spine_rollout_items(&[
        RolloutItem::ResponseItem(call.clone()),
        RolloutItem::ResponseItem(output.clone()),
    ]);
    assert_eq!(
        disabled_state.clone_history().raw_items(),
        &[call.clone(), output.clone()]
    );
    assert_eq!(
        disabled_state
            .spine_tree_update()
            .expect("Spine JIT remains enabled")
            .nodes
            .len(),
        1
    );
    assert!(
        response_text(
            &disabled_state
                .spine_status_prompt_overlay(None)
                .expect("status overlay enabled")
        )
        .contains("cursor=\"1\"")
    );
}

#[tokio::test]
async fn spine_tree_snapshot_is_derived_across_compact_and_rollout_replacement() {
    let mut disabled = make_session_configuration_for_tests().await;
    disabled.disable_spine_jit_for_test();
    assert!(SessionState::new(disabled).spine_tree_update().is_none());

    let mut enabled = make_session_configuration_for_tests().await;
    enabled.enable_spine_jit_for_test();
    let mut state = SessionState::new(enabled);
    let initial = state
        .spine_tree_update()
        .expect("Spine tree snapshot should be enabled");
    assert_eq!(initial.active_node_id, "1");
    assert_eq!(initial.nodes.len(), 1);

    let opened_rollout = vec![
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            id: None,
            name: "spine.open".to_string(),
            namespace: None,
            arguments: r#"{"summary":"task"}"#.to_string(),
            call_id: "open".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }),
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "open".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("Spine open accepted.".to_string()),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        }),
    ];
    state.append_spine_rollout_items(&opened_rollout);
    let opened = state
        .spine_tree_update()
        .expect("opened snapshot should be available");
    assert_eq!(opened.active_node_id, "1.1");
    assert_eq!(opened.nodes.len(), 2);
    assert_eq!(
        opened.nodes[0].status,
        codex_protocol::spine_tree::SpineTreeNodeStatus::Opened
    );
    assert_eq!(
        opened.nodes[1].status,
        codex_protocol::spine_tree::SpineTreeNodeStatus::Live
    );
    assert_eq!(opened.nodes[1].summary.as_deref(), Some("task"));

    state.append_spine_rollout_items(&[RolloutItem::Compacted(CompactedItem {
        message: "native compact memory".to_string(),
        replacement_history: Some(vec![response_message("user", "compacted context")]),
        window_number: None,
        first_window_id: None,
        previous_window_id: None,
        window_id: None,
    })]);
    let compacted = state
        .spine_tree_update()
        .expect("compacted snapshot should be available");
    assert_eq!(compacted.active_node_id, "2");
    assert_eq!(
        compacted.nodes[0].status,
        codex_protocol::spine_tree::SpineTreeNodeStatus::Compacted
    );
    assert_eq!(
        compacted.nodes.last().map(|node| node.status),
        Some(codex_protocol::spine_tree::SpineTreeNodeStatus::Live)
    );

    state.replace_spine_rollout(&opened_rollout);
    assert_eq!(
        state
            .spine_tree_update()
            .expect("replayed snapshot should be available"),
        opened
    );
    state.replace_spine_rollout(&[]);
    assert_eq!(
        state
            .spine_tree_update()
            .expect("rolled-back snapshot should be available"),
        initial
    );
}

#[tokio::test]
async fn spine_tree_pressure_rederives_for_resume_and_rollback_prefixes() {
    let mut enabled = make_session_configuration_for_tests().await;
    enabled.enable_spine_jit_for_test();
    let mut state = SessionState::new(enabled);
    let open_prefix = vec![
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            id: None,
            name: "spine.open".to_string(),
            namespace: None,
            arguments: r#"{"summary":"task"}"#.to_string(),
            call_id: "open".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }),
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "open".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("Spine open accepted.".to_string()),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        }),
        token_count(10_000),
    ];
    let mut full = open_prefix.clone();
    full.push(RolloutItem::ResponseItem(response_message(
        "user", "detail",
    )));
    full.push(token_count(42_000));

    state.replace_spine_rollout(&full);
    let resumed = state
        .spine_tree_update()
        .expect("resumed pressure snapshot");
    let active = resumed
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1")
        .expect("active node");
    assert_eq!(
        active.context_pressure,
        Some(
            codex_protocol::spine_tree::SpineNodeContextPressureSnapshot {
                open_input_tokens: Some(10_000),
                current_input_tokens: Some(42_000),
                context_tokens: Some(32_000),
                problem: None,
            }
        )
    );

    let mut rolled_back = full;
    rolled_back.push(RolloutItem::EventMsg(
        codex_protocol::protocol::EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        }),
    ));
    state.replace_spine_rollout(&rolled_back);
    let rollback = state
        .spine_tree_update()
        .expect("rollback pressure snapshot");
    assert_eq!(
        rollback
            .nodes
            .iter()
            .find(|node| node.node_id == "1.1")
            .and_then(|node| node.context_pressure.as_ref())
            .and_then(|pressure| pressure.context_tokens),
        Some(0)
    );

    state.replace_spine_rollout(&open_prefix);
    assert_eq!(
        state
            .spine_tree_update()
            .expect("fork pressure snapshot")
            .nodes
            .iter()
            .find(|node| node.node_id == "1.1")
            .and_then(|node| node.context_pressure.as_ref())
            .and_then(|pressure| pressure.context_tokens),
        Some(0)
    );
}

#[tokio::test]
async fn spine_tree_snapshot_uses_the_closed_nodes_final_summary_slot() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    session_configuration.enable_spine_jit_for_test();
    let mut state = SessionState::new(session_configuration);
    state.append_spine_rollout_items(&[
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            id: None,
            name: "spine.open".to_string(),
            namespace: None,
            arguments: r#"{"summary":"task"}"#.to_string(),
            call_id: "open".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }),
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "open".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("Spine open accepted.".to_string()),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        }),
        RolloutItem::ResponseItem(response_message("user", "detail")),
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            id: None,
            name: "spine.close".to_string(),
            namespace: None,
            arguments: r#"{"memory":"done"}"#.to_string(),
            call_id: "close".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }),
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "close".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("Spine close accepted.".to_string()),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        }),
    ]);

    let snapshot = state
        .spine_tree_update()
        .expect("closed snapshot should be available");
    let task = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1")
        .expect("closed task should be present");
    assert_eq!(task.memory_summary.as_deref(), Some("done"));
}

#[tokio::test]
async fn spine_control_validation_uses_the_pre_group_rollout_projection() {
    let mut disabled = make_session_configuration_for_tests().await;
    disabled.disable_spine_jit_for_test();
    let disabled_state = SessionState::new(disabled);
    assert!(
        disabled_state
            .validate_spine_control(crate::spine::SpineControlKind::Open)
            .is_err()
    );

    let mut enabled = make_session_configuration_for_tests().await;
    enabled.enable_spine_jit_for_test();
    let mut state = SessionState::new(enabled);
    assert!(
        state
            .validate_spine_control(crate::spine::SpineControlKind::Open)
            .is_ok()
    );
    assert!(
        state
            .validate_spine_control(crate::spine::SpineControlKind::Close)
            .is_err()
    );
    assert!(
        state
            .validate_spine_control(crate::spine::SpineControlKind::Next)
            .is_err()
    );

    state.append_spine_rollout_items(&[
        RolloutItem::ResponseItem(ResponseItem::FunctionCall {
            id: None,
            name: "spine.open".to_string(),
            namespace: None,
            arguments: r#"{"summary":"task"}"#.to_string(),
            call_id: "open".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }),
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
            id: None,
            call_id: "open".to_string(),
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("Spine open accepted.".to_string()),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        }),
    ]);
    assert!(
        state
            .validate_spine_control(crate::spine::SpineControlKind::Close)
            .is_ok()
    );
    assert!(
        state
            .validate_spine_control(crate::spine::SpineControlKind::Next)
            .is_ok()
    );
}

#[tokio::test]
async fn spine_trim_only_projects_native_history_without_tree_messages() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    session_configuration.disable_spine_jit_for_test();
    session_configuration.enable_spine_trim_for_test();
    let mut state = SessionState::new(session_configuration);
    let call = ResponseItem::FunctionCall {
        id: None,
        name: "shell".to_string(),
        namespace: None,
        arguments: r#"{"cmd":"cat"}"#.to_string(),
        call_id: "shell".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let output = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "shell".to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(trim_candidate_text("x")),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    };
    state.record_items([&call, &output], TruncationPolicy::Tokens(10_000));
    state.append_spine_rollout_items(&[
        RolloutItem::ResponseItem(call),
        RolloutItem::ResponseItem(output),
    ]);

    let projected = state.clone_history();
    assert_eq!(projected.raw_items().len(), 2);
    let ResponseItem::FunctionCallOutput { output, .. } = &projected.raw_items()[1] else {
        panic!("expected native tool output");
    };
    assert!(
        output
            .body
            .to_text()
            .unwrap()
            .starts_with("[TRIM_ID: trim_1]")
    );
}

#[tokio::test]
async fn spine_trim_validation_uses_the_current_rollout_window() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    session_configuration.disable_spine_jit_for_test();
    session_configuration.enable_spine_trim_for_test();
    let mut state = SessionState::new(session_configuration);
    let call = ResponseItem::FunctionCall {
        id: None,
        name: "shell".to_string(),
        namespace: None,
        arguments: r#"{"cmd":"cat"}"#.to_string(),
        call_id: "shell".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let output = ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "shell".to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(trim_candidate_text("x")),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    };
    state.append_spine_rollout_items(&[
        RolloutItem::ResponseItem(call),
        RolloutItem::ResponseItem(output),
    ]);

    let valid =
        codex_spine_core::TrimRequest::parse(r#"{"TRIM_ID":"trim_1","op":"snip"}"#).unwrap();
    assert!(state.validate_spine_trim(&valid).is_ok());
    let missed =
        codex_spine_core::TrimRequest::parse(r#"{"TRIM_ID":"trim_99","op":"snip"}"#).unwrap();
    assert!(
        state
            .validate_spine_trim(&missed)
            .unwrap_err()
            .contains("do not retry")
    );
}

#[tokio::test]
// Verifies connector merging deduplicates repeated IDs.
async fn merge_connector_selection_deduplicates_entries() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);
    let merged = state.merge_connector_selection([
        "calendar".to_string(),
        "calendar".to_string(),
        "drive".to_string(),
    ]);

    assert_eq!(
        merged,
        HashSet::from(["calendar".to_string(), "drive".to_string()])
    );
}

#[tokio::test]
// Verifies clearing connector selection removes all saved IDs.
async fn clear_connector_selection_removes_entries() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);
    state.merge_connector_selection(["calendar".to_string()]);

    state.clear_connector_selection();

    assert_eq!(state.get_connector_selection(), HashSet::new());
}

#[tokio::test]
async fn set_rate_limits_defaults_limit_id_to_codex_when_missing() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 12.0,
            window_minutes: Some(60),
            resets_at: Some(100),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    });

    assert_eq!(
        state
            .latest_rate_limits
            .as_ref()
            .and_then(|v| v.limit_id.clone()),
        Some("codex".to_string())
    );
}

#[tokio::test]
async fn replace_history_clears_auto_compact_window_prefill() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_auto_compact_window_estimated_prefill(/*tokens*/ 100);
    state.replace_history(Vec::new(), /*reference_context_item*/ None);

    assert_eq!(
        state.auto_compact_window_snapshot(),
        AutoCompactWindowSnapshot {
            prefill_input_tokens: None,
        }
    );
}

#[tokio::test]
async fn set_rate_limits_defaults_to_codex_when_limit_id_missing_after_other_bucket() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(RateLimitSnapshot {
        limit_id: Some("codex_other".to_string()),
        limit_name: Some("codex_other".to_string()),
        primary: Some(RateLimitWindow {
            used_percent: 20.0,
            window_minutes: Some(60),
            resets_at: Some(200),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    });
    state.set_rate_limits(RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 30.0,
            window_minutes: Some(60),
            resets_at: Some(300),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    });

    assert_eq!(
        state
            .latest_rate_limits
            .as_ref()
            .and_then(|v| v.limit_id.clone()),
        Some("codex".to_string())
    );
}

#[tokio::test]
async fn set_rate_limits_carries_account_metadata_from_codex_to_codex_other() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: Some("codex".to_string()),
        primary: Some(RateLimitWindow {
            used_percent: 10.0,
            window_minutes: Some(60),
            resets_at: Some(100),
        }),
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("50".to_string()),
        }),
        individual_limit: Some(SpendControlLimitSnapshot {
            limit: "25000".to_string(),
            used: "8000".to_string(),
            remaining_percent: 68,
            resets_at: 300,
        }),
        plan_type: Some(codex_protocol::account::PlanType::Plus),
        rate_limit_reached_type: None,
    });

    state.set_rate_limits(RateLimitSnapshot {
        limit_id: Some("codex_other".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 30.0,
            window_minutes: Some(120),
            resets_at: Some(200),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    });

    assert_eq!(
        state.latest_rate_limits,
        Some(RateLimitSnapshot {
            limit_id: Some("codex_other".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 30.0,
                window_minutes: Some(120),
                resets_at: Some(200),
            }),
            secondary: None,
            credits: Some(CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("50".to_string()),
            }),
            individual_limit: Some(SpendControlLimitSnapshot {
                limit: "25000".to_string(),
                used: "8000".to_string(),
                remaining_percent: 68,
                resets_at: 300,
            }),
            plan_type: Some(codex_protocol::account::PlanType::Plus),
            rate_limit_reached_type: None,
        })
    );
}

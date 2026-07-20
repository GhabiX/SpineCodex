use super::*;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::WorldStateItem;
use pretty_assertions::assert_eq;

fn message(role: &str, text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

fn call(call_id: &str, name: &str, arguments: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    })
}

fn namespaced_call(call_id: &str, namespace: &str, name: &str, arguments: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(namespace.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    })
}

fn output(call_id: &str, success: Option<bool>, text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.to_string()),
            success,
        },
        internal_chat_message_metadata_passthrough: None,
    })
}

fn spine_success_output(call_id: &str, tool: tool_response::SpineToolResponse) -> RolloutItem {
    let output = tool.success();
    let success = output.success;
    self::output(call_id, success, &output.into_text())
}

fn spawn_arguments() -> String {
    serde_json::json!({
        "tasks": [
            {"summary": "first", "prompt": "inspect first"},
            {"summary": "second", "prompt": "inspect second"}
        ]
    })
    .to_string()
}

fn spawn_receipt() -> String {
    serde_json::json!({
        "schema": codex_spine_core::SPINE_SPAWN_RESULT_SCHEMA,
        "results": [
            {
                "ordinal": 0,
                "outcome": "completed",
                "memory_body": "first memory",
                "execution_ref": "thread-first"
            },
            {
                "ordinal": 1,
                "outcome": "errored",
                "memory_body": "second error memory",
                "diagnostic": "child failed",
                "execution_ref": "thread-second"
            }
        ]
    })
    .to_string()
}

fn reasoning() -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Reasoning {
        id: None,
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "internal reasoning".to_string(),
        }],
        content: None,
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

fn response_items(rollout: &[RolloutItem]) -> Vec<ResponseItem> {
    rollout
        .iter()
        .filter_map(|item| match item {
            RolloutItem::ResponseItem(item) => Some(item.clone()),
            _ => None,
        })
        .collect()
}

fn trim_candidate_text(fragment: &str) -> String {
    assert!(!fragment.is_empty());
    let minimum_bytes = codex_spine_core::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES + 1;
    fragment.repeat(minimum_bytes.div_ceil(fragment.len()))
}

fn text(item: &ResponseItem) -> &str {
    let ResponseItem::Message { content, .. } = item else {
        panic!("expected message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected input text");
    };
    text
}

fn output_text(item: &ResponseItem) -> String {
    match item {
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => output.body.to_text().unwrap(),
        _ => panic!("expected tool output"),
    }
}

fn token_count(input_tokens: i64) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::TokenCount(TokenCountEvent {
        info: Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                input_tokens,
                total_tokens: input_tokens,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                input_tokens,
                total_tokens: input_tokens,
                ..TokenUsage::default()
            },
            model_context_window: Some(200_000),
        }),
        rate_limits: None,
    }))
}

#[test]
fn spine_status_matches_spine_dev_fields_and_context_accounting() {
    let rollout = vec![
        message("user", "request"),
        call(
            "open",
            "spine.open",
            r#"{"summary":"child \"scope\" <leaf> & focus"}"#,
        ),
        output("open", Some(true), "Spine open accepted."),
        token_count(10_000),
        message("user", "detail"),
        token_count(42_000),
    ];
    let overlay = status::prompt_overlay(&rollout, Some(100_000), true);

    assert_eq!(
        text(&overlay),
        r#"<spine_status cursor="1.1" summary="child &quot;scope&quot; &lt;leaf&gt; &amp; focus" parent="1" parent_summary="root" cursor_context="32.0K" context_left="100K" />"#
    );
}

#[test]
fn node_context_pressure_is_a_pure_rollout_prefix_projection() {
    let mut rollout = vec![
        message("user", "request"),
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("open", Some(true), "Spine open accepted."),
        token_count(10_000),
        message("user", "detail"),
        token_count(42_000),
    ];

    let full_projection = derive_from_rollout(&rollout).spine;
    let full = pressure::project(&rollout, &full_projection);
    let full_active = full
        .iter()
        .find(|(node_id, _)| node_id.to_string() == "1.1")
        .map(|(_, pressure)| pressure)
        .expect("active node pressure");
    assert_eq!(
        full_active,
        &pressure::NodeContextPressure {
            open_input_tokens: Some(10_000),
            current_input_tokens: Some(42_000),
            context_tokens: Some(32_000),
            problem: None,
        }
    );

    let resumed_projection = derive_from_rollout(&rollout).spine;
    assert_eq!(pressure::project(&rollout, &resumed_projection), full);

    let fork = &rollout[..4];
    let fork_projection = derive_from_rollout(fork).spine;
    let fork_pressure = pressure::project(fork, &fork_projection);
    assert_eq!(
        fork_pressure
            .iter()
            .find(|(node_id, _)| node_id.to_string() == "1.1")
            .and_then(|(_, pressure)| pressure.context_tokens),
        Some(0)
    );

    rollout.push(RolloutItem::EventMsg(EventMsg::ThreadRolledBack(
        ThreadRolledBackEvent { num_turns: 1 },
    )));
    let rollback_projection = derive_from_rollout(&rollout).spine;
    let rollback_pressure = pressure::project(&rollout, &rollback_projection);
    assert_eq!(
        rollback_pressure
            .iter()
            .find(|(node_id, _)| node_id.to_string() == "1.1")
            .and_then(|(_, pressure)| pressure.context_tokens),
        Some(0)
    );
}

fn long_tool_rollout() -> Vec<RolloutItem> {
    vec![
        call("shell", "shell", r#"{"cmd":"cat"}"#),
        output("shell", Some(true), &trim_candidate_text("0123456789\n")),
    ]
}

#[test]
fn adapter_projects_open_and_close_from_native_function_carriers() {
    let rollout = vec![
        message("user", "request"),
        namespaced_call("open", "spine", "open", r#"{"summary":"task"}"#),
        output("open", Some(true), "Spine open accepted."),
        message("user", "detail"),
        call("close", "spine.close", r#"{"memory":"done"}"#),
        output("close", Some(true), "Spine close accepted."),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.context.len(), 5);
    assert_eq!(text(&projection.context[0]), "[U1]\nrequest");
    assert_eq!(text(&projection.context[1]), "[U2]\ndetail");
    assert_eq!(
        text(&projection.context[2]),
        "<spine_memory node_id=\"1.1\">\ndone\n</spine_memory>"
    );
    assert!(matches!(
        projection.context[3],
        ResponseItem::FunctionCall { .. }
    ));
    assert!(matches!(
        projection.context[4],
        ResponseItem::FunctionCallOutput { .. }
    ));
}

#[test]
fn adapter_flattens_nested_memory_slots_in_source_order() {
    let rollout = vec![
        call("open-parent", "spine.open", r#"{"summary":"parent"}"#),
        output("open-parent", Some(true), "ok"),
        message("user", "before"),
        call("open-child", "spine.open", r#"{"summary":"child"}"#),
        output("open-child", Some(true), "ok"),
        message("user", "inside"),
        call("close-child", "spine.close", r#"{"memory":"child done"}"#),
        output("close-child", Some(true), "ok"),
        message("user", "after"),
        call("close-parent", "spine.close", r#"{"memory":"parent done"}"#),
        output("close-parent", Some(true), "ok"),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.context.len(), 7);
    assert_eq!(text(&projection.context[0]), "[U1]\nbefore");
    assert_eq!(text(&projection.context[1]), "[U2]\ninside");
    assert_eq!(
        text(&projection.context[2]),
        "<spine_memory node_id=\"1.1.1\">\nchild done\n</spine_memory>"
    );
    assert_eq!(text(&projection.context[3]), "[U3]\nafter");
    assert_eq!(
        text(&projection.context[4]),
        "<spine_memory node_id=\"1.1\">\nparent done\n</spine_memory>"
    );
    assert!(matches!(
        projection.context[5],
        ResponseItem::FunctionCall { .. }
    ));
    assert!(matches!(
        projection.context[6],
        ResponseItem::FunctionCallOutput { .. }
    ));
}

#[test]
fn adapter_replays_persisted_spine_success_carriers_without_success_metadata() {
    let rollout = vec![
        message("user", "request"),
        call("open-1", "spine.open", r#"{"summary":"first"}"#),
        spine_success_output("open-1", tool_response::SpineToolResponse::Open),
        message("user", "detail"),
        call("open-2", "spine.open", r#"{"summary":"second"}"#),
        spine_success_output("open-2", tool_response::SpineToolResponse::Open),
    ];

    let persisted = serde_json::to_string(&rollout).expect("serialize rollout");
    let restored: Vec<RolloutItem> = serde_json::from_str(&persisted).expect("deserialize rollout");
    for index in [2, 5] {
        let RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { output, .. }) =
            &restored[index]
        else {
            panic!("expected restored function output at index {index}");
        };
        assert_eq!(output.success, None);
    }

    let projection = derive_from_rollout(&restored);
    assert_eq!(projection.spine.cursor.to_string(), "1.1.1");
}

#[test]
fn adapter_does_not_accept_near_miss_spine_success_text() {
    let rollout = vec![
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("open", None, "Spine open accepted"),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
}

#[test]
fn spawn_bridge_keeps_toolcall_and_reduces_only_spawn_output_in_context() {
    let mut rollout = vec![message("user", "request")];
    rollout.extend([
        namespaced_call("spawn", "spine", "spawn", &spawn_arguments()),
        output("spawn", Some(true), &spawn_receipt()),
        message("user", "after"),
    ]);

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.spine.nodes.len(), 3);
    assert_eq!(projection.spine.nodes[1].summary.as_deref(), Some("first"));
    assert_eq!(projection.spine.nodes[2].summary.as_deref(), Some("second"));
    assert!(
        projection
            .spine
            .nodes
            .iter()
            .skip(1)
            .all(|node| node.status == NodeStatus::Closed)
    );
    assert_eq!(text(&projection.context[0]), "[U1]\nrequest");
    assert!(matches!(
        projection.context[1],
        ResponseItem::FunctionCall { .. }
    ));
    assert_eq!(
        output_text(&projection.context[2]),
        r#"{"status":"success"}"#
    );
    assert_eq!(
        text(&projection.context[4]),
        "<spine_memory node_id=\"1.1\">\nfirst memory\n</spine_memory>"
    );
    assert!(text(&projection.context[5]).contains("\"summary\": \"second\""));
    assert!(text(&projection.context[5]).contains("\"diagnostic\": \"child failed\""));
    assert_eq!(
        text(&projection.context[6]),
        "<spine_memory node_id=\"1.2\">\nsecond error memory\n</spine_memory>"
    );
    assert_eq!(text(&projection.context[7]), "[U2]\nafter");
    assert_eq!(projection.context.len(), 8);

    let effective = effective_rollout(&rollout);
    let events = lex_rollout(&effective, true);
    let mut live = SpineReducer::new();
    for event in events {
        live.apply(event);
    }
    assert_eq!(live.projection(), projection.spine);
    assert_eq!(
        materialize_context(
            &live.projection().visible_context,
            &rollout,
            None,
            None,
            true,
        ),
        projection.context
    );
    let before_receipt = derive_from_rollout(&rollout[..2]);
    assert_eq!(before_receipt.spine.nodes.len(), 1);
    assert_eq!(text(&before_receipt.context[0]), "[U1]\nrequest");
    assert!(matches!(
        before_receipt.context[1],
        ResponseItem::FunctionCall { .. }
    ));
    assert_eq!(response_items(&rollout).len(), 4);
}

#[test]
fn spawn_bridge_replay_accepts_persisted_carrier_without_success_metadata() {
    let rollout = vec![
        namespaced_call("spawn", "spine", "spawn", &spawn_arguments()),
        output("spawn", Some(true), &spawn_receipt()),
    ];
    let live = derive_from_rollout(&rollout);
    let persisted = serde_json::to_string(&rollout).expect("serialize spawn rollout");
    let mut restored: Vec<RolloutItem> = serde_json::from_str(&persisted).expect("restore rollout");
    let RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { output, .. }) =
        &mut restored[1]
    else {
        panic!("expected persisted spawn output");
    };
    assert_eq!(output.success, None);
    let replay = derive_from_rollout(&restored);
    assert_eq!(live, replay);
    assert_eq!(replay.spine.nodes.len(), 3);
    assert_eq!(replay.context.len(), 6);
}

#[test]
fn spawn_bridge_accepts_native_reasoning_and_leading_text() {
    let rollout = vec![
        reasoning(),
        message("assistant", "I will split this into two checks."),
        namespaced_call("shell", "", "shell", r#"{"cmd":"pwd"}"#),
        namespaced_call("spawn", "spine", "spawn", &spawn_arguments()),
        output("shell", Some(true), "workdir"),
        output("spawn", Some(true), &spawn_receipt()),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.nodes.len(), 3);
    assert!(matches!(
        projection.context.first(),
        Some(ResponseItem::Reasoning { .. })
    ));
    assert!(matches!(
        projection.context.get(1),
        Some(ResponseItem::Message { .. })
    ));
    assert_eq!(
        output_text(&projection.context[5]),
        r#"{"status":"success"}"#
    );
}

#[test]
fn spawn_lifecycle_rederives_from_selected_native_rollout_prefix() {
    let completed_prefix = vec![
        message("user", "before spawn"),
        namespaced_call("spawn", "spine", "spawn", &spawn_arguments()),
        output("spawn", Some(true), &spawn_receipt()),
    ];
    let completed = derive_from_rollout(&completed_prefix);
    assert_eq!(completed.spine.nodes.len(), 3);

    let persisted = serde_json::to_string(&completed_prefix).expect("serialize completed spawn");
    let restored: Vec<RolloutItem> =
        serde_json::from_str(&persisted).expect("restore completed spawn");
    assert_eq!(derive_from_rollout(&restored), completed);

    let before_receipt = derive_from_rollout(&completed_prefix[..2]);
    assert_eq!(before_receipt.spine.nodes.len(), 1);
    assert_eq!(text(&before_receipt.context[0]), "[U1]\nbefore spawn");
    assert!(matches!(
        before_receipt.context[1],
        ResponseItem::FunctionCall { .. }
    ));

    let retained_fork = derive_from_rollout(&completed_prefix);
    assert_eq!(retained_fork, completed);
    let pre_call_fork = derive_from_rollout(&completed_prefix[..1]);
    assert_eq!(pre_call_fork.spine.nodes.len(), 1);

    let mut rollback_after = completed_prefix.clone();
    rollback_after.push(message("user", "later turn"));
    rollback_after.push(RolloutItem::EventMsg(EventMsg::ThreadRolledBack(
        ThreadRolledBackEvent { num_turns: 1 },
    )));
    assert_eq!(derive_from_rollout(&rollback_after), completed);

    let mut rollback_before = completed_prefix;
    rollback_before.push(RolloutItem::EventMsg(EventMsg::ThreadRolledBack(
        ThreadRolledBackEvent { num_turns: 1 },
    )));
    let rolled_back_before = derive_from_rollout(&rollback_before);
    assert_eq!(rolled_back_before.spine.nodes.len(), 1);
    assert!(rolled_back_before.context.is_empty());
}

#[test]
fn spawn_bridge_keeps_malformed_failed_and_incomplete_groups_ordinary() {
    let cases = [
        vec![
            namespaced_call("spawn", "spine", "spawn", &spawn_arguments()),
            output("spawn", Some(true), "not a receipt"),
        ],
        vec![
            namespaced_call("spawn", "spine", "spawn", &spawn_arguments()),
            output(
                "spawn",
                Some(true),
                &serde_json::json!({"schema":"wrong","results":[]}).to_string(),
            ),
        ],
        vec![
            namespaced_call("spawn", "spine", "spawn", &spawn_arguments()),
            output("spawn", Some(false), "capacity rejected"),
        ],
        vec![namespaced_call(
            "spawn",
            "spine",
            "spawn",
            &spawn_arguments(),
        )],
    ];

    for (case, rollout) in cases.into_iter().enumerate() {
        let projection = derive_from_rollout(&rollout);
        assert_eq!(projection.spine.nodes.len(), 1);
        assert_eq!(projection.spine.cursor.to_string(), "1");
        if case < 3 {
            assert_eq!(
                output_text(&projection.context[1]),
                r#"{"status":"failure"}"#
            );
        } else {
            assert_eq!(projection.context, response_items(&rollout));
        }
    }
}

#[test]
fn spawn_bridge_feature_off_preserves_native_context_and_tree() {
    let rollout = vec![
        namespaced_call("spawn", "spine", "spawn", &spawn_arguments()),
        output("spawn", Some(true), &spawn_receipt()),
    ];
    let projection = derive_from_rollout_with_features(&rollout, true, false, false);
    assert_eq!(projection.spine.nodes.len(), 1);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.context, response_items(&rollout));
}

#[test]
fn closed_memory_projection_entries_follow_rollout_projection() {
    let rollout = vec![
        message("user", "request"),
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("open", Some(true), "ok"),
        message("user", "detail"),
        call("close", "spine.close", r#"{"memory":"done"}"#),
        output("close", Some(true), "ok"),
    ];

    let entries = closed_memory_projection_entries(&rollout, true);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].node_id, "1.1");
    assert_eq!(entries[0].summary, "task");
    assert_eq!(
        entries[0].body,
        "# Spine Memory 1.1\n\n## Node Memory\ndone"
    );
}

#[test]
fn user_message_projection_entries_follow_effective_rollout() {
    let rollout = vec![
        message(
            "user",
            "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>",
        ),
        message("user", "first"),
        message("assistant", "answer"),
        message("user", "rolled back"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
        message("user", "replacement"),
    ];

    assert_eq!(
        user_message_projection_entries(&rollout),
        vec![
            memory_projection::SpinetreeUserMessageProjectionEntry {
                anchor: 1,
                body: "first".to_string(),
            },
            memory_projection::SpinetreeUserMessageProjectionEntry {
                anchor: 2,
                body: "replacement".to_string(),
            },
        ]
    );
}

#[test]
fn adapter_projects_next_group_into_the_new_sibling() {
    let rollout = vec![
        message("user", "request"),
        call("open", "spine.open", r#"{"summary":"first"}"#),
        output("open", Some(true), "Spine open accepted."),
        message("user", "detail"),
        call(
            "next",
            "spine.next",
            r#"{"summary":"second","memory":"first done"}"#,
        ),
        output("next", Some(true), "Spine next accepted."),
        message("user", "continue"),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1.2");
    assert_eq!(text(&projection.context[0]), "[U1]\nrequest");
    assert_eq!(text(&projection.context[1]), "[U2]\ndetail");
    assert_eq!(
        text(&projection.context[2]),
        "<spine_memory node_id=\"1.1\">\nfirst done\n</spine_memory>"
    );
    assert!(text(&projection.context[3]).contains("id=\"1.2\""));
    assert!(matches!(
        projection.context[4],
        ResponseItem::FunctionCall { .. }
    ));
    assert!(matches!(
        projection.context[5],
        ResponseItem::FunctionCallOutput { .. }
    ));
    assert_eq!(text(&projection.context[6]), "[U3]\ncontinue");
}

#[test]
fn adapter_keeps_leading_assistant_and_multi_call_group_together() {
    let rollout = vec![
        message("assistant", "inspect first"),
        call("shell", "shell", r#"{"cmd":"pwd"}"#),
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("shell", Some(true), "workdir"),
        output("open", Some(true), "Spine open accepted."),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1.1");
    assert!(text(&projection.context[0]).starts_with("<spine_node"));
    assert_eq!(text(&projection.context[1]), "inspect first");
    assert_eq!(projection.context.len(), 6);
}

#[test]
fn failed_and_incomplete_control_outputs_do_not_transition() {
    let failed = vec![
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("open", Some(false), "failed"),
    ];
    let incomplete = vec![call("open", "spine.open", r#"{"summary":"task"}"#)];

    assert_eq!(derive_from_rollout(&failed).spine.cursor.to_string(), "1");
    assert_eq!(
        derive_from_rollout(&incomplete).spine.cursor.to_string(),
        "1"
    );
}

#[test]
fn successful_close_carrier_at_root_does_not_transition() {
    let rollout = vec![
        call("close", "spine.close", r#"{"memory":"invalid"}"#),
        output("close", Some(true), "Spine close accepted."),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.context.len(), rollout.len());
}

#[test]
fn trim_tags_only_large_completed_outputs_and_expires_after_next_toolcall() {
    let mut rollout = long_tool_rollout();
    let tagged = derive_from_rollout_with_features(&rollout, true, true, true);
    assert!(output_text(&tagged.context[1]).starts_with("[TRIM_ID: trim_1]"));

    rollout.extend([
        call("trim", "spine.trim", r#"{"TRIM_ID":"trim_1","op":"snip"}"#),
        output("trim", Some(true), "Spine trim accepted."),
    ]);
    let snipped = derive_from_rollout_with_features(&rollout, true, true, true);
    assert_eq!(
        output_text(&snipped.context[1]),
        TOOL_RESULT_CLEARED_MESSAGE
    );

    let mut expired = long_tool_rollout();
    expired.extend([
        call("next-tool", "shell", r#"{"cmd":"next"}"#),
        output("next-tool", Some(true), "short"),
    ]);
    let expired = derive_from_rollout_with_features(&expired, true, true, true);
    assert!(!output_text(&expired.context[1]).contains("TRIM_ID"));
}

#[test]
fn trim_slice_shapes_are_deterministic_and_independent_of_jit() {
    let base = long_tool_rollout();
    let cases = [
        (r#"{"TRIM_ID":"trim_1","op":"slice","head":4}"#, "0123"),
        (r#"{"TRIM_ID":"trim_1","op":"slice","tail":4}"#, "789\n"),
    ];
    for (arguments, expected_fragment) in cases {
        let mut rollout = base.clone();
        rollout.extend([
            call("trim", "spine.trim", arguments),
            output("trim", Some(true), "Spine trim accepted."),
        ]);
        for jit in [false, true] {
            let projection = derive_from_rollout_with_features(&rollout, jit, true, true);
            let output = &projection.context[1];
            assert_eq!(output_text(output), expected_fragment);
        }
    }

    let mut anchored = base;
    anchored.extend([
        call(
            "trim",
            "spine.trim",
            r#"{"TRIM_ID":"trim_1","op":"slice","anchor":"345","preceding":1,"following":1}"#,
        ),
        output("trim", Some(true), "Spine trim accepted."),
    ]);
    let projection = derive_from_rollout_with_features(&anchored, false, true, true);
    assert_eq!(
        output_text(&projection.context[1]),
        "0123456789\n0123456789\n"
    );
}

#[test]
fn trim_feature_matrix_preserves_native_shape_when_jit_is_off() {
    let rollout = long_tool_rollout();
    for (jit, trim, expected_tag) in [
        (false, false, false),
        (true, false, false),
        (false, true, true),
        (true, true, true),
    ] {
        let projection = derive_from_rollout_with_features(&rollout, jit, trim, true);
        let output = &projection.context[1];
        assert_eq!(output_text(output).contains("TRIM_ID"), expected_tag);
    }
}

#[test]
fn trim_feature_off_is_native_context_identity() {
    let rollout = long_tool_rollout();
    let expected = rollout
        .iter()
        .filter_map(|item| match item {
            RolloutItem::ResponseItem(item) => Some(item.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        derive_from_rollout_with_features(&rollout, false, false, true).context,
        expected
    );
}

#[test]
fn failed_and_incomplete_trim_requests_do_not_rewrite_output() {
    for suffix in [
        vec![
            call("trim", "spine.trim", r#"{"TRIM_ID":"trim_1","op":"snip"}"#),
            output("trim", Some(false), "trim rejected"),
        ],
        vec![call(
            "trim",
            "spine.trim",
            r#"{"TRIM_ID":"trim_1","op":"snip"}"#,
        )],
    ] {
        let mut rollout = long_tool_rollout();
        rollout.extend(suffix);
        let projection = derive_from_rollout_with_features(&rollout, false, true, true);
        let body = output_text(&projection.context[1]);
        assert!(!body.contains("TRIM_ID"));
        assert_ne!(body, TOOL_RESULT_CLEARED_MESSAGE);
    }
}

#[test]
fn trim_and_ordinary_tool_in_one_group_apply_old_edit_and_tag_new_output() {
    let mut rollout = long_tool_rollout();
    rollout.extend([
        call("trim", "spine.trim", r#"{"TRIM_ID":"trim_1","op":"snip"}"#),
        call("next-shell", "shell", r#"{"cmd":"next"}"#),
        output("trim", Some(true), "Spine trim accepted."),
        output(
            "next-shell",
            Some(true),
            &trim_candidate_text("new evidence\n"),
        ),
    ]);
    let projection = derive_from_rollout_with_features(&rollout, true, true, true);
    assert_eq!(
        output_text(&projection.context[1]),
        TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(output_text(&projection.context[5]).starts_with("[TRIM_ID: trim_5]"));
}

#[test]
fn trim_output_itself_never_becomes_a_candidate() {
    let rollout = vec![
        call("trim", "spine.trim", r#"{"TRIM_ID":"missing","op":"snip"}"#),
        output("trim", Some(true), &trim_candidate_text("not a candidate")),
    ];
    let projection = derive_from_rollout_with_features(&rollout, false, true, true);
    assert!(!output_text(&projection.context[1]).contains("TRIM_ID"));
}

#[test]
fn compact_replaces_old_trim_baseline_and_replays_new_candidates() {
    let replacement = vec![message("assistant", "native compact baseline")]
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::ResponseItem(item) => Some(item),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut rollout = long_tool_rollout();
    rollout.push(RolloutItem::Compacted(CompactedItem {
        message: "summary".to_string(),
        replacement_history: Some(replacement.clone()),
        window_number: Some(1),
        first_window_id: None,
        previous_window_id: None,
        window_id: None,
    }));
    rollout.extend([
        call("new-shell", "shell", r#"{"cmd":"new"}"#),
        output(
            "new-shell",
            Some(true),
            &trim_candidate_text("new evidence\n"),
        ),
    ]);
    let tagged = derive_from_rollout_with_features(&rollout, false, true, true);
    assert_eq!(tagged.context[0], replacement[0]);
    assert!(output_text(&tagged.context[2]).starts_with("[TRIM_ID: trim_4]"));

    rollout.extend([
        call("trim", "spine.trim", r#"{"TRIM_ID":"trim_4","op":"snip"}"#),
        output("trim", Some(true), "Spine trim accepted."),
    ]);
    let snipped = derive_from_rollout_with_features(&rollout, false, true, true);
    assert_eq!(
        output_text(&snipped.context[2]),
        TOOL_RESULT_CLEARED_MESSAGE
    );
}

#[test]
fn trim_rollback_and_fork_rederive_from_selected_native_prefix() {
    let first = long_tool_rollout();
    let mut rollout = vec![message("user", "first")];
    rollout.extend(first);
    rollout.push(message("user", "second"));
    rollout.extend([
        call("second-shell", "shell", r#"{"cmd":"second"}"#),
        output(
            "second-shell",
            Some(true),
            &trim_candidate_text("second result\n"),
        ),
        call("trim", "spine.trim", r#"{"TRIM_ID":"trim_5","op":"snip"}"#),
        output("trim", Some(true), "Spine trim accepted."),
    ]);

    let fork = derive_from_rollout_with_features(&rollout[..3], false, true, true);
    assert!(output_text(&fork.context[2]).starts_with("[TRIM_ID: trim_2]"));

    rollout.push(RolloutItem::EventMsg(EventMsg::ThreadRolledBack(
        ThreadRolledBackEvent { num_turns: 1 },
    )));
    let rolled_back = derive_from_rollout_with_features(&rollout, false, true, true);
    assert_eq!(rolled_back.context, fork.context);
}

#[test]
fn multiple_successful_controls_in_one_group_are_conflicting() {
    let rollout = vec![
        call("open", "spine.open", r#"{"summary":"task"}"#),
        call(
            "next",
            "spine.next",
            r#"{"summary":"sibling","memory":"done"}"#,
        ),
        output("open", Some(true), "Spine open accepted."),
        output("next", Some(true), "Spine next accepted."),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.context.len(), rollout.len());
}

#[test]
fn compact_replacement_history_is_materialized_exactly_once() {
    let replacement = vec![ResponseItem::Message {
        id: Some("replacement".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "native summary".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];
    let rollout = vec![
        message("user", "old"),
        RolloutItem::Compacted(CompactedItem {
            message: "summary".to_string(),
            replacement_history: Some(replacement.clone()),
            window_number: Some(1),
            first_window_id: None,
            previous_window_id: None,
            window_id: None,
        }),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "2");
    assert_eq!(projection.context, replacement);
}

#[test]
fn rollback_rederives_from_surviving_native_prefix() {
    let rollout = vec![
        message("user", "first"),
        call("open", "spine.open", r#"{"summary":"first task"}"#),
        spine_success_output("open", tool_response::SpineToolResponse::Open),
        message("user", "second"),
        call("close", "spine.close", r#"{"memory":"done"}"#),
        spine_success_output("close", tool_response::SpineToolResponse::Close),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ];

    let persisted = serde_json::to_string(&rollout).expect("serialize rollback rollout");
    let restored: Vec<RolloutItem> =
        serde_json::from_str(&persisted).expect("deserialize rollback rollout");
    let projection = derive_from_rollout(&restored);
    assert_eq!(projection.spine.cursor.to_string(), "1.1");
    assert_eq!(projection.context.len(), 4);
    assert_eq!(text(&projection.context[0]), "[U1]\nfirst");
}

#[test]
fn rollback_selected_prefix_trims_pre_turn_context_updates() {
    let rollout = vec![
        message(
            "developer",
            "<permissions instructions>base</permissions instructions>",
        ),
        message("user", "first"),
        message("assistant", "first response"),
        message(
            "developer",
            "<collaboration_mode>rolled back</collaboration_mode>",
        ),
        token_count(17),
        message("user", "second"),
        message("assistant", "second response"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.context.len(), 3);
    assert_eq!(
        text(&projection.context[0]),
        "<permissions instructions>base</permissions instructions>"
    );
    assert_eq!(text(&projection.context[1]), "[U1]\nfirst");
    assert_eq!(text(&projection.context[2]), "first response");
}

#[test]
fn fork_prefix_and_resume_full_rollout_are_pure_derivations() {
    let rollout = vec![
        message("user", "request"),
        call("open", "spine.open", r#"{"summary":"task"}"#),
        spine_success_output("open", tool_response::SpineToolResponse::Open),
        message("user", "detail"),
    ];
    let persisted = serde_json::to_string(&rollout).expect("serialize resumable rollout");
    let restored: Vec<RolloutItem> =
        serde_json::from_str(&persisted).expect("deserialize resumable rollout");
    let full = derive_from_rollout(&restored);
    let resumed = derive_from_rollout(&restored);
    let fork = derive_from_rollout(&restored[..3]);
    assert_eq!(full, resumed);
    assert_eq!(fork.spine.cursor.to_string(), "1.1");
    assert_eq!(fork.context.len(), 4);
}

#[test]
fn non_context_rollout_records_do_not_change_response_ordinals() {
    let response_only = vec![
        message("user", "request"),
        call("open", "spine.open", r#"{"summary":"task"}"#),
        output("open", Some(true), "ok"),
    ];
    let with_metadata = vec![
        response_only[0].clone(),
        RolloutItem::WorldState(WorldStateItem {
            full: true,
            state: serde_json::json!({"cwd":"/tmp"}),
        }),
        response_only[1].clone(),
        response_only[2].clone(),
    ];

    assert_eq!(
        derive_from_rollout(&response_only),
        derive_from_rollout(&with_metadata)
    );
}

#[test]
fn multimodal_user_items_are_preserved_while_text_is_tagged() {
    let item = ResponseItem::Message {
        id: Some("multimodal".to_string()),
        role: "user".to_string(),
        content: vec![
            ContentItem::InputImage {
                image_url: "data:image/png;base64,abc".to_string(),
                detail: None,
            },
            ContentItem::InputText {
                text: "inspect image".to_string(),
            },
        ],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let projection = derive_from_rollout(&[RolloutItem::ResponseItem(item)]);
    let ResponseItem::Message { content, .. } = &projection.context[0] else {
        panic!("expected message");
    };
    assert!(matches!(content[0], ContentItem::InputImage { .. }));
    assert!(matches!(
        &content[1],
        ContentItem::InputText { text } if text == "[U1]\ninspect image"
    ));
}

#[test]
fn contextual_user_message_keeps_host_role_without_consuming_an_anchor() {
    let rollout = vec![
        message(
            "user",
            "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>",
        ),
        message("user", "actual request"),
    ];
    let projection = derive_from_rollout(&rollout);

    assert_eq!(
        text(&projection.context[0]),
        "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>"
    );
    assert_eq!(text(&projection.context[1]), "[U1]\nactual request");
}

#[test]
fn closed_memory_user_slot_preserves_the_complete_native_message() {
    let item = ResponseItem::Message {
        id: Some("multimodal-memory".to_string()),
        role: "user".to_string(),
        content: vec![
            ContentItem::InputImage {
                image_url: "data:image/png;base64,abc".to_string(),
                detail: None,
            },
            ContentItem::InputText {
                text: "inspect image".to_string(),
            },
        ],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let mut expected = item.clone();
    tag_user_message(&mut expected, 1);
    let rollout = vec![
        call("open", "spine.open", r#"{"summary":"image task"}"#),
        output("open", Some(true), "ok"),
        RolloutItem::ResponseItem(item),
        call("close", "spine.close", r#"{"memory":"image inspected"}"#),
        output("close", Some(true), "ok"),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.context[0], expected);
    assert_eq!(
        text(&projection.context[1]),
        "<spine_memory node_id=\"1.1\">\nimage inspected\n</spine_memory>"
    );
}

#[test]
fn rollback_after_compact_keeps_native_replacement_baseline() {
    let replacement = vec![ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "native summary".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];
    let rollout = vec![
        message("user", "first"),
        RolloutItem::Compacted(CompactedItem {
            message: "summary".to_string(),
            replacement_history: Some(replacement.clone()),
            window_number: Some(1),
            first_window_id: None,
            previous_window_id: None,
            window_id: None,
        }),
        message("user", "rolled back"),
        call("open", "spine.open", r#"{"summary":"discarded"}"#),
        output("open", Some(true), "ok"),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ];

    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "2");
    assert_eq!(projection.context, replacement);
}

#[test]
fn adapter_returns_materialized_context_without_persistence() {
    let rollout = vec![message("user", "request")];
    let projection = derive_from_rollout(&rollout);
    assert_eq!(projection.spine.cursor.to_string(), "1");
    assert_eq!(projection.context.len(), 1);
    assert_eq!(text(&projection.context[0]), "[U1]\nrequest");
}

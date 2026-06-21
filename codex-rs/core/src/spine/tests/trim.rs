use super::*;

#[test]
fn completed_toolcall_tags_long_text_tool_response_for_next_turn_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "x".repeat(600);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "rendered long output should expose trim id: {:?}",
        rendered[1]
    );
    assert!(
        function_output_text_content(&rendered[1]).contains(&long_text),
        "tagging must keep original visible output until trim"
    );
    assert_eq!(function_output_text_content(&output), long_text);
    let trim_events = runtime.store.trim_events().expect("trim events");
    assert!(matches!(
        trim_events.as_slice(),
        [LoggedTrimEvent {
            trim_seq: 0,
            event: TrimEvent::Candidate {
                trim_id,
                toolcall_seq: 2,
                raw_ordinal: 1,
                context_index: 1,
                call_id,
                response_kind: TrimResponseKind::FunctionCallOutput,
                ..
            }
        }] if trim_id == "trim_0" && call_id == "long-tool"
    ));
}

#[test]
fn trim_only_runtime_tags_and_trims_without_tree_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let output = function_output_text("long-tool", &"trim-only output ".repeat(50));
    let raw = vec![Some(output.clone())];
    let mut runtime =
        SpineRuntime::load_or_create_with_jit(&rollout, 0, false).expect("create trim runtime");

    assert!(
        !runtime.store.tree_path_for_test().exists(),
        "trim-only must not create the JIT parser tree ledger"
    );
    runtime.observe_raw_items(1).expect("record raw");
    runtime
        .observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id: "long-tool".to_string(),
                request_call_ids: vec!["long-tool".to_string()],
                segments: vec![CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 0,
                    context_index: 0,
                }],
            },
            &raw,
        )
        .expect("observe trim-only completed toolcall");

    let projected = runtime
        .project_raw_history_with_trim(&[output.clone()])
        .expect("project trim-only history");
    assert!(
        function_output_text_content(&projected[0]).starts_with("[TRIM_ID: trim_1]\n"),
        "trim-only projection should expose the generated trim id"
    );
    assert_eq!(
        runtime.trim_tool_response("trim_1").expect("trim succeeds"),
        SpineTrimOutcome::Cleared {
            trim_id: "trim_1".to_string()
        }
    );
    let cleared = runtime
        .project_raw_history_with_trim(&[output])
        .expect("project cleared trim-only history");
    assert_eq!(
        function_output_text_content(&cleared[0]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        !runtime.store.tree_path_for_test().exists(),
        "trim-only trim must still not create the JIT parser tree ledger"
    );
}

#[test]
fn trim_only_fork_clone_copies_trim_ledger_without_tree_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let output = function_output_text("long-tool", &"trim-only fork output ".repeat(50));
    let raw = vec![Some(output.clone())];
    let mut source = SpineRuntime::load_or_create_with_jit(&source_rollout, 0, false)
        .expect("create trim runtime");

    source.observe_raw_items(1).expect("record raw");
    source
        .observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id: "long-tool".to_string(),
                request_call_ids: vec!["long-tool".to_string()],
                segments: vec![CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 0,
                    context_index: 0,
                }],
            },
            &raw,
        )
        .expect("observe trim-only completed toolcall");
    source.trim_tool_response("trim_1").expect("clear trim id");
    assert!(
        !source.store.tree_path_for_test().exists(),
        "trim-only source must not create the JIT parser tree ledger"
    );

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[true]);
    let target =
        SpineRuntime::load_or_create_with_jit(&target_rollout, 1, false).expect("load target");
    let projected = target
        .project_raw_history_with_trim(&[output])
        .expect("project target");
    assert_eq!(
        function_output_text_content(&projected[0]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        !target.store.tree_path_for_test().exists(),
        "trim-only clone must not create the JIT parser tree ledger"
    );
}

#[test]
fn completed_toolcall_tags_long_custom_tool_response_for_next_turn_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("custom_tool", "custom-long-tool");
    let long_text = "custom output ".repeat(60);
    let output = custom_tool_output_text("custom-long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("custom-long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert!(
        custom_tool_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "rendered custom output should expose trim id: {:?}",
        rendered[1]
    );
    assert!(
        custom_tool_output_text_content(&rendered[1]).contains(&long_text),
        "tagging must keep original custom output visible until trim"
    );
    assert_eq!(custom_tool_output_text_content(&output), long_text);
    let trim_events = runtime.store.trim_events().expect("trim events");
    assert!(matches!(
        trim_events.as_slice(),
        [LoggedTrimEvent {
            trim_seq: 0,
            event: TrimEvent::Candidate {
                trim_id,
                toolcall_seq: 2,
                raw_ordinal: 1,
                context_index: 1,
                call_id,
                response_kind: TrimResponseKind::CustomToolCallOutput,
                ..
            }
        }] if trim_id == "trim_0" && call_id == "custom-long-tool"
    ));
}

#[test]
fn completed_toolcall_does_not_tag_content_items_tool_response_for_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "content-items-tool");
    let output = function_output_content_items("content-items-tool", &"content item ".repeat(80));
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("content-items-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime.materialize_history(&raw).expect("materialize"),
        vec![request, output]
    );
    assert!(runtime.store.trim_events().expect("trim events").is_empty());
}

#[test]
fn completed_toolcall_does_not_tag_short_tool_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "short-tool");
    let output = function_output("short-tool");
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("short-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime.materialize_history(&raw).expect("materialize"),
        vec![request, output]
    );
    assert!(runtime.store.trim_events().expect("trim events").is_empty());
}

#[test]
fn trim_tool_response_clears_visible_projection_and_preserves_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime.trim_tool_response("trim_0").expect("trim succeeds"),
        SpineTrimOutcome::Cleared {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert_eq!(
        function_output_text_content(&rendered[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert_eq!(function_output_text_content(&output), long_text);
    assert_eq!(
        runtime
            .trim_tool_response("trim_0")
            .expect("repeat trim is idempotent"),
        SpineTrimOutcome::AlreadyCleared {
            trim_id: "trim_0".to_string()
        }
    );

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_history(&raw)
        .expect("replayed trim projection");
    assert_eq!(
        function_output_text_content(&replayed_rendered[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
}

#[test]
fn trim_slice_head_rewrites_visible_projection_and_preserves_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "abcdefg ".repeat(80);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime
            .slice_tool_response_head("trim_0", 7, &raw)
            .expect("slice succeeds"),
        SpineTrimOutcome::Sliced {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abcdefg");
    assert_eq!(function_output_text_content(&output), long_text);
    assert!(matches!(
        runtime.store.trim_events().expect("persisted trim events").as_slice(),
        [
            LoggedTrimEvent {
                event: TrimEvent::Candidate { trim_id, .. },
                ..
            },
            LoggedTrimEvent {
                event: TrimEvent::Sliced { trim_id: sliced_id, .. },
                ..
            }
        ] if trim_id == "trim_0" && sliced_id == "trim_0"
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_history(&raw)
        .expect("materialize replay");
    assert_eq!(
        function_output_text_content(&replayed_rendered[1]),
        "abcdefg"
    );
}

#[test]
fn trim_slice_tail_rewrites_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!("{}TAIL-END", "prefix ".repeat(90));
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_tail("trim_0", 8, &raw)
        .expect("slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "TAIL-END");
}

#[test]
fn trim_slice_anchor_window_rewrites_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!(
        "{}abc<needle>xyz{}",
        "left ".repeat(60),
        " right".repeat(60)
    );
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_anchor("trim_0", "<needle>", 3, 3, &raw)
        .expect("slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abc<needle>xyz");
}

#[test]
fn trim_slice_rejects_missing_anchor_without_projection_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert!(matches!(
        runtime.slice_tool_response_anchor("trim_0", "missing", 1, 1, &raw),
        Ok(SpineTrimOutcome::Miss { trim_id }) if trim_id == "trim_0"
    ));
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "missing anchor must not change visible projection"
    );
}

#[test]
fn trim_repeated_slice_applies_to_current_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!("{}abcdef{}", "prefix ".repeat(60), " suffix".repeat(60));
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_anchor("trim_0", "abcdef", 0, 0, &raw)
        .expect("first slice succeeds");
    runtime
        .slice_tool_response_head("trim_0", 3, &raw)
        .expect("second slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abc");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_history(&raw)
        .expect("materialize replay");
    assert_eq!(function_output_text_content(&replayed_rendered[1]), "abc");
}

#[test]
fn trim_snip_after_slice_clears_visible_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_head("trim_0", 9, &raw)
        .expect("slice succeeds");
    assert_eq!(
        runtime.trim_tool_response("trim_0").expect("snip succeeds"),
        SpineTrimOutcome::Cleared {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(
        function_output_text_content(&rendered[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
}

#[test]
fn trim_tool_response_only_matches_latest_completed_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request_1 = ordinary_call("shell_command", "long-tool");
    let output_1 = function_output_text("long-tool", &"old output ".repeat(80));
    let request_2 = ordinary_call("shell_command", "short-tool");
    let output_2 = function_output("short-tool");
    let raw = vec![
        Some(request_1.clone()),
        Some(output_1.clone()),
        Some(request_2.clone()),
        Some(output_2.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record first raw");
    runtime
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    runtime
        .observe_context_item(1, 1, &output_1)
        .expect("observe first output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe first completed toolcall");
    runtime.observe_raw_items(2).expect("record second raw");
    runtime
        .observe_context_item(2, 2, &request_2)
        .expect("observe second request");
    runtime
        .observe_context_item(3, 3, &output_2)
        .expect("observe second output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("short-tool", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe second completed toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("trim_0")
            .expect("old trim id misses after newer completed toolcall"),
        SpineTrimOutcome::Miss {
            trim_id: "trim_0".to_string()
        }
    );
    assert!(matches!(
        runtime.store.trim_events().expect("trim events").as_slice(),
        [LoggedTrimEvent {
            event: TrimEvent::Candidate { trim_id, .. },
            ..
        }] if trim_id == "trim_0"
    ));
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "miss must not clear the old output"
    );
}

#[test]
fn trim_tool_response_does_not_retry_old_id_after_missed_attempt_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let trim_request = spine_call(SPINE_TOOL_TRIM, "trim-miss");
    let trim_output = function_output_text("trim-miss", "Do not retry this trim id.");
    let raw = vec![
        Some(request.clone()),
        Some(output.clone()),
        Some(trim_request.clone()),
        Some(trim_output.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record target raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe target request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe target output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe target completed toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("unknown_trim")
            .expect("unknown trim id misses"),
        SpineTrimOutcome::Miss {
            trim_id: "unknown_trim".to_string()
        }
    );

    runtime
        .observe_raw_items(2)
        .expect("record committed trim attempt raw");
    runtime
        .observe_context_item(2, 2, &trim_request)
        .expect("observe trim attempt request");
    runtime
        .observe_context_item(3, 3, &trim_output)
        .expect("observe trim attempt output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("trim-miss", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe committed trim attempt as latest toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("trim_0")
            .expect("old target trim id is no longer in previous completed toolcall"),
        SpineTrimOutcome::Miss {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "missed attempt commit must not make the older target retryable"
    );
    assert!(
        function_output_text_content(&rendered[1]).contains(&long_text),
        "missed attempt commit must leave the older target body intact under the tag"
    );
}

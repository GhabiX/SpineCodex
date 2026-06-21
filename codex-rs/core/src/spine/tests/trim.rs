use super::*;

#[test]
fn rollback_before_trim_clear_restores_tagged_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let mut raw = vec![Some(request.clone()), Some(output.clone())];
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
        .checkpoint_before_user_msg(&rollout, 2, &raw)
        .expect("checkpoint before trim clear");
    runtime
        .trim_tool_response("trim_0")
        .expect("clear trim target");
    raw.push(None);
    runtime
        .observe_raw_items(1)
        .expect("record rolled-back raw");
    runtime
        .observe_context_item(2, 2, &text_item("rolled back"))
        .expect("observe rolled-back msg");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[2])
        .expect("load rollback")
        .expect("sidecar exists");
    let materialized = replayed.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized[0], request);
    let rolled_back_output = function_output_text_content(&materialized[1]);
    assert!(
        rolled_back_output.starts_with("[TRIM_ID: trim_0]\n"),
        "rollback before clear must keep the candidate tag visible, got: {rolled_back_output:?}"
    );
    assert!(
        rolled_back_output.contains(&long_text),
        "rollback before clear must restore the original visible body under the tag"
    );
}

#[test]
fn rollback_before_trim_candidate_removes_trim_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let output = function_output_text("long-tool", &"important raw output ".repeat(40));
    let raw_after_rollback = vec![None, None];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .checkpoint_before_user_msg(&rollout, 0, &[])
        .expect("checkpoint before candidate");
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
            &[Some(request), Some(output)],
        )
        .expect("observe completed toolcall");

    let mut replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[0])
        .expect("load rollback")
        .expect("sidecar exists");
    assert!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize")
            .is_empty()
    );
    assert_eq!(
        replayed
            .trim_tool_response("trim_0")
            .expect("trim id should be outside rollback-visible state"),
        SpineTrimOutcome::Miss {
            trim_id: "trim_0".to_string()
        }
    );
}

#[test]
fn fork_after_trim_clear_preserves_projection_and_allocates_non_colliding_trim_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let request_1 = ordinary_call("shell_command", "first-long-tool");
    let output_1 = function_output_text("first-long-tool", &"first raw output ".repeat(50));
    let request_2 = ordinary_call("shell_command", "second-long-tool");
    let output_2 = function_output_text("second-long-tool", &"second raw output ".repeat(50));
    let raw = vec![
        Some(request_1.clone()),
        Some(output_1.clone()),
        Some(request_2.clone()),
        Some(output_2.clone()),
    ];
    let mut source = SpineRuntime::load_or_create(&source_rollout, 0).expect("create source");

    source.observe_raw_items(2).expect("record source raw");
    source
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    source
        .observe_context_item(1, 1, &output_1)
        .expect("observe first output");
    source
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("first-long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe first completed toolcall");
    source
        .trim_tool_response("trim_0")
        .expect("clear first long output");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[true, true]);
    let target = SpineRuntime::load_for_rollout_items(&target_rollout, &raw[..2], &[])
        .expect("load target")
        .expect("target sidecar exists");
    let target_visible = target
        .materialize_history(&raw[..2])
        .expect("materialize target");
    assert_eq!(
        function_output_text_content(&target_visible[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    drop(target);

    let mut forked = SpineRuntime::load_or_create(&target_rollout, 2).expect("load fork writer");
    forked.observe_raw_items(2).expect("record second raw");
    forked
        .observe_context_item(2, 2, &request_2)
        .expect("observe second request");
    forked
        .observe_context_item(3, 3, &output_2)
        .expect("observe second output");
    forked
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("second-long-tool", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe second completed toolcall");

    let fork_visible = forked.materialize_history(&raw).expect("materialize fork");
    assert_eq!(
        function_output_text_content(&fork_visible[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        function_output_text_content(&fork_visible[3]).starts_with("[TRIM_ID: trim_2]\n"),
        "fork must continue after copied candidate+clear seqs without reusing trim_0"
    );
}

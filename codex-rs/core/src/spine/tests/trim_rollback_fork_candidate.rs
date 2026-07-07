use super::*;

#[test]
fn rollback_before_trim_candidate_removes_trim_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let output = function_output_text("long-tool", &trim_candidate_text("important raw output "));
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
            single_request_response_toolcall("long-tool", 0, 0, 1, 1),
            &[Some(request), Some(output)],
        )
        .expect("observe completed toolcall");

    let mut replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[0])
        .expect("load rollback")
        .expect("sidecar exists");
    assert!(
        replayed
            .materialize_variable_context_for_test(&raw_after_rollback)
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

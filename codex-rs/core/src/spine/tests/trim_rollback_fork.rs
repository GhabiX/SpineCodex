use super::*;

#[test]
fn rollback_before_trim_clear_restores_tagged_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = trim_candidate_text("important raw output ");
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
    let materialized = replayed
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
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

use super::*;

#[test]
fn trim_tool_response_clears_visible_projection_and_preserves_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = trim_candidate_text("important raw output ");
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
    let rendered = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
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
        .materialize_variable_context_for_test(&raw)
        .expect("replayed trim projection");
    assert_eq!(
        function_output_text_content(&replayed_rendered[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
}

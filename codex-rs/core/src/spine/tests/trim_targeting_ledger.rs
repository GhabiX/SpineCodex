use super::*;

#[test]
fn missing_trim_ledger_fails_closed_instead_of_restoring_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = trim_candidate_text("important raw output ");
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request), Some(output)];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, raw[0].as_ref().expect("request"))
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, raw[1].as_ref().expect("output"))
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");
    let rendered = runtime
        .materialize_variable_context_for_test(&raw)
        .expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "corruption fixture should have trim projection before the ledger is moved aside"
    );

    let parked_trim_ledger = dir.path().join("parked-trim.jsonl");
    std::fs::rename(runtime.store.trim_path_for_test(), &parked_trim_ledger)
        .expect("park trim ledger to simulate corruption");
    let err = match SpineRuntime::load_for_rollout_items(&rollout, &raw, &[]) {
        Err(err) => err,
        Ok(_) => panic!("missing trim ledger must fail closed"),
    };
    assert!(
        err.to_string()
            .contains("missing required Spine trim ledger"),
        "unexpected missing trim ledger error: {err}"
    );
}

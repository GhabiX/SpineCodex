use super::pending_control_raw_requests::observe_spine_request_with_args;
use super::*;

#[test]
fn open_commit_uses_raw_request_without_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let (_, req_raw, req_context) = observe_spine_request_with_args(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_OPEN,
        "open",
        r#"{"summary":"raw child"}"#,
    );
    let (_, resp_raw, resp_context) = observe_function_output(&mut runtime, &mut raw, "open");
    let completed =
        single_request_response_toolcall("open", req_raw, req_context, resp_raw, resp_context);

    let parse_stack_before = runtime.parse_stack().clone();
    runtime
        .maybe_commit_output_with_toolcall_and_raw_items(
            "open",
            None,
            SpineTokenBaselines::default(),
            completed,
            &raw,
        )
        .expect("commit raw-request open");

    assert_ne!(runtime.parse_stack(), &parse_stack_before);
    assert!(
        runtime
            .pending_commit("open")
            .expect("raw request consumed")
            .is_none()
    );
}

#[test]
fn open_raw_request_reuses_existing_request_anchor_when_rebuilding_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let (_, req_raw, req_context) = observe_spine_request_with_args(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_OPEN,
        "sg_call_0",
        r#"{"summary":"raw child"}"#,
    );
    runtime
        .stage_open("sg_call_0".to_string(), "raw child".to_string())
        .expect("stage open before abort");
    assert!(runtime.abort_pending("sg_call_0"));
    let (_, resp_raw, resp_context) = observe_function_output(&mut runtime, &mut raw, "sg_call_0");
    let completed =
        single_request_response_toolcall("sg_call_0", req_raw, req_context, resp_raw, resp_context);

    runtime
        .maybe_commit_output_with_toolcall_and_raw_items(
            "sg_call_0",
            None,
            SpineTokenBaselines::default(),
            completed,
            &raw,
        )
        .expect("rebuilding pending from raw items must reuse the existing request anchor");

    assert!(
        runtime
            .pending_commit("sg_call_0")
            .expect("pending query after open commit")
            .is_none()
    );
}

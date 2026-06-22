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
    let completed = completed_toolcall(
        "open",
        vec![
            tool_req(req_raw, req_context),
            tool_resp(resp_raw, resp_context),
        ],
    );

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

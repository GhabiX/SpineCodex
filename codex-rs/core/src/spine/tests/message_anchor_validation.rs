use super::*;

#[test]
fn close_memory_rejects_unknown_user_anchor_reference() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "known user");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    let err = runtime
        .stage_close(
            "close".to_string(),
            "This memory cites [U99], which does not exist.".to_string(),
        )
        .expect_err("unknown user anchor must fail");
    assert!(
        err.to_string().contains("unknown user anchor [U99]"),
        "{err}"
    );
    runtime
        .stage_close(
            "close".to_string(),
            "This memory cites the existing [U1].".to_string(),
        )
        .expect("known user anchor should be accepted");
}

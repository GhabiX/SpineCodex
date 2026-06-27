use super::*;

#[test]
fn fork_clone_suffix_replay_rejects_request_only_hole_before_later_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let _source = SpineRuntime::load_or_create(&source_rollout, 0).expect("create source");
    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout, 0)
        .expect("capture clone boundary")
        .expect("source sidecar exists");
    let raw = vec![
        Some(ordinary_call("shell_command", "hole-0")),
        Some(text_item("later msg must not overtake request-only hole")),
    ];
    let mut state = SpineSessionState::new();

    let err = state
        .install_cloned_sidecar_for_fork(&boundary, &target_rollout, &raw)
        .expect_err("fork clone must reject later parser-visible item behind open tool request");
    assert!(
        err.to_string()
            .contains("while durable tool requests are pending"),
        "unexpected fork clone error: {err}"
    );
}

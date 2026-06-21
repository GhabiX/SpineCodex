use super::*;

#[test]
fn spine_error_classifies_missing_raw_coverage_as_sidecar_corruption() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    let raw = vec![Some(text_item("uncovered durable item"))];

    let err = runtime
        .validate_raw_coverage(&raw)
        .expect_err("missing durable raw coverage must fail closed");
    assert_eq!(err.class(), SpineErrorClass::SidecarCorruption);
    assert!(err.should_invalidate_runtime());
    assert!(
        err.to_string()
            .contains("spine sidecar is missing token coverage for raw ordinal 0"),
        "unexpected coverage error: {err}"
    );
    assert!(err.to_string().contains("token_seq="));
}

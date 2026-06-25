use super::*;

#[test]
fn root_compact_checkpoint_validates_against_root_compact_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root visible work");
    let result = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("compact root with checkpoint");

    runtime
        .store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &runtime.raw_live,
            &raw,
            result.raw_boundary,
            result.variable_context(),
        )
        .expect("runtime compact checkpoint should bind to RootCompact marker");
}

use super::compact_checkpoint_validation::root_epoch_mem_record;
use super::*;

#[test]
fn compact_checkpoint_without_root_compact_marker_fails_validation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let body_path = store
        .write_memory_body("root-1-0", body)
        .expect("write body");
    let mem = root_epoch_mem_record("root-1-0", body, body_path.clone());
    store.append_mem(&mem).expect("append mem");
    store
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &rollout, &mem, body, 0, 1, body_path,
        ))
        .expect("append compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect_err("checkpoint without RootCompact marker must fail closed");
    assert!(
        err.to_string().contains("is not preceded by RootCompact"),
        "unexpected checkpoint validation error: {err}"
    );
}

use super::clone_missing_memory::assert_clone_for_rollout_fails_closed_when_visible_memory_body_is_missing;

#[test]
fn fork_missing_memory_artifact_fails_closed() {
    assert_clone_for_rollout_fails_closed_when_visible_memory_body_is_missing();
}

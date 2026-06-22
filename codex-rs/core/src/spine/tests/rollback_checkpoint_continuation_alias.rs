use super::rollback_checkpoint_continuation::assert_rollback_checkpoint_new_open_reuses_restored_sibling_id;

#[test]
fn rollback_allocates_correct_sibling_after_restore() {
    assert_rollback_checkpoint_new_open_reuses_restored_sibling_id();
}

use super::rollback_checkpoint_restore::assert_rollback_uses_pre_user_checkpoint_to_restore_parse_stack;

#[test]
fn rollback_restores_parse_stack_before_target_user_msg() {
    assert_rollback_uses_pre_user_checkpoint_to_restore_parse_stack();
}

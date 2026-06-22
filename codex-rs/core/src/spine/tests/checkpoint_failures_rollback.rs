use super::checkpoint_failures::assert_checkpoint_missing_required_field_fails_closed;

#[test]
fn rollback_checkpoint_missing_field_fails_closed() {
    assert_checkpoint_missing_required_field_fails_closed();
}

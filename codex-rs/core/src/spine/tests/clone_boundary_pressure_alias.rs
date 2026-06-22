use super::clone_boundary_pressure::assert_clone_boundary_excludes_future_structural_and_pressure_records;

#[test]
fn fork_preserves_context_pressure_metadata() {
    assert_clone_boundary_excludes_future_structural_and_pressure_records();
}

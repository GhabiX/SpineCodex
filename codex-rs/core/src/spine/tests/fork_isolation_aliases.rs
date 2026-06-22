use super::fork_isolation::assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent;

#[test]
fn fork_child_initial_h_ps_matches_parent() {
    assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent();
}

#[test]
fn fork_child_mutation_does_not_change_parent() {
    assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent();
}

#[test]
fn fork_rewrites_node_dir_to_child_sidecar() {
    assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent();
}

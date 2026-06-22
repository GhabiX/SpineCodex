use super::*;

#[test]
fn closed_child_tree_snapshot_preserves_zero_source_suffix_accounting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-zero-child",
        "zero child",
        provider_token_baselines(5_000),
    );
    append_msg(&mut runtime, &mut raw, "zero child work");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-zero-child",
        "1.1.1",
        provider_token_baselines(5_000),
    );

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("closed child should reduce into ParseStack nodes")
    };
    let memory = nodes
        .iter()
        .find_map(|node| match node {
            SpineTreeNode::SpineTree { memory, .. } => Some(memory),
            _ => None,
        })
        .expect("closed child memory ref");
    assert_eq!(memory.open_context_tokens, Some(5_000));
    assert_eq!(memory.close_context_tokens, Some(5_000));
    assert_eq!(memory.closed_source_suffix_tokens, Some(0));

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    let nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(
        nodes["1.1.1"].accounting,
        closed_child_accounting_with_source_suffix(0)
    );

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let replayed_snapshot = replayed.build_tree_snapshot().expect("replay snapshot");
    let replayed_nodes = snapshot_nodes_by_id(&replayed_snapshot);
    assert_eq!(
        replayed_nodes["1.1.1"].accounting,
        nodes["1.1.1"].accounting
    );
}

pub(super) fn provider_token_baselines(provider_input_tokens: u64) -> SpineTokenBaselines {
    SpineTokenBaselines {
        provider_input_tokens: Some(provider_input_tokens),
    }
}

pub(super) fn closed_child_accounting_with_source_suffix(
    closed_source_suffix_tokens: u64,
) -> Option<SpineTreeNodeAccountingSnapshot> {
    Some(SpineTreeNodeAccountingSnapshot {
        current_node_context_tokens: None,
        current_node_context_problem: None,
        current_node_context_baseline_source: None,
        closed_source_suffix_tokens: Some(closed_source_suffix_tokens),
        closed_memory_context_tokens: None,
        memory_output_tokens: Some(1_250),
    })
}

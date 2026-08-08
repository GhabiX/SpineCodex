use super::*;
use pretty_assertions::assert_eq;

#[test]
fn typed_fragments_own_exact_rendering() {
    let node = SpineNodeFragment::new(
        &NodeId::root_epoch(1).child(1),
        "child <scope>",
        NodeStatus::Live,
        NodeContextCost::Percentage(13),
        "Node guidance.",
    )
    .unwrap();
    let memory = SpineMemoryFragment::new(&NodeId::root_epoch(1), "finished").unwrap();
    let opened = SpineNodeFragment::new(
        &NodeId::root_epoch(1).child(1),
        "child <scope>",
        NodeStatus::Opened,
        NodeContextCost::Unavailable,
        "Node guidance.",
    )
    .unwrap();

    assert_eq!(
        node.render(),
        "<spine_node id=\"1.1\" summary=\"child &lt;scope&gt;\" status=\"live\">\nCurrent Remaining Context Windows: 87%\nNode guidance.\n</spine_node>"
    );
    assert_eq!(
        memory.render(),
        "<spine_memory node_id=\"1\">\nfinished\n</spine_memory>"
    );
    assert_eq!(
        opened.render(),
        "<spine_node id=\"1.1\" summary=\"child &lt;scope&gt;\" status=\"opened\">\nCurrent Remaining Context Windows: unavailable\nNode guidance.\n</spine_node>"
    );
}

#[test]
fn typed_node_fragment_saturates_remaining_context_at_zero() {
    let node = SpineNodeFragment::new(
        &NodeId::root_epoch(1).child(1),
        "exhausted",
        NodeStatus::Opened,
        NodeContextCost::Percentage(101),
        "",
    )
    .unwrap();

    assert_eq!(
        node.render(),
        "<spine_node id=\"1.1\" summary=\"exhausted\" status=\"opened\">\nCurrent Remaining Context Windows: 0%\n</spine_node>"
    );
}

#[test]
fn final_rendered_fragment_has_a_hard_byte_limit() {
    let accepted = SpineMemoryFragment::new(
        &NodeId::root_epoch(1),
        &"x".repeat(MAX_SPINE_FRAGMENT_BYTES - 64),
    )
    .unwrap();
    let result = SpineMemoryFragment::new(
        &NodeId::root_epoch(1),
        &"x".repeat(MAX_SPINE_FRAGMENT_BYTES),
    );

    assert!(accepted.render().len() <= MAX_SPINE_FRAGMENT_BYTES);
    assert!(result.is_err());
}

use super::*;
use pretty_assertions::assert_eq;

#[test]
fn context_cost_rounds_up() {
    assert_eq!(
        context_cost(Some(10_001), 80_000),
        NodeContextCost::Percentage(13)
    );
}

#[test]
fn node_context_cost_is_unavailable_without_a_complete_coordinate() {
    assert_eq!(context_cost(None, 80_000), NodeContextCost::Unavailable);
}

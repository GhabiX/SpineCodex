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

#[test]
fn context_pressure_keeps_missing_and_invalid_coordinates_visible() {
    assert_eq!(
        context_pressure(Some(100), None).problem,
        Some(ContextPressureProblem::MissingCurrentUsage)
    );
    assert_eq!(
        context_pressure(None, Some(200)).problem,
        Some(ContextPressureProblem::MissingOpenContextBaseline)
    );
    let mismatch = context_pressure(Some(200), Some(100));
    assert_eq!(mismatch.context_tokens, None);
    assert_eq!(
        mismatch.problem,
        Some(ContextPressureProblem::CoordinateMismatch)
    );
}

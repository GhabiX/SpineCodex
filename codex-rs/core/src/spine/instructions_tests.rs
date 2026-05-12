use super::SPINE_VIEW_INSTRUCTIONS;
use super::append_spine_view_instructions;
use super::strip_spine_view_instructions;

#[test]
fn feature_off_leaves_base_instructions_byte_identical() {
    let base = "base instructions\nwith punctuation: !?".to_string();
    let actual = append_spine_view_instructions(base.clone(), false);

    assert_eq!(actual.as_bytes(), base.as_bytes());
}

#[test]
fn feature_on_appends_exact_spine_view_instructions() {
    let base = "base instructions".to_string();
    let actual = append_spine_view_instructions(base.clone(), true);

    assert_eq!(actual, format!("{base}\n\n{SPINE_VIEW_INSTRUCTIONS}"));
    assert_eq!(actual.matches("<spine_view>").count(), 1);
}

#[test]
fn feature_on_does_not_append_spine_view_twice() {
    let base = format!("base instructions\n\n{SPINE_VIEW_INSTRUCTIONS}");
    let actual = append_spine_view_instructions(base.clone(), true);

    assert_eq!(actual, base);
    assert_eq!(actual.matches("<spine_view>").count(), 1);
}

#[test]
fn strip_spine_view_restores_original_base_suffix() {
    let base = "base instructions";
    let actual = append_spine_view_instructions(base.to_string(), true);

    assert_eq!(strip_spine_view_instructions(&actual), base);
    assert_eq!(strip_spine_view_instructions(base), base);
}

#[test]
fn spine_view_instructions_discourage_turn_by_turn_fragmentation() {
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("not as a per-message or per-turn log"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("task plan and context manager"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("At the start, form a compact Spine plan"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Default to staying in the current live node"));
    assert!(
        SPINE_VIEW_INSTRUCTIONS.contains(
            "Move spine only when a scope boundary improves focus, cost, or future recall"
        )
    );
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("investigate/localize -> implement/verify"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("one node per shell command"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("one-turn-per-node fragmentation"));
}

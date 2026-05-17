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
    assert!(actual.contains(
        "Spine Memory is internal context; never expose or imitate it in user-visible messages."
    ));
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
fn spine_view_instructions_keep_core_contract() {
    for required in [
        "<spine_view>",
        "</spine_view>",
        "runtime-generated memory IR",
        "Spine Memory is internal context; never expose or imitate it in user-visible messages.",
        "task_projection.current.checklist",
        "task_projection.draft_nodes",
        "never send spine_plantree as input",
        "does not create, finish, close, compact, or move Spine nodes",
        "spine.next/close are not end-of-response cleanup",
        "Do not use summary, child_summary, or instruction with spine.open",
        "Completed Spine nodes are read-only",
    ] {
        assert!(
            SPINE_VIEW_INSTRUCTIONS.contains(required),
            "missing Spine instruction contract anchor {required:?}"
        );
    }

    for forbidden in [
        "A node is a working scope, not a checklist item",
        "The node summary is only a short tree label",
        "use update_plan with spine_plantree",
        "spine_plantree.root.children",
        "current spine_plantree",
        "update spine_plantree first",
    ] {
        assert!(
            !SPINE_VIEW_INSTRUCTIONS.contains(forbidden),
            "unexpected legacy Spine instruction {forbidden:?}"
        );
    }
}

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
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("<spine_view>"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("</spine_view>"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("runtime-generated memory IR"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Use Spine effectively and efficiently"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("substantial raw history"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("future work is likely to reuse"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("raw details are still useful"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("coherent work scope is complete"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("task_projection"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("single Spine planning input"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("One call should include both"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("task_projection.current.checklist"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("task_projection.draft_nodes"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("current real Spine node checklist"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("future planned scopes"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("parent real node id or earlier ~draft_id"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("never send spine_plantree as input"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("returned normalized spine_plantree is runtime output only"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("top-level plan"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Successful writable updates return spine_tree"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("treat it as authoritative"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("planning only"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains(
        "does not create, finish, close, compact, or move Spine nodes"
    ));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("refresh task_projection"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("planned draft children"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("materialize it before doing the work"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("immediately call update_plan in the new child"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("that draft's summary/checklist"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("do not bypass planned child scopes"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Do not combine task_projection with top-level plan"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Move Spine at coherent scope boundaries"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("start a focused child scope"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("matching planned child scope"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("sibling-level under the same parent"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("spine.next/close are not end-of-response cleanup"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("finish its user-visible work there"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("genuinely new sibling/parent-sibling work"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains(
        "Spine transitions are internal context-management steps, not substitutes for normal Codex turn delivery"
    ));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains(
        "after spine.next or spine.close, continue work if the latest user request remains unfinished"
    ));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains(
        "send the user-facing final answer/update if that request is complete, paused, blocked, or needs a decision"
    ));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains(
        "Do not use a Spine Tree update, tool output, or generated memory as the user-visible report"
    ));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("around 80k raw tokens"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("every additional 30k"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Treat the warning as a cue"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("spine.next"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("spine.close"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("child_summary"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("optional instruction argument"));
    assert!(
        SPINE_VIEW_INSTRUCTIONS
            .contains("Do not use summary, child_summary, or instruction with spine.open")
    );
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Completed Spine nodes are read-only"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("A node is a working scope, not a checklist item"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("The node summary is only a short tree label"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("use update_plan with spine_plantree"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("spine_plantree.root.children"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("current spine_plantree"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("update spine_plantree first"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("task tree draft"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("model-authored input path"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("spine_plantree.children"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("root checkpoints"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("investigate/localize"));
}

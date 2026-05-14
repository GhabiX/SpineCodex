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
fn spine_view_instructions_keep_core_contract() {
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("<spine_view>"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("</spine_view>"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("runtime-generated worklog IR"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Use Spine effectively and efficiently"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("substantial raw history"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("future work is likely to reuse"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("raw details are still useful"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("coherent work scope is complete"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("spine_plantree"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("task tree draft"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("current editable scope"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("planning only"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("does not create Spine nodes"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("~<predicted-id>"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("task structure or next work scope changes"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("promptly refresh the current spine_plantree"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("displayed PlanTree stays current"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("around 50k raw tokens"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("every additional 30k"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Treat the warning as a cue"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("spine.next"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("spine.close"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("optional instruction argument"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Do not use summary or instruction with spine.open"));
    assert!(SPINE_VIEW_INSTRUCTIONS.contains("Completed Spine nodes are read-only"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("A node is a working scope, not a checklist item"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("The node summary is only a short tree label"));
    assert!(!SPINE_VIEW_INSTRUCTIONS.contains("investigate/localize"));
}

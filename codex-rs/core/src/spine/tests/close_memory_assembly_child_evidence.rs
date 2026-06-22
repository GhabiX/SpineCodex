use super::*;

#[test]
fn child_only_skeleton_does_not_invent_user_message() {
    let plan = source_plan(vec![child_memory_entry(
        2,
        0,
        "# Spine Memory 1.1.1\n\nchild exact\n",
    )]);
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

    let body = skeleton
        .assemble("preserved node memory facts")
        .expect("assembled body");
    assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
    assert!(body.contains("## Node Memory\npreserved node memory facts"));
    assert!(!body.contains("## Memory Slot"));
    assert!(!body.contains("## User Message"));
}

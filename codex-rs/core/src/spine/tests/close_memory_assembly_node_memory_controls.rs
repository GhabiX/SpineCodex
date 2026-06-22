use super::*;

#[test]
fn direct_node_memory_allows_standalone_body_control_tags_as_text() {
    let plan = source_plan(vec![source_entry(
        2,
        0,
        assistant_message("assistant details"),
        false,
    )]);
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

    let nested_slot_tag = skeleton
        .assemble("before\n<SPINE_SLOT_1>\nafter")
        .expect("standalone slot-like tag is opaque node memory text");
    assert!(nested_slot_tag.contains("## Node Memory\nbefore\n<SPINE_SLOT_1>\nafter"));

    let runtime_tag = skeleton
        .assemble("before\n<spine_memory>\nafter")
        .expect("standalone runtime-like tag is opaque node memory text");
    assert!(runtime_tag.contains("## Node Memory\nbefore\n<spine_memory>\nafter"));
}

#[test]
fn direct_node_memory_rejects_empty_and_allows_user_msg_text() {
    let plan = source_plan(vec![source_entry(
        2,
        0,
        assistant_message("assistant details"),
        false,
    )]);
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

    let empty_node = skeleton
        .assemble("  ")
        .expect_err("empty node memory must fail");
    assert!(empty_node.to_string().contains("empty node memory"));

    let user_msg = skeleton
        .assemble("before\n<USER_MSG_1>\ndo not return this\n</USER_MSG_1>\nafter")
        .expect("user-msg-like tags are opaque node memory text");
    assert!(user_msg.contains("## Node Memory\nbefore\n<USER_MSG_1>\ndo not return this"));
}

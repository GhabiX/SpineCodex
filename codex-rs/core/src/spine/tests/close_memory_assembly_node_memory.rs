use super::*;

#[test]
fn direct_node_memory_allows_markdown_content() {
    let plan = source_plan(vec![source_entry(
        2,
        0,
        assistant_message("assistant details"),
        false,
    )]);
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

    let body = skeleton
        .assemble(
            r#"Node memory can contain Markdown bullets:
- next step remains open

```json
{"quoted":"json is content, not protocol"}
```"#,
        )
        .expect("markdown node memory should be accepted");

    assert!(body.contains("## Node Memory\nNode memory can contain Markdown bullets"));
    assert!(body.contains(r#"{"quoted":"json is content, not protocol"}"#));
}

#[test]
fn direct_node_memory_allows_inline_protocol_marker_discussion() {
    let plan = source_plan(vec![source_entry(
        2,
        0,
        assistant_message("assistant details"),
        false,
    )]);
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

    let body = skeleton
        .assemble("The failure involved a literal <SPINE_SLOT_ substring in the node memory body.")
        .expect("inline protocol marker discussion should be accepted");

    assert!(body.contains("## Node Memory\nThe failure involved a literal <SPINE_SLOT_ substring"));
}

#[test]
fn direct_node_memory_treats_structure_markers_as_opaque_content() {
    let plan = source_plan(vec![source_entry(
        2,
        0,
        assistant_message("assistant details"),
        false,
    )]);
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");
    let body = skeleton
        .assemble("## User Message\nquoted historical text")
        .expect("node memory body is opaque text except for non-empty validation");

    assert!(body.contains("## Node Memory\n## User Message\nquoted historical text"));
}

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

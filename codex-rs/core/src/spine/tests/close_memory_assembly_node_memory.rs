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

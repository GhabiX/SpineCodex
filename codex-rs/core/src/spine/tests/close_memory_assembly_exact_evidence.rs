use super::*;

#[test]
fn skeleton_preserves_exact_user_child_and_direct_node_memory() {
    let plan = source_plan(vec![
        source_entry(2, 0, user_message("USER_EXACT\nline 2"), true),
        source_entry(3, 1, assistant_message("assistant details"), false),
        child_memory_entry(4, 2, "# Spine Memory 1.1.1\n\nchild body\n"),
    ]);
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

    let body = skeleton
        .assemble("node continuation facts")
        .expect("assembled body");
    assert!(body.contains("# Spine Memory 1.1"));
    assert!(body.contains("## User Message [U1]\nUSER_EXACT\nline 2"));
    assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild body"));
    assert!(body.contains("## Node Memory\nnode continuation facts"));
    assert!(!body.contains("assistant details"));
    assert!(!body.contains("## Memory Slot"));
}

#[test]
fn direct_close_memory_assembles_user_anchor_evidence() {
    let plan = source_plan(vec![
        source_entry_with_user_anchor(2, 0, user_message("[U7]\napprove"), true, Some(7)),
        source_entry(3, 1, assistant_message("tool detail"), false),
    ]);

    let compact = spine_close_memory_assembly_from_tool_arg(
        "1.1",
        &plan,
        "After [U7], implementation continued and tests passed.",
    )
    .expect("direct memory assembly");

    assert!(compact.body.contains("## User Message [U7]\n[U7]\napprove"));
    assert!(
        compact
            .body
            .contains("## Node Memory\nAfter [U7], implementation continued and tests passed.")
    );
    assert_eq!(compact.source_context_range, 2..4);
    assert_eq!(compact.source_raw_range, 2..4);
    assert_eq!(compact.memory_output_tokens, None);
}

#[test]
fn exact_only_skeleton_requires_node_memory() {
    let plan = source_plan(vec![
        source_entry_with_user_anchor(2, 0, user_message("only user one"), true, Some(1)),
        child_memory_entry(3, 1, "# Spine Memory 1.1.1\n\nchild exact\n"),
        source_entry_with_user_anchor(4, 2, user_message("only user two"), true, Some(2)),
    ]);
    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

    let body = skeleton
        .assemble("node memory for exact-only suffix")
        .expect("assembled body");
    assert!(body.contains("## User Message [U1]\nonly user one"));
    assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
    assert!(body.contains("## User Message [U2]\nonly user two"));
    assert!(!body.contains("## Memory Slot"));
    assert!(body.contains("## Node Memory\nnode memory for exact-only suffix"));
}

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

#[test]
fn multimodal_user_entry_is_preserved_as_runtime_text() {
    let plan = source_plan(vec![crate::spine::SpineCompactSourcePlanEntry {
        context_index: 2,
        source_ordinal: 0,
        source_hash: "hash-0".to_string(),
        kind: SpineCompactSourceEntryKind::RawResponseItem {
            item: ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![
                    ContentItem::InputText {
                        text: "text".to_string(),
                    },
                    ContentItem::InputText {
                        text: "second".to_string(),
                    },
                    ContentItem::InputImage {
                        image_url: "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR".to_string(),
                        detail: Some(codex_protocol::models::ImageDetail::High),
                    },
                ],
                phase: None,
            },
            raw_ordinal: 2,
            from_user: true,
            user_anchor: Some(1),
        },
    }]);

    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");
    let body = skeleton
        .assemble("node multimodal continuation")
        .expect("assembled body");
    assert!(body.contains("## User Message [U1]\ntext\nsecond\n<image omitted detail=high>"));
    assert!(body.contains("## Node Memory\nnode multimodal continuation"));
    assert!(!body.contains("RAW_IMAGE_SHOULD_NOT_APPEAR"));
}

use super::*;
use crate::spine::NodeId;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_NEXT;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use crate::spine::is_spine_close_like_tool_name;

#[path = "close_memory_assembly_node_memory.rs"]
mod close_memory_assembly_node_memory;
#[path = "close_memory_assembly_source_plan_validator.rs"]
mod close_memory_assembly_source_plan_validator;

pub(super) fn node_id(path: &[u32]) -> NodeId {
    serde_json::from_value(serde_json::json!(path)).expect("node id")
}

pub(super) fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

pub(super) fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

pub(super) fn source_entry(
    context_index: usize,
    source_ordinal: usize,
    item: ResponseItem,
    from_user: bool,
) -> crate::spine::SpineCompactSourcePlanEntry {
    source_entry_with_user_anchor(
        context_index,
        source_ordinal,
        item,
        from_user,
        from_user.then_some(1),
    )
}

pub(super) fn source_entry_with_user_anchor(
    context_index: usize,
    source_ordinal: usize,
    item: ResponseItem,
    from_user: bool,
    user_anchor: Option<u64>,
) -> crate::spine::SpineCompactSourcePlanEntry {
    crate::spine::SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash: format!("hash-{source_ordinal}"),
        kind: SpineCompactSourceEntryKind::RawResponseItem {
            item,
            raw_ordinal: u64::try_from(context_index).expect("context index fits u64"),
            from_user,
            user_anchor,
        },
    }
}

pub(super) fn child_memory_entry(
    context_index: usize,
    source_ordinal: usize,
    body: &str,
) -> crate::spine::SpineCompactSourcePlanEntry {
    crate::spine::SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash: format!("child-hash-{source_ordinal}"),
        kind: SpineCompactSourceEntryKind::ChildMemory {
            node_id: node_id(&[1, 1, 1]),
            compact_id: "mem-1-1-1".to_string(),
            source_raw_range: 2..3,
            body: body.to_string(),
            body_hash: "body-hash".to_string(),
        },
    }
}

pub(super) fn source_plan(
    entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
) -> SpineCompactSourcePlan {
    SpineCompactSourcePlan {
        node_id: node_id(&[1, 1]),
        source_context_range: 2..2 + entries.len(),
        source_raw_range: 2..2 + u64::try_from(entries.len()).expect("entries len fits u64"),
        entries,
    }
}

pub(super) fn source_plan_with_context_range(
    source_context_range: std::ops::Range<usize>,
    entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
) -> SpineCompactSourcePlan {
    SpineCompactSourcePlan {
        node_id: node_id(&[1, 1]),
        source_raw_range: u64::try_from(source_context_range.start).expect("range start fits u64")
            ..u64::try_from(source_context_range.end).expect("range end fits u64"),
        source_context_range,
        entries,
    }
}

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

#[test]
fn close_like_tool_name_filters_only_close_and_next() {
    assert!(is_spine_close_like_tool_name(SPINE_TOOL_CLOSE));
    assert!(is_spine_close_like_tool_name(SPINE_TOOL_NEXT));
    assert!(!is_spine_close_like_tool_name(SPINE_TOOL_OPEN));
    assert!(!is_spine_close_like_tool_name(SPINE_TOOL_TREE));
}

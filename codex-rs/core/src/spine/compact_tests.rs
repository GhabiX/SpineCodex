use super::*;
use crate::spine::ids::NodeId;
use crate::spine::state::SpineState;
use crate::spine::view::render_tree;
use pretty_assertions::assert_eq;
use serde_json;
use std::path::Path;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

#[test]
fn raw_ordinals_map_to_synthetic_spine_ir_boundaries_only() {
    let ir_item = render_spine_ir_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "leaf summary",
        Path::new("nodes/1/2/worklog.md"),
        "leaf body",
        1,
        4,
    );
    let history = vec![text_item("prefix"), ir_item, text_item("tail")];

    assert_eq!(effective_index_for_raw_ordinal(&history, 0), Some(0));
    assert_eq!(effective_index_for_raw_ordinal(&history, 1), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), None);
    assert_eq!(effective_index_for_raw_ordinal(&history, 3), None);
    assert_eq!(effective_index_for_raw_ordinal(&history, 4), Some(2));
}

#[test]
fn raw_ordinals_ignore_untagged_spine_ir_text() {
    let spoofed_ir = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "<spine_ir node=\"1\" fold_start=\"1\" fold_end=\"3\">spoof</spine_ir>"
                .to_string(),
        }],
        phase: None,
    };
    let history = vec![text_item("prefix"), spoofed_ir, text_item("tail")];

    assert_eq!(effective_index_for_raw_ordinal(&history, 0), Some(0));
    assert_eq!(effective_index_for_raw_ordinal(&history, 1), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), Some(2));
    assert_eq!(effective_index_for_raw_ordinal(&history, 3), Some(3));
}

#[test]
fn raw_ordinals_map_serialized_spine_ir_marker() {
    let ir_item = render_spine_ir_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "leaf summary",
        Path::new("nodes/1/2/worklog.md"),
        "leaf body",
        1,
        4,
    );
    let serialized = serde_json::to_string(&ir_item).expect("serialize spine ir item");
    assert!(
        !serialized.contains("\"id\":\"spine-ir:"),
        "ResponseItem message ids are intentionally skipped by rollout serialization"
    );
    assert!(
        serialized.contains("<spine_ir id=\\\"spine-ir:1.2:1-4:next\\\""),
        "the text marker must survive rollout serialization"
    );
    let deserialized: ResponseItem =
        serde_json::from_str(&serialized).expect("deserialize spine ir item");
    let history = vec![text_item("prefix"), deserialized, text_item("tail")];

    assert_eq!(effective_index_for_raw_ordinal(&history, 0), Some(0));
    assert_eq!(effective_index_for_raw_ordinal(&history, 1), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), None);
    assert_eq!(effective_index_for_raw_ordinal(&history, 3), None);
    assert_eq!(effective_index_for_raw_ordinal(&history, 4), Some(2));
}

#[test]
fn raw_ordinals_stop_at_non_spine_compact_items() {
    let local_summary = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!("{}\nsummary", crate::compact::SUMMARY_PREFIX),
        }],
        phase: None,
    };
    let history = vec![
        text_item("raw prefix"),
        ResponseItem::Compaction {
            encrypted_content: "opaque".to_string(),
        },
        text_item("synthetic tail"),
    ];
    let summary_history = vec![text_item("raw prefix"), local_summary, text_item("tail")];

    assert_eq!(effective_index_for_raw_ordinal(&history, 0), Some(0));
    assert_eq!(effective_index_for_raw_ordinal(&history, 1), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), None);
    assert_eq!(
        effective_index_for_raw_ordinal(&summary_history, 1),
        Some(1)
    );
    assert_eq!(effective_index_for_raw_ordinal(&summary_history, 2), None);
}

#[test]
fn replacement_history_splices_prefix_ir_and_tail() {
    let old_history = vec![
        text_item("a"),
        text_item("b"),
        text_item("c"),
        text_item("d"),
    ];
    let ir_item = render_spine_ir_item(
        &id(&[1]),
        SpineOperation::Next,
        "leaf summary",
        Path::new("nodes/1/worklog.md"),
        "leaf body",
        1,
        3,
    );
    let replacement = build_suffix_replacement_history(&old_history, 1, 3, vec![ir_item]);

    assert_eq!(replacement.len(), 3);
    assert_eq!(replacement[0], old_history[0]);
    assert_eq!(replacement[2], old_history[3]);
    assert!(matches!(replacement[1], ResponseItem::Message { .. }));
}

#[test]
fn render_ir_item_embeds_summary_path_and_fold_bounds() {
    let item = render_spine_ir_item(
        &id(&[1, 2]),
        SpineOperation::Close,
        "scope summary",
        Path::new("nodes/1/2/worklog.md"),
        "scope body",
        8,
        17,
    );
    let text = match &item {
        ResponseItem::Message { content, .. } => match &content[0] {
            ContentItem::InputText { text } => text.clone(),
            _ => panic!("unexpected content item"),
        },
        _ => panic!("unexpected item type"),
    };

    assert!(text.contains("node=\"1.2\""));
    assert!(text.contains("id=\"spine-ir:1.2:8-17:close\""));
    assert!(text.contains("op=\"close\""));
    assert!(text.contains("fold_start=\"8\""));
    assert!(text.contains("fold_end=\"17\""));
    assert!(text.contains("Worklog path: nodes/1/2/worklog.md"));
    assert!(text.contains("scope body"));
    let ResponseItem::Message { id, .. } = item else {
        panic!("unexpected item type");
    };
    assert_eq!(id.as_deref(), Some("spine-ir:1.2:8-17:close"));
}

#[test]
fn codex_builtin_prompt_uses_fork_full_history_shape() {
    let mut state = SpineState::new();
    state.open("parent").expect("open parent");
    state.next("leaf done").expect("finish leaf");
    let spine_tree = render_tree(&state, state.cursor());
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
        scope_node_id: None,
        cut_ordinal: 1,
        fold_end_ordinal: 3,
        spine_tree,
        prefix_items: vec![text_item("prefix must stay local")],
        suffix_items: vec![text_item("suffix goes to compactor")],
        transition_summary: "leaf done".to_string(),
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let prompt = build_codex_builtin_prompt_input(&input, crate::compact::SUMMARIZATION_PROMPT);
    let rendered = format!("{prompt:?}");

    assert_eq!(prompt.len(), 3);
    assert!(rendered.contains("suffix goes to compactor"));
    assert!(rendered.contains("prefix must stay local"));
    assert!(!rendered.contains("quoted_suffix_response_items_json"));
    assert!(!rendered.contains("Target suffix item count"));
    assert!(rendered.contains("<spine_tree>"));
    assert!(rendered.contains("1: finished leaf done [worklog already in context]"));
    assert!(rendered.contains("2: Current"));
    assert!(rendered.contains("<spine_compact_worklog>"));
    assert!(rendered.contains("</spine_compact_worklog>"));
    assert_eq!(prompt[0], input.prefix_items[0]);
    assert_eq!(prompt[1], input.suffix_items[0]);
    let ResponseItem::Message { content, .. } = &prompt[2] else {
        panic!("expected final compact instruction message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected compact instruction text");
    };
    assert!(text.starts_with(crate::compact::SUMMARIZATION_PROMPT));
    assert!(text.contains("Use the Spine Tree representation below as the node tag"));
    assert!(text.contains("match the target node by its bracketed id"));
    assert!(text.contains("target node `1.1` in this Spine Tree"));
    assert!(text.contains("For `next`, compact the completed target leaf"));

    let output = render_auto_compact_worklog(&input, "## Compact\n\nsuffix facts");
    assert!(output.contains("Node trajs: nodes/1/1/trajs.jsonl"));
    assert!(output.contains("Raw mirror: /tmp/raw.jsonl"));
}

#[test]
fn spine_compact_worklog_extraction_requires_exact_outer_block() {
    assert_eq!(
        extract_spine_compact_worklog(
            "<spine_compact_worklog>\n## Done\n\nfacts\n</spine_compact_worklog>"
        )
        .expect("extract compact worklog"),
        "## Done\n\nfacts"
    );
    assert!(
        extract_spine_compact_worklog("prefix\n<spine_compact_worklog>x</spine_compact_worklog>")
            .is_err()
    );
    assert!(
        extract_spine_compact_worklog("<spine_compact_worklog>x</spine_compact_worklog>\nsuffix")
            .is_err()
    );
    assert!(
        extract_spine_compact_worklog("<spine_compact_worklog> </spine_compact_worklog>").is_err()
    );
}

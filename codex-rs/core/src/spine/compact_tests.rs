use super::*;
use crate::spine::ids::NodeId;
use pretty_assertions::assert_eq;
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
fn raw_ordinals_map_through_synthetic_spine_ir_ranges() {
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
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 3), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 4), Some(2));
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
    let text = match item {
        ResponseItem::Message { content, .. } => match &content[0] {
            ContentItem::InputText { text } => text.clone(),
            _ => panic!("unexpected content item"),
        },
        _ => panic!("unexpected item type"),
    };

    assert!(text.contains("node=\"1.2\""));
    assert!(text.contains("op=\"close\""));
    assert!(text.contains("fold_start=\"8\""));
    assert!(text.contains("fold_end=\"17\""));
    assert!(text.contains("Worklog path: nodes/1/2/worklog.md"));
    assert!(text.contains("scope body"));
}

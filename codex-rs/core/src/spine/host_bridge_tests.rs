use super::*;
use crate::spine::compact::render_spine_handoff_item;
use crate::spine::compact::render_spine_initial_context_item;
use crate::spine::compact::render_spine_memory_item;
use crate::spine::ids::NodeId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

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

fn initial_context_item() -> ResponseItem {
    render_spine_initial_context_item(vec![ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "hydrated context".to_string(),
        }],
        phase: None,
    }])
    .expect("wrap initial context")
}

fn installed_span(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
) -> InstalledCompactSpan {
    InstalledCompactSpan {
        compact_id: compact_id.to_string(),
        node_id,
        op,
        cut_ordinal,
        fold_end_ordinal,
    }
}

fn mixed_projection_fixture() -> (Vec<ResponseItem>, Vec<InstalledCompactSpan>) {
    let spans = vec![
        installed_span("compact-a", id(&[1]), SpineOperation::Close, 1, 4),
        installed_span("compact-b", id(&[2]), SpineOperation::Close, 5, 8),
    ];
    let history = vec![
        text_item("raw 0"),
        render_spine_memory_item(&id(&[1]), SpineOperation::Close, "a", "a facts"),
        render_spine_handoff_item(&id(&[1]), &id(&[2])),
        text_item("raw 4"),
        render_spine_memory_item(&id(&[2]), SpineOperation::Close, "b", "b facts"),
        initial_context_item(),
        text_item("raw 8"),
    ];
    (history, spans)
}

fn reference_raw_for_effective_index_with_spans(
    history: &[ResponseItem],
    target_index: usize,
    runtime_spans: &[InstalledCompactSpan],
) -> Option<u64> {
    let mut raw_cursor = 0_u64;
    let mut span_cursor = 0_usize;
    for (index, item) in history.iter().enumerate() {
        if index == target_index {
            return Some(raw_cursor);
        }
        match classify_effective_item(item, raw_cursor, runtime_spans, &mut span_cursor)? {
            BridgeWidth::Raw1 => {
                raw_cursor = raw_cursor.checked_add(1)?;
            }
            BridgeWidth::Zero => {}
            BridgeWidth::Span { end, .. } => {
                raw_cursor = end;
            }
            BridgeWidth::Stop => return None,
        }
    }
    (target_index == history.len()).then_some(raw_cursor)
}

fn reference_effective_index_for_raw_ordinal_with_spans(
    history: &[ResponseItem],
    target_raw_ordinal: u64,
    runtime_spans: &[InstalledCompactSpan],
) -> Option<usize> {
    let mut raw_cursor = 0_u64;
    let mut span_cursor = 0_usize;
    for (index, item) in history.iter().enumerate() {
        match classify_effective_item(item, raw_cursor, runtime_spans, &mut span_cursor)? {
            BridgeWidth::Raw1 => {
                if raw_cursor == target_raw_ordinal {
                    return Some(index);
                }
                raw_cursor = raw_cursor.checked_add(1)?;
            }
            BridgeWidth::Zero => {}
            BridgeWidth::Span { start, end, .. } => {
                if target_raw_ordinal == start {
                    return Some(index);
                }
                if target_raw_ordinal > start && target_raw_ordinal < end {
                    return None;
                }
                raw_cursor = end;
            }
            BridgeWidth::Stop => {
                return (target_raw_ordinal == raw_cursor).then_some(index);
            }
        }
    }
    (target_raw_ordinal == raw_cursor).then_some(history.len())
}

#[test]
fn host_bridge_projection_matches_reference_raw_for_index() {
    let (history, spans) = mixed_projection_fixture();
    let projection = HostBridgeProjection::build(&history, &spans).expect("build projection");

    for index in 0..=history.len() + 1 {
        assert_eq!(
            projection.raw_for_effective_index(index),
            reference_raw_for_effective_index_with_spans(&history, index, &spans),
            "effective index {index}"
        );
    }
}

#[test]
fn host_bridge_projection_matches_reference_index_for_raw() {
    let (history, spans) = mixed_projection_fixture();
    let projection = HostBridgeProjection::build(&history, &spans).expect("build projection");

    for raw in 0..=10 {
        assert_eq!(
            projection.effective_index_for_raw_boundary(raw),
            reference_effective_index_for_raw_ordinal_with_spans(&history, raw, &spans),
            "raw boundary {raw}"
        );
    }
}

#[test]
fn host_bridge_projection_finds_first_span_in_prefix() {
    let (history, spans) = mixed_projection_fixture();
    let projection = HostBridgeProjection::build(&history, &spans).expect("build projection");

    assert_eq!(projection.first_span_in_prefix(0), None);
    assert_eq!(projection.first_span_in_prefix(1), None);
    assert_eq!(projection.first_span_in_prefix(2), Some((1, 1)));
    assert_eq!(projection.first_span_in_prefix(history.len()), Some((1, 1)));
}

#[test]
fn host_bridge_projection_returns_memory_item_for_span() {
    let (history, spans) = mixed_projection_fixture();
    let projection = HostBridgeProjection::build(&history, &spans).expect("build projection");

    assert_eq!(
        projection
            .memory_item_for_span("compact-b")
            .expect("compact-b memory"),
        history[4]
    );
    assert!(projection.memory_item_for_span("missing").is_err());
}

#[test]
fn host_bridge_projection_rejects_ambiguous_memory_carrier() {
    let memory_item =
        render_spine_memory_item(&id(&[1]), SpineOperation::Close, "summary", "memory body");
    let history = vec![text_item("raw 0"), memory_item];
    let spans = vec![
        installed_span("compact-a", id(&[1]), SpineOperation::Close, 1, 3),
        installed_span("compact-b", id(&[1]), SpineOperation::Close, 1, 3),
    ];

    assert!(HostBridgeProjection::build(&history, &spans).is_err());
}

#[test]
fn host_bridge_projection_treats_handoff_and_initial_context_as_zero() {
    let history = vec![
        text_item("raw 0"),
        render_spine_handoff_item(&id(&[1]), &id(&[2])),
        initial_context_item(),
        text_item("raw 1"),
    ];
    let projection = HostBridgeProjection::build(&history, &[]).expect("build projection");

    assert_eq!(projection.raw_len(), 2);
    assert_eq!(projection.raw_for_effective_index(1), Some(1));
    assert_eq!(projection.raw_for_effective_index(2), Some(1));
    assert_eq!(projection.effective_index_for_raw_boundary(1), Some(3));
}

#[test]
fn host_bridge_projection_stops_at_non_spine_compact() {
    let history = vec![
        text_item("raw prefix"),
        ResponseItem::Compaction {
            encrypted_content: "opaque".to_string(),
        },
        text_item("synthetic tail"),
    ];
    let projection = HostBridgeProjection::build(&history, &[]).expect("build projection");

    assert_eq!(projection.raw_len(), 1);
    assert_eq!(projection.effective_index_for_raw_boundary(1), Some(1));
    assert_eq!(projection.effective_index_for_raw_boundary(2), None);
    assert_eq!(projection.raw_for_effective_index(1), Some(1));
    assert_eq!(projection.raw_for_effective_index(2), None);
    assert_eq!(projection.raw_for_effective_index(history.len()), None);
}

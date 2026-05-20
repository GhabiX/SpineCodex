use super::*;
use crate::spine::checkpoint_render::render_spine_handoff_item;
use crate::spine::checkpoint_render::render_spine_memory_item;
use crate::spine::ids::NodeId;
use crate::spine::segment::RawSpan;
use crate::spine::store::InstalledCompactSpan;
use crate::spine::store::SpineOperation;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

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

fn user_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

#[test]
fn materializes_direct_raw_mem_and_note_segments() -> anyhow::Result<()> {
    let raw_items = vec![
        user_item("raw zero"),
        user_item("folded into memory"),
        user_item("raw two"),
    ];
    let pi = vec![
        Segment::Raw(RawSpan { start: 0, end: 1 }),
        Segment::Mem {
            compact_id: "compact-a".to_string(),
        },
        Segment::Note {
            kind: "note-a".to_string(),
        },
        Segment::Raw(RawSpan { start: 2, end: 3 }),
    ];
    let artifacts = BTreeMap::from([("compact-a".to_string(), RawSpan { start: 1, end: 2 })]);
    let mem_items = BTreeMap::from([("compact-a".to_string(), text_item("memory a"))]);
    let note_items = BTreeMap::from([("note-a".to_string(), vec![text_item("note a")])]);

    let rendered = materialize_spine_host_history(SpineHostMaterializationInput {
        pi: &pi,
        artifacts: &artifacts,
        raw_source: SpineHostRawSource::Direct(&raw_items),
        mem_items: &mem_items,
        note_items: &note_items,
    })?;
    let serialized = serde_json::to_string(&rendered)?;

    assert_eq!(rendered.len(), 4);
    assert!(serialized.contains("raw zero"));
    assert!(serialized.contains("memory a"));
    assert!(serialized.contains("note a"));
    assert!(serialized.contains("raw two"));
    assert!(!serialized.contains("folded into memory"));
    Ok(())
}

#[test]
fn materializes_host_bridge_raw_around_existing_mem_span() -> anyhow::Result<()> {
    let history = vec![
        user_item("raw zero"),
        render_spine_memory_item(
            &NodeId::from_segments(vec![1, 1]),
            SpineOperation::Close,
            "closed child",
            "child memory",
        ),
        render_spine_handoff_item(
            &NodeId::from_segments(vec![1, 1]),
            &NodeId::from_segments(vec![1]),
        ),
        user_item("raw three"),
    ];
    let runtime_spans = vec![InstalledCompactSpan {
        compact_id: "compact-child".to_string(),
        node_id: NodeId::from_segments(vec![1, 1]),
        op: SpineOperation::Close,
        cut_ordinal: 1,
        fold_end_ordinal: 3,
    }];
    let pi = vec![
        Segment::Raw(RawSpan { start: 0, end: 1 }),
        Segment::Mem {
            compact_id: "compact-new".to_string(),
        },
        Segment::Raw(RawSpan { start: 3, end: 4 }),
    ];
    let artifacts = BTreeMap::from([("compact-new".to_string(), RawSpan { start: 1, end: 3 })]);
    let mem_items = BTreeMap::from([("compact-new".to_string(), text_item("new memory"))]);
    let note_items = BTreeMap::new();

    let rendered = materialize_spine_host_history(SpineHostMaterializationInput {
        pi: &pi,
        artifacts: &artifacts,
        raw_source: SpineHostRawSource::HostBridge {
            history: &history,
            runtime_spans: &runtime_spans,
        },
        mem_items: &mem_items,
        note_items: &note_items,
    })?;
    let serialized = serde_json::to_string(&rendered)?;

    assert_eq!(rendered.len(), 3);
    assert!(serialized.contains("raw zero"));
    assert!(serialized.contains("new memory"));
    assert!(serialized.contains("raw three"));
    assert!(!serialized.contains("child memory"));
    Ok(())
}

#[test]
fn rejects_pi_that_does_not_cover_raw_source() {
    let raw_items = vec![user_item("raw zero"), user_item("raw one")];
    let pi = vec![Segment::Raw(RawSpan { start: 0, end: 1 })];
    let artifacts = BTreeMap::new();
    let mem_items = BTreeMap::new();
    let note_items = BTreeMap::new();

    let error = materialize_spine_host_history(SpineHostMaterializationInput {
        pi: &pi,
        artifacts: &artifacts,
        raw_source: SpineHostRawSource::Direct(&raw_items),
        mem_items: &mem_items,
        note_items: &note_items,
    })
    .expect_err("incomplete Pi must not materialize host history");

    assert!(
        error
            .to_string()
            .contains("Pi covers raw length 1, expected 2")
    );
}

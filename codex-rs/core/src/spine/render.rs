use crate::spine::SpineError;
use crate::spine::io::sha1_hex;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::SegRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::parse_stack::ParseStack;
use codex_protocol::models::ResponseItem;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(super) struct VisibleItemRef {
    pub(super) context_index: usize,
    pub(super) source: VisibleItemSource,
}

#[derive(Clone, Debug)]
pub(super) enum VisibleItemSource {
    RawResponseItem {
        raw_ordinal: u64,
        from_user: bool,
    },
    ToolCallSegment {
        kind: ToolCallSegmentKind,
        raw_ordinal: u64,
    },
    MemoryRef {
        memory: MemoryRef,
        require_live_raw: bool,
    },
    MemorySeg {
        memory_id: String,
        body_path: PathBuf,
    },
}

pub(super) fn render_parse_stack_to_context(
    ps: &ParseStack,
    raw_items: &[Option<ResponseItem>],
) -> Result<Vec<ResponseItem>, SpineError> {
    render_parse_stack_to_context_with_memory_body(ps, raw_items, None)
}

pub(super) fn render_parse_stack_to_context_with_memory_body(
    ps: &ParseStack,
    raw_items: &[Option<ResponseItem>],
    staged_memory_body: Option<(&str, &str)>,
) -> Result<Vec<ResponseItem>, SpineError> {
    let visible_refs = project_parse_stack_visible_items(ps)?;
    let mut out = Vec::with_capacity(visible_refs.len());
    for visible_ref in &visible_refs {
        render_visible_ref_to_context(
            &visible_ref.source,
            raw_items,
            staged_memory_body,
            &mut out,
        )?;
    }
    Ok(out)
}

pub(super) fn project_parse_stack_visible_items(
    ps: &ParseStack,
) -> Result<Vec<VisibleItemRef>, SpineError> {
    let mut refs = Vec::new();
    let mut next_context_index = 0usize;
    project_symbols_to_visible_refs(&ps.symbols, &mut next_context_index, &mut refs)?;
    Ok(refs)
}

pub(super) fn project_spine_tree_nodes_visible_items(
    nodes: &[SpineTreeNode],
    context_start: usize,
) -> Result<Vec<VisibleItemRef>, SpineError> {
    let mut refs = Vec::new();
    let mut next_context_index = context_start;
    for node in nodes {
        project_node_to_visible_refs(node, &mut next_context_index, &mut refs)?;
    }
    Ok(refs)
}

fn project_symbols_to_visible_refs(
    symbols: &[Symbol],
    next_context_index: &mut usize,
    refs: &mut Vec<VisibleItemRef>,
) -> Result<(), SpineError> {
    for symbol in symbols {
        match symbol {
            Symbol::Control(ControlSymbol::Init(_))
            | Symbol::Control(ControlSymbol::End)
            | Symbol::Control(ControlSymbol::Open(_))
            | Symbol::Control(ControlSymbol::Close(_))
            | Symbol::Control(ControlSymbol::Compact(_, _, _, _)) => {}
            Symbol::SpineTreeNode(node) => {
                project_node_to_visible_refs(node, next_context_index, refs)?
            }
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    project_node_to_visible_refs(node, next_context_index, refs)?;
                }
            }
            Symbol::RootEpoches(root_epochs) => {
                if let Some(root_epoch) = root_epochs.last() {
                    push_visible_ref(
                        next_context_index,
                        refs,
                        VisibleItemSource::MemoryRef {
                            memory: root_epoch.memory.clone(),
                            require_live_raw: false,
                        },
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn project_node_to_visible_refs(
    node: &SpineTreeNode,
    next_context_index: &mut usize,
    refs: &mut Vec<VisibleItemRef>,
) -> Result<(), SpineError> {
    match node {
        SpineTreeNode::MsgAsLeafNode { msg, from_user } => match msg {
            SegRef::ResponseItem { raw_ordinal, .. } => push_visible_ref(
                next_context_index,
                refs,
                VisibleItemSource::RawResponseItem {
                    raw_ordinal: *raw_ordinal,
                    from_user: *from_user,
                },
            ),
            SegRef::Memory {
                memory_id,
                body_path,
            } => push_visible_ref(
                next_context_index,
                refs,
                VisibleItemSource::MemorySeg {
                    memory_id: memory_id.clone(),
                    body_path: body_path.clone(),
                },
            ),
        },
        SpineTreeNode::ToolCallAsLeafNode { segments } => {
            for segment in segments {
                let SegRef::ResponseItem { raw_ordinal, .. } = &segment.seg else {
                    return Err(SpineError::InvalidEvent(
                        "visible toolcall segment must reference raw response item".to_string(),
                    ));
                };
                push_visible_ref(
                    next_context_index,
                    refs,
                    VisibleItemSource::ToolCallSegment {
                        kind: segment.kind,
                        raw_ordinal: *raw_ordinal,
                    },
                )?;
            }
            Ok(())
        }
        SpineTreeNode::SpineTree { memory, .. } => push_visible_ref(
            next_context_index,
            refs,
            VisibleItemSource::MemoryRef {
                memory: memory.clone(),
                require_live_raw: true,
            },
        ),
    }
}

fn push_visible_ref(
    next_context_index: &mut usize,
    refs: &mut Vec<VisibleItemRef>,
    source: VisibleItemSource,
) -> Result<(), SpineError> {
    let context_index = *next_context_index;
    *next_context_index = next_context_index
        .checked_add(1)
        .ok_or_else(|| SpineError::InvalidEvent("visible context index overflow".to_string()))?;
    refs.push(VisibleItemRef {
        context_index,
        source,
    });
    Ok(())
}

fn render_visible_ref_to_context(
    source: &VisibleItemSource,
    raw_items: &[Option<ResponseItem>],
    staged_memory_body: Option<(&str, &str)>,
    out: &mut Vec<ResponseItem>,
) -> Result<(), SpineError> {
    match source {
        VisibleItemSource::RawResponseItem { raw_ordinal, .. }
        | VisibleItemSource::ToolCallSegment { raw_ordinal, .. } => {
            let raw_index = usize::try_from(*raw_ordinal)
                .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
            let item = raw_items
                .get(raw_index)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    let missing_label = match source {
                        VisibleItemSource::RawResponseItem { .. } => "visible Msg",
                        VisibleItemSource::ToolCallSegment { .. } => "visible toolcall segment",
                        _ => unreachable!("raw source label requested for non-raw source"),
                    };
                    SpineError::InvalidEvent(format!(
                        "missing raw item for {missing_label} raw ordinal {raw_ordinal}"
                    ))
                })?;
            out.push(item.clone());
            Ok(())
        }
        VisibleItemSource::MemoryRef {
            memory,
            require_live_raw,
        } => {
            if *require_live_raw && !memory_ref_is_live(memory, raw_items)? {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    memory.compact_id
                )));
            }
            let body = read_memory_ref_body_with_staged(memory, staged_memory_body)?;
            out.push(memory_response_item(&body));
            Ok(())
        }
        VisibleItemSource::MemorySeg {
            memory_id,
            body_path,
        } => {
            let body = read_memory_body(memory_id, body_path, None)?;
            out.push(memory_response_item(&body));
            Ok(())
        }
    }
}

pub(super) fn read_memory_ref_body(memory: &MemoryRef) -> Result<String, SpineError> {
    read_memory_ref_body_with_staged(memory, None)
}

fn read_memory_ref_body_with_staged(
    memory: &MemoryRef,
    staged_memory_body: Option<(&str, &str)>,
) -> Result<String, SpineError> {
    if let Some((memory_id, body)) = staged_memory_body {
        if memory_id == memory.compact_id {
            let actual_hash = sha1_hex(body.as_bytes());
            if actual_hash != memory.body_hash {
                return Err(SpineError::InvalidStore(format!(
                    "staged memory body hash mismatch for {memory_id}"
                )));
            }
            return Ok(body.to_string());
        }
    }
    read_memory_body(
        &memory.compact_id,
        &memory.body_path,
        Some(memory.body_hash.as_str()),
    )
}

pub(super) fn read_memory_body(
    memory_id: &str,
    body_path: &std::path::Path,
    expected_hash: Option<&str>,
) -> Result<String, SpineError> {
    let body = std::fs::read_to_string(body_path)?;
    if let Some(expected_hash) = expected_hash {
        let actual_hash = sha1_hex(body.as_bytes());
        if actual_hash != expected_hash {
            return Err(SpineError::InvalidStore(format!(
                "memory body hash mismatch for {memory_id}"
            )));
        }
    }
    Ok(body)
}

fn memory_ref_is_live(
    memory: &MemoryRef,
    raw_items: &[Option<ResponseItem>],
) -> Result<bool, SpineError> {
    let start = usize::try_from(memory.source_raw_range.start)
        .map_err(|_| SpineError::InvalidEvent("memory raw start overflow".to_string()))?;
    let end = usize::try_from(memory.source_raw_range.end)
        .map_err(|_| SpineError::InvalidEvent("memory raw end overflow".to_string()))?;
    if start > end || end > raw_items.len() {
        return Ok(false);
    }
    Ok(raw_items[start..end].iter().all(Option::is_some))
}

pub(super) fn memory_response_item(body: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![codex_protocol::models::ContentItem::InputText {
            text: format!("<spine_memory>\n{body}\n</spine_memory>"),
        }],
        phase: None,
    }
}

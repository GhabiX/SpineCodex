use crate::spine::SpineError;
use crate::spine::io::sha1_hex;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::SegRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::parse_stack::ParseStack;
use codex_protocol::models::ResponseItem;

pub(super) fn render_parse_stack_to_context(
    ps: &ParseStack,
    raw_items: &[Option<ResponseItem>],
) -> Result<Vec<ResponseItem>, SpineError> {
    let mut out = Vec::new();
    render_symbols_to_context(&ps.symbols, raw_items, &mut out)?;
    Ok(out)
}

fn render_symbols_to_context(
    symbols: &[Symbol],
    raw_items: &[Option<ResponseItem>],
    out: &mut Vec<ResponseItem>,
) -> Result<(), SpineError> {
    for symbol in symbols {
        match symbol {
            Symbol::Control(ControlSymbol::Init(_))
            | Symbol::Control(ControlSymbol::Open(_))
            | Symbol::Control(ControlSymbol::Close(_))
            | Symbol::Control(ControlSymbol::Compact(_, _)) => {}
            Symbol::SpineTreeNode(node) => render_node_to_context(node, raw_items, out)?,
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    render_node_to_context(node, raw_items, out)?;
                }
            }
            Symbol::RootEpoches(root_epochs) => {
                if let Some(root_epoch) = root_epochs.last() {
                    out.push(memory_response_item(&read_memory_ref_body(
                        &root_epoch.memory,
                    )?));
                }
            }
        }
    }
    Ok(())
}

fn render_node_to_context(
    node: &SpineTreeNode,
    raw_items: &[Option<ResponseItem>],
    out: &mut Vec<ResponseItem>,
) -> Result<(), SpineError> {
    match node {
        SpineTreeNode::MsgAsLeafNode {
            msg:
                SegRef::ResponseItem {
                    raw_ordinal,
                    context_index: _,
                },
            ..
        } => {
            let raw_index = usize::try_from(*raw_ordinal)
                .map_err(|_| SpineError::InvalidEvent("raw ordinal overflow".to_string()))?;
            let item = raw_items
                .get(raw_index)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    SpineError::InvalidEvent(format!(
                        "missing raw item for visible Msg raw ordinal {raw_ordinal}"
                    ))
                })?;
            out.push(item.clone());
            Ok(())
        }
        SpineTreeNode::SpineTree { memory, .. } => {
            if memory_ref_is_live(memory, raw_items)? {
                out.push(memory_response_item(&read_memory_ref_body(memory)?));
            } else {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    memory.compact_id
                )));
            }
            Ok(())
        }
    }
}

pub(super) fn read_memory_ref_body(memory: &MemoryRef) -> Result<String, SpineError> {
    let body = std::fs::read_to_string(&memory.body_path)?;
    if sha1_hex(body.as_bytes()) != memory.body_hash {
        return Err(SpineError::InvalidStore(format!(
            "memory body hash mismatch for {}",
            memory.compact_id
        )));
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
            text: format!("<spine_memory runtime_generated=\"true\">\n{body}\n</spine_memory>"),
        }],
        phase: None,
    }
}

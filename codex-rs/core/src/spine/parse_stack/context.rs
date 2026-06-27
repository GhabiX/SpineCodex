use crate::spine::SpineError;
use crate::spine::model::SegRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;

pub(super) fn validate_shifted_symbol_context_indices(
    previous_visible_context_index: Option<usize>,
    symbol: &Symbol,
) -> Result<(), SpineError> {
    let mut previous = previous_visible_context_index;
    for context_index in symbol_response_context_indices(symbol) {
        if let Some(previous_context_index) = previous
            && context_index <= previous_context_index
        {
            return Err(SpineError::InvalidEvent(format!(
                "spine parse stack visible context_index {context_index} is not strictly after previous visible context_index {previous_context_index}"
            )));
        }
        previous = Some(context_index);
    }
    Ok(())
}

pub(super) fn symbol_response_context_indices(symbol: &Symbol) -> Vec<usize> {
    symbol_response_context_refs(symbol)
        .into_iter()
        .map(|(_, context_index)| context_index)
        .collect()
}

pub(super) fn symbol_response_context_refs(symbol: &Symbol) -> Vec<(u64, usize)> {
    let mut out = Vec::new();
    collect_symbol_response_context_refs(symbol, &mut out);
    out
}

fn collect_symbol_response_context_refs(symbol: &Symbol, out: &mut Vec<(u64, usize)>) {
    match symbol {
        Symbol::Control(_) | Symbol::RootEpoches(_) => {}
        Symbol::SpineTreeNode(node) => collect_node_response_context_refs(node, out),
        Symbol::SpineTreeNodes(nodes) => {
            for node in nodes {
                collect_node_response_context_refs(node, out);
            }
        }
    }
}

fn collect_node_response_context_refs(node: &SpineTreeNode, out: &mut Vec<(u64, usize)>) {
    match node {
        SpineTreeNode::MsgAsLeafNode { msg, .. } => {
            collect_seg_ref_response_context_ref(msg, out);
        }
        SpineTreeNode::ToolCallAsLeafNode { segments } => {
            for segment in segments {
                collect_seg_ref_response_context_ref(&segment.seg, out);
            }
        }
        SpineTreeNode::SpineTree { .. } => {}
    }
}

fn collect_seg_ref_response_context_ref(seg: &SegRef, out: &mut Vec<(u64, usize)>) {
    if let SegRef::ResponseItem {
        raw_ordinal,
        context_index,
    } = seg
    {
        out.push((*raw_ordinal, *context_index));
    }
}

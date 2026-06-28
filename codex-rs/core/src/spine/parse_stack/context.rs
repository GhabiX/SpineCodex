use crate::spine::SpineError;
use crate::spine::model::SegRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;

#[derive(Debug, Default)]
struct VisibleContextTail {
    next_context_index: usize,
    last_visible_context_index: Option<usize>,
}

pub(super) fn validate_shifted_symbol_context_indices(
    previous_visible_context_index: Option<usize>,
    symbol: &Symbol,
) -> Result<(), SpineError> {
    let mut previous = previous_visible_context_index;
    for (_, context_index) in symbol_response_context_refs(symbol) {
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

pub(super) fn current_visible_tail_context_index(symbols: &[Symbol]) -> Option<usize> {
    let mut tail = VisibleContextTail::default();
    for symbol in symbols {
        collect_symbol_visible_tail(symbol, &mut tail);
    }
    tail.last_visible_context_index
}

pub(super) fn symbol_response_context_refs(symbol: &Symbol) -> Vec<(u64, usize)> {
    let mut out = Vec::new();
    collect_symbol_response_context_refs(symbol, &mut out);
    out
}

fn collect_symbol_visible_tail(symbol: &Symbol, tail: &mut VisibleContextTail) {
    match symbol {
        Symbol::Control(_) => {}
        Symbol::RootEpoches(root_epochs) => {
            if !root_epochs.is_empty() {
                tail.push_visible_item();
            }
        }
        Symbol::SpineTreeNode(node) => collect_node_visible_tail(node, tail),
        Symbol::SpineTreeNodes(nodes) => {
            for node in nodes {
                collect_node_visible_tail(node, tail);
            }
        }
    }
}

fn collect_node_visible_tail(node: &SpineTreeNode, tail: &mut VisibleContextTail) {
    match node {
        SpineTreeNode::MsgAsLeafNode { msg, .. } => {
            collect_seg_ref_visible_tail(msg, tail);
        }
        SpineTreeNode::ToolCallAsLeafNode { segments } => {
            for segment in segments {
                collect_seg_ref_visible_tail(&segment.seg, tail);
            }
        }
        SpineTreeNode::SpineTree { .. } => tail.push_visible_item(),
    }
}

fn collect_seg_ref_visible_tail(seg: &SegRef, tail: &mut VisibleContextTail) {
    match seg {
        SegRef::ResponseItem { .. } | SegRef::Memory { .. } => tail.push_visible_item(),
    }
}

impl VisibleContextTail {
    fn push_visible_item(&mut self) {
        let context_index = self.next_context_index;
        self.next_context_index = self
            .next_context_index
            .checked_add(1)
            .expect("visible context index overflow");
        self.last_visible_context_index = Some(context_index);
    }
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

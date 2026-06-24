use super::Symbol;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::SpineTreeNode;
use std::collections::BTreeMap;

pub(super) fn apply_memory_context_accounting_to_symbol(
    symbol: &mut Symbol,
    accounting: &BTreeMap<String, i64>,
) {
    match symbol {
        Symbol::Control(ControlSymbol::Close(memory))
        | Symbol::Control(ControlSymbol::Compact(memory, _, _, _)) => {
            apply_memory_context_accounting_to_memory(memory, accounting);
        }
        Symbol::Control(ControlSymbol::Init(_))
        | Symbol::Control(ControlSymbol::End)
        | Symbol::Control(ControlSymbol::Open(_)) => {}
        Symbol::SpineTreeNode(node) => {
            apply_memory_context_accounting_to_node(node, accounting);
        }
        Symbol::SpineTreeNodes(nodes) => {
            for node in nodes {
                apply_memory_context_accounting_to_node(node, accounting);
            }
        }
        Symbol::RootEpoches(root_epochs) => {
            for root_epoch in root_epochs {
                apply_memory_context_accounting_to_memory(&mut root_epoch.memory, accounting);
            }
        }
    }
}

fn apply_memory_context_accounting_to_node(
    node: &mut SpineTreeNode,
    accounting: &BTreeMap<String, i64>,
) {
    match node {
        SpineTreeNode::MsgAsLeafNode { .. } | SpineTreeNode::ToolCallAsLeafNode { .. } => {}
        SpineTreeNode::SpineTree {
            memory, children, ..
        } => {
            apply_memory_context_accounting_to_memory(memory, accounting);
            for child in children {
                apply_memory_context_accounting_to_node(child, accounting);
            }
        }
    }
}

fn apply_memory_context_accounting_to_memory(
    memory: &mut MemoryRef,
    accounting: &BTreeMap<String, i64>,
) {
    if memory.closed_memory_context_tokens.is_none() {
        memory.closed_memory_context_tokens = accounting.get(&memory.compact_id).copied();
    }
}

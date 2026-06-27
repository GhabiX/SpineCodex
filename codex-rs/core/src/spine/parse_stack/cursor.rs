use super::ControlSymbol;
use super::ParseStack;
use super::Symbol;
use super::tree;
use crate::spine::SpineError;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::NodeId;
use crate::spine::model::TreeMeta;

impl ParseStack {
    pub(in crate::spine) fn current_open_meta(&self) -> Result<&TreeMeta, SpineError> {
        self.current_open_meta_opt()
            .ok_or_else(|| SpineError::InvalidEvent("ParseStack has no live Open".to_string()))
    }

    pub(in crate::spine) fn current_open_meta_opt(&self) -> Option<&TreeMeta> {
        self.symbols.iter().rev().find_map(|symbol| match symbol {
            Symbol::Control(ControlSymbol::Open(meta)) => Some(meta),
            _ => None,
        })
    }

    pub(in crate::spine) fn live_open_metas(&self) -> Vec<&TreeMeta> {
        self.symbols
            .iter()
            .filter_map(|symbol| match symbol {
                Symbol::Control(ControlSymbol::Open(meta)) => Some(meta),
                _ => None,
            })
            .collect()
    }

    pub(in crate::spine) fn set_live_open_context_baseline(
        &mut self,
        node: &NodeId,
        provider_input_tokens: i64,
        source: ContextBaselineSource,
    ) -> Result<bool, SpineError> {
        let Some(meta) = self
            .symbols
            .iter_mut()
            .rev()
            .find_map(|symbol| match symbol {
                Symbol::Control(ControlSymbol::Open(meta)) if &meta.id == node => Some(meta),
                _ => None,
            })
        else {
            return Ok(false);
        };
        match (
            meta.open_input_tokens,
            meta.open_context_tokens,
            meta.open_context_source,
        ) {
            (None, None, None) => {
                meta.open_input_tokens = Some(provider_input_tokens);
                meta.open_context_tokens = Some(provider_input_tokens);
                meta.open_context_source = Some(source);
                Ok(true)
            }
            (Some(existing_input), Some(existing_context), Some(existing_source))
                if existing_input == provider_input_tokens
                    && existing_context == provider_input_tokens
                    && existing_source == source =>
            {
                Ok(false)
            }
            _ => Err(SpineError::InvalidEvent(format!(
                "open context baseline for node {node} is already set"
            ))),
        }
    }

    pub(in crate::spine) fn current_open_has_nodes(&self) -> Result<bool, SpineError> {
        let open_idx = self
            .symbols
            .iter()
            .rposition(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Open(_))))
            .ok_or_else(|| SpineError::InvalidEvent("ParseStack has no live Open".to_string()))?;
        Ok(self.symbols[open_idx + 1..]
            .iter()
            .any(|symbol| matches!(symbol, Symbol::SpineTreeNodes(nodes) if !nodes.is_empty())))
    }

    pub(in crate::spine) fn current_root_epoch_id(&self) -> Result<NodeId, SpineError> {
        self.current_cursor_id()?
            .0
            .first()
            .copied()
            .map(NodeId::root_epoch)
            .ok_or_else(|| SpineError::InvalidEvent("current node id is empty".to_string()))
    }

    pub(in crate::spine) fn current_cursor_id(&self) -> Result<NodeId, SpineError> {
        if let Some(open) = self.current_open_meta_opt() {
            return Ok(open.id.clone());
        }
        for symbol in self.symbols.iter().rev() {
            match symbol {
                Symbol::SpineTreeNodes(nodes) => {
                    if let Some(root) = tree::root_epoch_from_nodes(nodes) {
                        return Ok(root);
                    }
                }
                Symbol::SpineTreeNode(node) => {
                    if let Some(root) = tree::root_epoch_from_node(node) {
                        return Ok(root);
                    }
                }
                Symbol::RootEpoches(root_epochs) => {
                    let next = root_epochs
                        .last()
                        .and_then(|root_epoch| root_epoch.memory.node_id.0.first().copied())
                        .and_then(|root| root.checked_add(1))
                        .ok_or_else(|| {
                            SpineError::InvalidEvent(
                                "current root epoch id is unavailable".to_string(),
                            )
                        })?;
                    return Ok(NodeId::root_epoch(next));
                }
                Symbol::Control(ControlSymbol::Init(meta)) => return Ok(meta.id.clone()),
                Symbol::Control(_) => {}
            }
        }
        Err(SpineError::InvalidEvent(
            "ParseStack has no cursor".to_string(),
        ))
    }

    pub(in crate::spine) fn next_child_id(&self) -> Result<NodeId, SpineError> {
        tree::next_child_id(self)
    }
}

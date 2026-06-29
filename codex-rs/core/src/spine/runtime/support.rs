use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

use super::SPINE_NAMESPACE;
use super::SPINE_TOOL_CLOSE;
use super::SPINE_TOOL_NEXT;
use super::SPINE_TOOL_OPEN;
#[cfg(test)]
use super::SPINE_TOOL_TREE;
use super::SpineError;
use crate::context::ContextualUserFragment;
use crate::context::TurnAborted;
use crate::context::is_contextual_user_fragment;
use crate::spine::model::COMMIT_MARKER_VERSION;
use crate::spine::model::MemRecord;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineCommitMemoryRef;
use crate::spine::model::SpineLedgerEvent;
use codex_protocol::protocol::ENVIRONMENT_CONTEXT_CLOSE_TAG;
use codex_protocol::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;

pub(crate) fn is_spine_context_observation_fixed_prefix_item(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    match role.as_str() {
        "developer" => true,
        "user" => {
            content.iter().any(is_contextual_user_fragment)
                && !content
                    .iter()
                    .any(is_spine_runtime_contextual_user_fragment)
        }
        _ => false,
    }
}

fn is_spine_mutable_context_item(item: &ResponseItem) -> bool {
    !is_spine_context_observation_fixed_prefix_item(item)
}

pub(super) struct HostHistoryLens<'a> {
    history: &'a [ResponseItem],
}

impl<'a> HostHistoryLens<'a> {
    pub(super) fn new(history: &'a [ResponseItem]) -> Self {
        Self { history }
    }

    pub(super) fn mutable_len(&self) -> usize {
        self.history
            .iter()
            .filter(|item| is_spine_mutable_context_item(item))
            .count()
    }

    pub(super) fn is_fixed_prefix(&self, index: usize) -> Result<bool, SpineError> {
        let item = self.history.get(index).ok_or_else(|| {
            SpineError::CompactFailure(format!(
                "full host index {} exceeds host history length {}",
                index,
                self.history.len()
            ))
        })?;
        Ok(is_spine_context_observation_fixed_prefix_item(item))
    }

    pub(super) fn mutable_index_for_full_index(&self, index: usize) -> Result<usize, SpineError> {
        if self.is_fixed_prefix(index)? {
            return Err(SpineError::CompactFailure(format!(
                "full host index {} is fixed prefix and has no mutable context index",
                index
            )));
        }
        Ok(self
            .history
            .iter()
            .take(index)
            .filter(|item| is_spine_mutable_context_item(item))
            .count())
    }

    pub(super) fn mutable_index_for_full_boundary(
        &self,
        index: usize,
    ) -> Result<usize, SpineError> {
        if index > self.history.len() {
            return Err(SpineError::CompactFailure(format!(
                "full host boundary {} exceeds host history length {}",
                index,
                self.history.len()
            )));
        }
        Ok(self
            .history
            .iter()
            .take(index)
            .filter(|item| is_spine_mutable_context_item(item))
            .count())
    }

    pub(super) fn full_index_for_mutable_index(&self, index: usize) -> Result<usize, SpineError> {
        self.history
            .iter()
            .enumerate()
            .filter(|(_, item)| is_spine_mutable_context_item(item))
            .nth(index)
            .map(|(index, _)| index)
            .ok_or_else(|| {
                SpineError::CompactFailure(format!(
                    "spine mutable context_index {} exceeds mutable history length {}",
                    index,
                    self.mutable_len()
                ))
            })
    }

    pub(super) fn full_index_for_mutable_boundary(
        &self,
        index: usize,
    ) -> Result<usize, SpineError> {
        if index == self.mutable_len() {
            return Ok(self.history.len());
        }
        self.full_index_for_mutable_index(index)
    }

    pub(super) fn raw_item_for_mutable_index(
        &self,
        index: usize,
    ) -> Result<&'a ResponseItem, SpineError> {
        let full_index = self.full_index_for_mutable_index(index)?;
        self.history.get(full_index).ok_or_else(|| {
            SpineError::CompactFailure(format!(
                "spine mutable context_index {} mapped to missing host history index {}",
                index, full_index
            ))
        })
    }
}

pub(crate) fn spine_mutable_context_index_for_full_history_index(
    history: &[ResponseItem],
    full_history_index: usize,
) -> Result<usize, SpineError> {
    HostHistoryLens::new(history).mutable_index_for_full_index(full_history_index)
}

pub(crate) fn spine_mutable_context_index_for_full_history_boundary(
    history: &[ResponseItem],
    full_history_index: usize,
) -> Result<usize, SpineError> {
    HostHistoryLens::new(history).mutable_index_for_full_boundary(full_history_index)
}

fn is_spine_runtime_contextual_user_fragment(content_item: &ContentItem) -> bool {
    matches!(content_item, ContentItem::InputText { text } if {
        TurnAborted::matches_text(text) || {
            let mut lines = text.trim().lines().map(str::trim);
            matches!(
                (lines.next(), lines.next(), lines.next(), lines.next()),
                (Some(open), Some(cwd), Some(close), None)
                    if open == ENVIRONMENT_CONTEXT_OPEN_TAG
                        && cwd.starts_with("<cwd>")
                        && cwd.ends_with("</cwd>")
                        && close == ENVIRONMENT_CONTEXT_CLOSE_TAG
            )
        }
    })
}

pub(super) fn tool_request_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCall { call_id, .. }
        | ResponseItem::CustomToolCall { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::ToolSearchCall {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

pub(super) fn tool_response_call_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_str()),
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            ..
        } => Some(call_id.as_str()),
        _ => None,
    }
}

pub(crate) fn is_non_toolcall_msg(item: &ResponseItem) -> bool {
    tool_request_call_id(item).is_none()
        && tool_response_call_id(item).is_none()
        && !matches!(
            item,
            ResponseItem::ToolSearchOutput { call_id: None, .. }
                | ResponseItem::ToolSearchCall { call_id: None, .. }
        )
}

pub(super) fn validate_model_node_memory(memory: &str) -> Result<(), SpineError> {
    if memory.trim().is_empty() {
        return Err(SpineError::ToolUse(
            "spine.close/next memory must not be empty".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn close_event_boundary(event: &SpineLedgerEvent) -> Result<u64, SpineError> {
    match event {
        SpineLedgerEvent::Close { boundary, .. } => Ok(*boundary),
        _ => Err(SpineError::Invariant(
            "close commit marker requested for non-close event".to_string(),
        )),
    }
}

pub(super) fn close_commit_marker(
    seq: u64,
    mem: &MemRecord,
    kind: SpineCommitKindMarker,
    raw_boundary: u64,
    width: u64,
) -> Result<SpineCommitMarker, SpineError> {
    if kind == SpineCommitKindMarker::RootCompact {
        return Err(SpineError::Invariant(
            "root compact marker requested from close marker builder".to_string(),
        ));
    }
    commit_marker(
        format!("{}:{}", commit_marker_kind_label(kind), mem.compact_id),
        kind,
        seq,
        width,
        raw_boundary,
        None,
        mem,
    )
}

pub(super) fn root_compact_commit_marker(
    seq: u64,
    mem: &MemRecord,
) -> Result<SpineCommitMarker, SpineError> {
    commit_marker(
        format!("root_compact:{}", mem.compact_id),
        SpineCommitKindMarker::RootCompact,
        seq,
        1,
        mem.raw_end,
        mem.raw_live_hash.clone(),
        mem,
    )
}

fn commit_marker(
    op_id: String,
    kind: SpineCommitKindMarker,
    token_seq_start: u64,
    width: u64,
    raw_boundary: u64,
    raw_live_hash: Option<String>,
    mem: &MemRecord,
) -> Result<SpineCommitMarker, SpineError> {
    Ok(SpineCommitMarker {
        version: COMMIT_MARKER_VERSION,
        op_id,
        kind,
        token_seq_start,
        token_seq_end: token_seq_start.checked_add(width).ok_or_else(|| {
            SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
        })?,
        raw_boundary,
        raw_live_hash,
        memory_refs: vec![commit_memory_ref(mem)],
    })
}

fn commit_marker_kind_label(kind: SpineCommitKindMarker) -> &'static str {
    match kind {
        SpineCommitKindMarker::Close => "close",
        SpineCommitKindMarker::CloseThenOpen => "close_then_open",
        SpineCommitKindMarker::RootCompact => "root_compact",
    }
}

fn commit_memory_ref(mem: &MemRecord) -> SpineCommitMemoryRef {
    SpineCommitMemoryRef {
        compact_id: mem.compact_id.clone(),
        kind: mem.kind,
        node: mem.node.clone(),
        raw_start: mem.raw_start,
        raw_end: mem.raw_end,
        context_start: mem.context_start,
        context_end: mem.context_end,
        raw_live_hash: mem.raw_live_hash.clone(),
        body_path: mem.body_path.clone(),
        body_hash: mem.body_hash.clone(),
    }
}

pub(crate) fn is_real_user_message(item: &ResponseItem) -> bool {
    crate::spine::lexer::is_real_user_message(item)
}

pub(super) fn is_spine_parser_control_tool_name(name: &str) -> bool {
    matches!(name, SPINE_TOOL_OPEN | SPINE_TOOL_CLOSE | SPINE_TOOL_NEXT)
}

pub(crate) fn is_spine_parser_control_tool(namespace: Option<&str>, name: &str) -> bool {
    namespace == Some(SPINE_NAMESPACE) && is_spine_parser_control_tool_name(name)
}

#[cfg(test)]
pub(crate) fn is_spine_close_like_tool_name(name: &str) -> bool {
    matches!(name, SPINE_TOOL_CLOSE | SPINE_TOOL_NEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_control_tool_policy_requires_spine_namespace_and_control_name() {
        assert!(is_spine_parser_control_tool(
            Some(SPINE_NAMESPACE),
            SPINE_TOOL_OPEN
        ));
        assert!(is_spine_parser_control_tool(
            Some(SPINE_NAMESPACE),
            SPINE_TOOL_CLOSE
        ));
        assert!(is_spine_parser_control_tool(
            Some(SPINE_NAMESPACE),
            SPINE_TOOL_NEXT
        ));
        assert!(!is_spine_parser_control_tool(
            Some(SPINE_NAMESPACE),
            SPINE_TOOL_TREE
        ));
        assert!(!is_spine_parser_control_tool(None, SPINE_TOOL_OPEN));
        assert!(!is_spine_parser_control_tool(
            Some("other"),
            SPINE_TOOL_OPEN
        ));
    }

    fn message(role: &str, text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            phase: None,
        }
    }

    #[test]
    fn host_lens_roundtrip_with_fixed_prefix() {
        let history = vec![
            message("developer", "fixed developer prefix"),
            message("user", "mutable 0"),
            message("assistant", "mutable 1"),
            message("user", "mutable 2"),
        ];
        let lens = HostHistoryLens::new(&history);

        assert_eq!(lens.mutable_len(), 3);
        assert!(
            lens.mutable_index_for_full_index(0).is_err(),
            "fixed-prefix host items must not have a mutable index"
        );

        for (full, mutable) in [(1, 0), (2, 1), (3, 2)] {
            let mutable_index = lens
                .mutable_index_for_full_index(full)
                .expect("full host item maps to mutable index");
            assert_eq!(mutable_index, mutable);
            let full_index = lens
                .full_index_for_mutable_index(mutable_index)
                .expect("mutable item maps back to full host index");
            assert_eq!(full_index, full);
        }

        assert_eq!(
            lens.full_index_for_mutable_boundary(3)
                .expect("mutable end boundary maps to full host end"),
            history.len()
        );
        assert_eq!(
            lens.mutable_index_for_full_boundary(history.len())
                .expect("full host end boundary maps to mutable end"),
            3
        );
        assert_eq!(
            lens.mutable_index_for_full_boundary(0)
                .expect("full boundary before fixed prefix maps to mutable start"),
            0
        );
        assert_eq!(
            lens.mutable_index_for_full_boundary(1)
                .expect("full boundary after fixed prefix maps to mutable start"),
            0
        );
    }
}

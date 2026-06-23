use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::runtime::SpineError;
use super::runtime::SpineSessionState;

pub(crate) use super::runtime::SpineHostEffects as HostEffects;
pub(crate) use super::runtime::SpineToolcallHookEvidence as ToolcallHookEvidence;

pub(crate) struct InitEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
}

pub(crate) struct CompactEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) compacted_history: &'a [ResponseItem],
    pub(crate) raw_items: &'a [Option<ResponseItem>],
    pub(crate) close_provider_input_tokens: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct MessageEvidence<'a> {
    pub(crate) rollout_path: &'a Path,
    pub(crate) raw_ordinal: u64,
    pub(crate) context_index: usize,
    pub(crate) item: &'a ResponseItem,
    pub(crate) raw_items: &'a [Option<ResponseItem>],
}

pub(crate) struct ToolCallEvidence<'a> {
    kind: ToolCallEvidenceKind<'a>,
    force_ordinary: bool,
}

enum ToolCallEvidenceKind<'a> {
    Single {
        item: &'a ResponseItem,
    },
    Grouped {
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    },
    #[cfg(test)]
    Runtime(super::runtime::SpineToolCallEvidence<'a>),
}

impl<'a> ToolCallEvidence<'a> {
    pub(crate) fn single(item: &'a ResponseItem) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Single { item },
            force_ordinary: false,
        }
    }

    pub(crate) fn grouped(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self::grouped_with_policy(commit_call_id, tool_call_ids, output_items, false)
    }

    pub(crate) fn grouped_as_ordinary(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
    ) -> Self {
        Self::grouped_with_policy(commit_call_id, tool_call_ids, output_items, true)
    }

    fn grouped_with_policy(
        commit_call_id: &'a str,
        tool_call_ids: &'a [String],
        output_items: &'a [ResponseItem],
        force_ordinary: bool,
    ) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            },
            force_ordinary,
        }
    }

    pub(crate) fn completed_output(
        &self,
    ) -> Result<Option<super::runtime::SpineCompletedToolCallOutputEvidence<'a>>, SpineError> {
        let runtime_evidence = match &self.kind {
            ToolCallEvidenceKind::Single { item } => {
                super::runtime::SpineToolCallEvidence::single(item)
            }
            ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            } if self.force_ordinary => super::runtime::SpineToolCallEvidence::grouped_as_ordinary(
                commit_call_id,
                tool_call_ids,
                output_items,
            ),
            ToolCallEvidenceKind::Grouped {
                commit_call_id,
                tool_call_ids,
                output_items,
            } => super::runtime::SpineToolCallEvidence::grouped(
                commit_call_id,
                tool_call_ids,
                output_items,
            ),
            #[cfg(test)]
            ToolCallEvidenceKind::Runtime(evidence) => return evidence.completed_output(),
        };
        runtime_evidence.completed_output()
    }
}

#[cfg(test)]
impl<'a> From<super::runtime::SpineToolCallEvidence<'a>> for ToolCallEvidence<'a> {
    fn from(evidence: super::runtime::SpineToolCallEvidence<'a>) -> Self {
        Self {
            kind: ToolCallEvidenceKind::Runtime(evidence),
            force_ordinary: false,
        }
    }
}

pub(crate) fn on_init(
    state: &mut SpineSessionState,
    evidence: InitEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state.on_init(super::runtime::SpineInitEvidence {
        rollout_path: evidence.rollout_path,
    })
}

pub(crate) fn on_non_toolcall_msg(
    state: &mut SpineSessionState,
    evidence: MessageEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state.observe_non_toolcall_msg_with_host_effects(super::runtime::SpineMessageEvidence {
        rollout_path: evidence.rollout_path,
        raw_ordinal: evidence.raw_ordinal,
        context_index: evidence.context_index,
        item: evidence.item,
        raw_items: evidence.raw_items,
    })
}

pub(crate) fn on_compact(
    state: &mut SpineSessionState,
    evidence: CompactEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state.prepare_native_root_compact_from_history_with_checkpoint(
        super::runtime::SpineCompactEvidence {
            rollout_path: evidence.rollout_path,
            compacted_history: evidence.compacted_history,
            raw_items: evidence.raw_items,
            close_provider_input_tokens: evidence.close_provider_input_tokens,
        },
    )
}

pub(crate) fn on_toolcall(
    state: &mut SpineSessionState,
    evidence: ToolcallHookEvidence<'_>,
) -> Result<HostEffects, SpineError> {
    state.prepare_completed_toolcall_for_commit(evidence)
}

use super::ParseStack;
use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::memory_ref;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
use crate::spine::model::RawMask;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use std::collections::BTreeMap;

pub(in crate::spine) fn event_to_token(
    event: &LoggedSpineLedgerEvent,
    archive: &SpineArchive,
    mems: &BTreeMap<String, MemRecord>,
    raw_mask: RawMask<'_>,
) -> Result<SpineToken, SpineError> {
    match &event.event {
        SpineLedgerEvent::Init { raw_start } => {
            crate::spine::lexer::lex_init_token(archive, *raw_start)
        }
        SpineLedgerEvent::Msg {
            raw_ordinal,
            context_index,
            from_user,
            user_anchor,
        } => crate::spine::lexer::lex_msg_token(
            *raw_ordinal,
            *context_index,
            *from_user,
            *user_anchor,
        ),
        SpineLedgerEvent::ToolCall { segments } => {
            crate::spine::lexer::lex_toolcall_event_as_token(segments.iter().cloned())
        }
        SpineLedgerEvent::Open {
            child,
            boundary,
            index,
            summary,
            open_input_tokens,
            open_context_tokens,
            open_context_source,
            ..
        } => crate::spine::lexer::lex_open_token(
            archive,
            child.clone(),
            *boundary,
            *index,
            summary.clone(),
            *open_input_tokens,
            *open_context_tokens,
            *open_context_source,
        ),
        SpineLedgerEvent::Close { node, .. } => {
            let mem = mems.values().find(|mem| &mem.node == node).ok_or_else(|| {
                SpineError::InvalidEvent(format!("missing memory for close node {node}"))
            })?;
            validate_replay_memory_raw_evidence(mem, raw_mask)?;
            crate::spine::lexer::lex_close_token(replay_memory_ref(archive, mem, event.seq))
        }
        SpineLedgerEvent::RootCompact {
            mem,
            next_open_index,
            ..
        } => {
            let mem = mems.get(mem).ok_or_else(|| {
                SpineError::InvalidEvent("missing memory for root compact".to_string())
            })?;
            validate_replay_memory_raw_evidence(mem, raw_mask)?;
            let memory = replay_memory_ref(archive, mem, event.seq);
            crate::spine::lexer::plan_root_compact().lex_compact_token(
                memory,
                usize::try_from(*next_open_index).map_err(|_| {
                    SpineError::InvalidEvent("root open index overflow".to_string())
                })?,
                None,
                None,
            )
        }
        SpineLedgerEvent::OpenContextBaseline { .. } => Err(SpineError::InvalidEvent(
            "OpenContextBaseline is metadata and cannot be converted to a SpineToken".to_string(),
        )),
    }
}

fn replay_memory_ref(archive: &SpineArchive, mem: &MemRecord, event_seq: u64) -> MemoryRef {
    memory_ref(
        archive,
        mem.compact_id.clone(),
        mem.node.clone(),
        mem.body_hash.clone(),
        mem.raw_start..mem.raw_end,
        mem.context_start..mem.context_end,
        event_seq..event_seq + 1,
        mem.open_input_tokens,
        mem.close_input_tokens,
        mem.open_context_tokens,
        mem.close_context_tokens,
        mem.closed_source_suffix_tokens,
        mem.closed_memory_context_tokens,
        mem.open_context_source,
        mem.memory_output_tokens,
    )
}

fn validate_replay_memory_raw_evidence(
    mem: &MemRecord,
    raw_mask: RawMask<'_>,
) -> Result<(), SpineError> {
    if !mem.allowed_by(raw_mask)? {
        return Err(SpineError::InvalidEvent(format!(
            "memory {} does not cover live raw evidence",
            mem.compact_id
        )));
    }
    Ok(())
}

pub(in crate::spine) fn apply_metadata_event(
    ps: &mut ParseStack,
    event: &LoggedSpineLedgerEvent,
) -> Result<bool, SpineError> {
    match &event.event {
        SpineLedgerEvent::OpenContextBaseline {
            node,
            open_input_tokens,
            open_context_tokens,
            open_context_source,
            ..
        } => {
            if open_input_tokens != open_context_tokens {
                return Err(SpineError::InvalidEvent(format!(
                    "open context baseline for node {node} has mismatched provider input encoding"
                )));
            }
            ps.set_live_open_context_baseline(node, *open_input_tokens, *open_context_source)
        }
        _ => Ok(false),
    }
}

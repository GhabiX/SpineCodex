use super::ParseStack;
use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::memory_ref;
use crate::spine::archive::tree_meta;
use crate::spine::archive::tree_meta_with_token_baselines;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::NodeId;
use crate::spine::model::RawMask;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub(in crate::spine) fn event_to_token(
    event: &LoggedSpineLedgerEvent,
    archive: &SpineArchive,
    mems: &BTreeMap<String, MemRecord>,
    raw_mask: RawMask<'_>,
) -> Result<SpineToken, SpineError> {
    match &event.event {
        SpineLedgerEvent::Init { raw_start } => Ok(SpineToken::Init {
            meta: tree_meta(
                archive,
                NodeId::root_epoch(1),
                *raw_start,
                "root".to_string(),
            )?,
        }),
        SpineLedgerEvent::Msg {
            raw_ordinal,
            context_index,
            from_user,
            user_anchor,
        } => crate::spine::lexer::lex_msg(*raw_ordinal, *context_index, *from_user, *user_anchor)
            .and_then(|lexed| lexed.into_single_token("msg")),
        SpineLedgerEvent::ToolCall { segments } => {
            crate::spine::lexer::lex_toolcall_event(segments.iter().cloned())
                .and_then(|lexed| lexed.into_single_token("toolcall"))
        }
        SpineLedgerEvent::Open {
            child,
            index,
            summary,
            open_input_tokens,
            open_context_tokens,
            open_context_source,
            ..
        } => {
            if open_input_tokens != open_context_tokens {
                return Err(SpineError::InvalidEvent(format!(
                    "open event for node {child} has mismatched provider input baseline encoding"
                )));
            }
            Ok(SpineToken::Open {
                meta: tree_meta_with_token_baselines(
                    archive,
                    child.clone(),
                    *index,
                    summary.clone(),
                    *open_input_tokens,
                    *open_context_source,
                )?,
            })
        }
        SpineLedgerEvent::Close { node, .. } => {
            let mem = mems.values().find(|mem| &mem.node == node).ok_or_else(|| {
                SpineError::InvalidEvent(format!("missing memory for close node {node}"))
            })?;
            if !mem.allowed_by(raw_mask)? {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    mem.compact_id
                )));
            }
            Ok(SpineToken::Close {
                memory: memory_ref(
                    archive,
                    mem.compact_id.clone(),
                    mem.node.clone(),
                    mem.body_hash.clone(),
                    mem.raw_start..mem.raw_end,
                    mem.context_start..mem.context_end,
                    event.seq..event.seq + 1,
                    mem.open_input_tokens,
                    mem.close_input_tokens,
                    mem.open_context_tokens,
                    mem.close_context_tokens,
                    mem.closed_source_suffix_tokens,
                    mem.closed_memory_context_tokens,
                    mem.open_context_source,
                    mem.memory_output_tokens,
                ),
            })
        }
        SpineLedgerEvent::RootCompact {
            mem,
            next_open_index,
            ..
        } => {
            let mem = mems.get(mem).ok_or_else(|| {
                SpineError::InvalidEvent("missing memory for root compact".to_string())
            })?;
            if !mem.allowed_by(raw_mask)? {
                return Err(SpineError::InvalidEvent(format!(
                    "memory {} does not cover live raw evidence",
                    mem.compact_id
                )));
            }
            Ok(SpineToken::Compact {
                memory: memory_ref(
                    archive,
                    mem.compact_id.clone(),
                    mem.node.clone(),
                    mem.body_hash.clone(),
                    mem.raw_start..mem.raw_end,
                    mem.context_start..mem.context_end,
                    event.seq..event.seq + 1,
                    mem.open_input_tokens,
                    mem.close_input_tokens,
                    mem.open_context_tokens,
                    mem.close_context_tokens,
                    mem.closed_source_suffix_tokens,
                    mem.closed_memory_context_tokens,
                    mem.open_context_source,
                    mem.memory_output_tokens,
                ),
                next_open_index: usize::try_from(*next_open_index).map_err(|_| {
                    SpineError::InvalidEvent("root open index overflow".to_string())
                })?,
                next_open_input_tokens: None,
                next_open_context_tokens: None,
            })
        }
        SpineLedgerEvent::OpenContextBaseline { .. } => Err(SpineError::InvalidEvent(
            "OpenContextBaseline is metadata and cannot be converted to a SpineToken".to_string(),
        )),
    }
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

pub(in crate::spine) fn parse_stack_from_events_with_forced_events(
    events: &[LoggedSpineLedgerEvent],
    archive: &SpineArchive,
    mems: &[MemRecord],
    raw_mask: RawMask<'_>,
    forced_event_seqs: &BTreeSet<u64>,
    marker_structural_event_seqs: &BTreeSet<u64>,
) -> Result<ParseStack, SpineError> {
    let mems = mems
        .iter()
        .cloned()
        .map(|mem| (mem.compact_id.clone(), mem))
        .collect::<BTreeMap<_, _>>();
    let mut ps = ParseStack::new();
    for event in events {
        if forced_event_seqs.contains(&event.seq) {
            if !apply_metadata_event(&mut ps, event)? {
                ps.shift(event_to_token(event, archive, &mems, raw_mask)?, archive)?;
            }
            continue;
        }
        if marker_structural_event_seqs.contains(&event.seq) || !event.allowed_by(raw_mask)? {
            continue;
        }
        if !apply_metadata_event(&mut ps, event)? {
            ps.shift(event_to_token(event, archive, &mems, raw_mask)?, archive)?;
        }
    }
    Ok(ps)
}

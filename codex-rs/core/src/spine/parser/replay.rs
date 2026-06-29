use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::memory_ref;
use crate::spine::lexer::LexedTokenBatch;
use crate::spine::model::LoggedSpineLedgerEvent;
use crate::spine::model::MemRecord;
use crate::spine::model::MemoryRef;
use crate::spine::model::RawMask;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::parse_stack::ParseStack;

use super::ParserState;

impl ParserState {
    pub(in crate::spine) fn from_replay_events_with_initial_and_forced_events(
        events: &[LoggedSpineLedgerEvent],
        archive: &SpineArchive,
        mems: &[MemRecord],
        raw_mask: RawMask<'_>,
        forced_event_seqs: &BTreeSet<u64>,
        marker_structural_event_seqs: &BTreeSet<u64>,
        initial: Option<&ParseStack>,
        min_seq: Option<u64>,
    ) -> Result<Self, SpineError> {
        let events = events
            .iter()
            .filter(|event| min_seq.is_none_or(|min_seq| event.seq >= min_seq))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(initial) = initial {
            let mut parser = Self::from_parse_stack(initial.clone());
            let mems = mems
                .iter()
                .map(|mem| (mem.compact_id.clone(), mem))
                .collect::<BTreeMap<_, _>>();
            for event in &events {
                if matches!(event.event, SpineLedgerEvent::OpenContextBaseline { .. }) {
                    continue;
                }
                if forced_event_seqs.contains(&event.seq)
                    || (!marker_structural_event_seqs.contains(&event.seq)
                        && event.allowed_by(raw_mask)?)
                {
                    parser.apply_replay_event(event, archive, &mems, raw_mask)?;
                }
            }
            return Ok(parser);
        }
        Self::from_replay_events_with_forced_events(
            &events,
            archive,
            mems,
            raw_mask,
            forced_event_seqs,
            marker_structural_event_seqs,
        )
    }

    pub(in crate::spine) fn from_replay_events_with_forced_events(
        events: &[LoggedSpineLedgerEvent],
        archive: &SpineArchive,
        mems: &[MemRecord],
        raw_mask: RawMask<'_>,
        forced_event_seqs: &BTreeSet<u64>,
        marker_structural_event_seqs: &BTreeSet<u64>,
    ) -> Result<Self, SpineError> {
        let mems = mems
            .iter()
            .map(|mem| (mem.compact_id.clone(), mem))
            .collect::<BTreeMap<_, _>>();
        let mut parser = Self::new();
        for event in events {
            if forced_event_seqs.contains(&event.seq)
                || (!marker_structural_event_seqs.contains(&event.seq)
                    && event.allowed_by(raw_mask)?)
            {
                parser.apply_replay_event(event, archive, &mems, raw_mask)?;
            }
        }
        Ok(parser)
    }

    pub(in crate::spine) fn apply_replay_event(
        &mut self,
        event: &LoggedSpineLedgerEvent,
        archive: &SpineArchive,
        mems: &BTreeMap<String, &MemRecord>,
        raw_mask: RawMask<'_>,
    ) -> Result<(), SpineError> {
        if !apply_replay_metadata_event(&mut self.parse_stack, event)? {
            let lexed = replay_event_to_lexed_batch(event, archive, mems, raw_mask)?;
            let staged = self.stage_lexed_batches(std::iter::once(&lexed), archive)?;
            self.install_prepared_state(staged);
        }
        Ok(())
    }
}

fn replay_event_to_lexed_batch(
    event: &LoggedSpineLedgerEvent,
    archive: &SpineArchive,
    mems: &BTreeMap<String, &MemRecord>,
    raw_mask: RawMask<'_>,
) -> Result<LexedTokenBatch, SpineError> {
    match &event.event {
        SpineLedgerEvent::Init { raw_start } => crate::spine::lexer::lex_init(archive, *raw_start),
        SpineLedgerEvent::Msg {
            raw_ordinal,
            context_index,
            from_user,
            user_anchor,
        } => crate::spine::lexer::lex_msg(*raw_ordinal, *context_index, *from_user, *user_anchor),
        SpineLedgerEvent::ToolCall { segments } => {
            crate::spine::lexer::lex_toolcall_event(segments.iter().cloned())
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
        } => crate::spine::lexer::lex_open(
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
            let mem = mems
                .values()
                .copied()
                .find(|mem| &mem.node == node)
                .ok_or_else(|| {
                    SpineError::InvalidEvent(format!("missing memory for close node {node}"))
                })?;
            validate_replay_memory_raw_evidence(mem, raw_mask)?;
            let SpineLedgerEvent::Close {
                node,
                boundary,
                summary,
                close_input_tokens,
                close_context_tokens,
            } = &event.event
            else {
                unreachable!("close event was matched before replay close lexing")
            };
            crate::spine::lexer::lex_close(
                node.clone(),
                *boundary,
                summary.clone(),
                *close_input_tokens,
                *close_context_tokens,
                replay_memory_ref(archive, mem, event.seq),
            )
        }
        SpineLedgerEvent::RootCompact {
            mem,
            next_open_index,
            ..
        } => {
            let mem = mems.get(mem).copied().ok_or_else(|| {
                SpineError::InvalidEvent("missing memory for root compact".to_string())
            })?;
            validate_replay_memory_raw_evidence(mem, raw_mask)?;
            let memory = replay_memory_ref(archive, mem, event.seq);
            crate::spine::lexer::plan_root_compact().lex_compact_batch(
                memory,
                usize::try_from(*next_open_index).map_err(|_| {
                    SpineError::InvalidEvent("root open index overflow".to_string())
                })?,
                None,
                None,
            )
        }
        SpineLedgerEvent::OpenContextBaseline { .. } => Err(SpineError::InvalidEvent(
            "OpenContextBaseline is metadata and cannot be converted to a LexedTokenBatch"
                .to_string(),
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

fn apply_replay_metadata_event(
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

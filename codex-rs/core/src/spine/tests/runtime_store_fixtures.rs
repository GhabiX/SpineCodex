use super::*;
use std::path::PathBuf;

#[path = "runtime_store_writer_fixtures.rs"]
mod runtime_store_writer_fixtures;
pub(crate) use runtime_store_writer_fixtures::*;
#[path = "runtime_store_event_fixtures.rs"]
mod runtime_store_event_fixtures;
pub(crate) use runtime_store_event_fixtures::*;
#[path = "runtime_store_checkpoint_fixtures.rs"]
mod runtime_store_checkpoint_fixtures;
pub(crate) use runtime_store_checkpoint_fixtures::*;

// Shared raw/context fixtures.

pub(crate) fn root_child_open_event(summary: &str) -> SpineLedgerEvent {
    SpineLedgerEvent::Open {
        child: NodeId::root_epoch(1).child(1),
        boundary: 0,
        index: 0,
        summary: summary.to_string(),
        open_input_tokens: None,
        open_context_tokens: None,
        open_context_source: None,
    }
}

pub(crate) fn user_msg_event(raw_ordinal: u64, context_index: u64) -> SpineLedgerEvent {
    SpineLedgerEvent::Msg {
        raw_ordinal,
        context_index,
        from_user: true,
        user_anchor: None,
    }
}

pub(crate) fn rollout_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("rollout.jsonl")
}

pub(crate) fn clone_for_rollout_with_raw_live(
    source_rollout: &std::path::Path,
    target_rollout: &std::path::Path,
    raw_live: &[bool],
) {
    let boundary = SpineStore::clone_boundary_for_rollout(
        source_rollout,
        u64::try_from(raw_live.len()).expect("raw live len"),
    )
    .expect("capture clone boundary")
    .expect("source sidecar exists");
    SpineStore::clone_for_rollout_with_raw_live(&boundary, target_rollout, raw_live)
        .expect("clone sidecar");
}

pub(crate) fn current_context_len(runtime: &SpineRuntime, raw: &[Option<ResponseItem>]) -> usize {
    runtime
        .materialize_history_for_test(raw)
        .expect("materialize current h(PS)")
        .len()
}

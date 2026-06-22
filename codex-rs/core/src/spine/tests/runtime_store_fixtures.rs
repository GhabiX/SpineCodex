use super::*;
use std::path::PathBuf;

#[path = "runtime_store_writer_fixtures.rs"]
mod runtime_store_writer_fixtures;
pub(super) use runtime_store_writer_fixtures::*;
#[path = "runtime_store_event_fixtures.rs"]
mod runtime_store_event_fixtures;
pub(super) use runtime_store_event_fixtures::*;
#[path = "runtime_store_checkpoint_fixtures.rs"]
mod runtime_store_checkpoint_fixtures;
pub(super) use runtime_store_checkpoint_fixtures::*;

// Shared raw/context fixtures.

pub(super) fn rollout_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("rollout.jsonl")
}

pub(super) fn clone_for_rollout_with_raw_live(
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

pub(super) fn current_context_len(runtime: &SpineRuntime, raw: &[Option<ResponseItem>]) -> usize {
    runtime
        .materialize_history(raw)
        .expect("materialize current h(PS)")
        .len()
}

use super::*;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

// Shared raw/context fixtures.

pub(super) fn rollout_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("rollout.jsonl")
}

pub(super) fn eventually_load_or_create_writer(
    rollout: &std::path::Path,
    raw_len: u64,
) -> SpineRuntime {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_err = None;
    loop {
        match SpineRuntime::load_or_create(rollout, raw_len) {
            Ok(runtime) => return runtime,
            Err(err) => {
                last_err = Some(err);
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    panic!(
        "writer lock should release after drop: {}",
        last_err
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    );
}

pub(super) fn eventually_set_replayed_writer(
    state: &mut SpineSessionState,
    rollout: &std::path::Path,
    raw_len: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_err = None;
    loop {
        let replayed = SpineRuntime::load_for_rollout(rollout, raw_len)
            .expect("reload read-only replay after first live runtime drops")
            .expect("sidecar exists");
        match state.set_replayed(raw_len, Some(replayed)) {
            Ok(()) => return,
            Err(err) => {
                last_err = Some(err);
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    panic!(
        "replayed runtime can become live writer after lock release: {}",
        last_err
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    );
}

pub(super) fn logged_events(runtime: &SpineRuntime) -> Vec<LoggedSpineLedgerEvent> {
    runtime.store.events_for_test().expect("events")
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

pub(super) fn root_compact_checkpoint_for_memory(
    rollout_path: &std::path::Path,
    mem: &MemRecord,
    body: &str,
    root_event_seq: u64,
    token_seq: u64,
    body_path: String,
) -> SpineCompactCheckpoint {
    let replacement_history = vec![memory_response_item(body)];
    let replacement_history_hash =
        hash_response_items(&replacement_history).expect("hash replacement_history");
    SpineCompactCheckpoint {
        version: CHECKPOINT_VERSION,
        rollout_path: rollout_path.display().to_string(),
        raw_boundary: mem.raw_end,
        token_seq,
        raw_live_hash: mem
            .raw_live_hash
            .clone()
            .expect("root compact memory carries raw live hash"),
        context_len: replacement_history.len(),
        h_ps_hash: replacement_history_hash.clone(),
        replacement_history_hash,
        response_item_refs: Vec::new(),
        memory_item_refs: vec![CompactCheckpointMemoryItemRef {
            compact_id: mem.compact_id.clone(),
            context_index: 0,
            item_hash: hash_response_items(&[memory_response_item(body)])
                .expect("hash memory item"),
        }],
        memory_refs: vec![CheckpointMemoryRef {
            compact_id: mem.compact_id.clone(),
            node_id: mem.node.to_string(),
            body_path,
            body_hash: mem.body_hash.clone(),
            source_raw_start: mem.raw_start,
            source_raw_end: mem.raw_end,
            source_context_start: mem.context_start,
            source_context_end: mem.context_end,
            source_token_seq_start: root_event_seq,
            source_token_seq_end: token_seq,
            open_input_tokens: mem.open_input_tokens,
            close_input_tokens: mem.close_input_tokens,
            open_context_tokens: mem.open_context_tokens,
            close_context_tokens: mem.close_context_tokens,
            closed_source_suffix_tokens: mem.closed_source_suffix_tokens,
            closed_memory_context_tokens: mem.closed_memory_context_tokens,
            open_context_source: mem.open_context_source,
            memory_output_tokens: mem.memory_output_tokens,
        }],
    }
}

pub(super) fn event_log(runtime: &SpineRuntime) -> Vec<SpineLedgerEvent> {
    logged_events(runtime)
        .into_iter()
        .map(|event| event.event)
        .collect()
}

pub(super) fn event_log_debug(runtime: &SpineRuntime) -> Vec<String> {
    event_log(runtime)
        .into_iter()
        .map(|event| format!("{event:?}"))
        .collect()
}

pub(super) fn assert_parse_stack_tree_and_events_unchanged(
    runtime: &SpineRuntime,
    parse_stack_before: &ParseStack,
    tree_before: &str,
    events_before: &[String],
) {
    assert_eq!(runtime.parse_stack(), parse_stack_before);
    assert_eq!(
        runtime.render_tree().expect("render tree after failure"),
        tree_before
    );
    assert_eq!(event_log_debug(runtime), events_before);
}

pub(super) fn ledger_event_debug(runtime: &SpineRuntime) -> Vec<String> {
    runtime
        .ledger
        .events
        .iter()
        .map(|event| format!("{event:?}"))
        .collect()
}

pub(super) fn assert_pending_close_retry_state(runtime: &SpineRuntime, ledger_before: &[String]) {
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Close(_)))),
        "failed close-like reduce should retain the zero-width Close token for retry"
    );
    assert_eq!(ledger_event_debug(runtime), ledger_before);
}

pub(super) fn assert_pending_compact_retry_state(runtime: &SpineRuntime, ledger_before: &[String]) {
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Compact(..)))),
        "failed root compact reduce should retain the zero-width Compact token for retry"
    );
    assert_eq!(ledger_event_debug(runtime), ledger_before);
}

pub(super) fn current_context_len(runtime: &SpineRuntime, raw: &[Option<ResponseItem>]) -> usize {
    runtime
        .materialize_history(raw)
        .expect("materialize current h(PS)")
        .len()
}

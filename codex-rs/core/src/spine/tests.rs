use super::*;
use crate::spine::CHECKPOINT_VERSION;
use crate::spine::SpineCloneBoundary;
use crate::spine::archive::memory_ref;
use crate::spine::archive::tree_meta;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::compact_checkpoint::CompactCheckpointMemoryItemRef;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::io::hash_response_items;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::NodeId;
use crate::spine::model::PressureEvent;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineToken;
use crate::spine::model::ToolCallEventSegment;
use crate::spine::model::ToolCallSegment;
use crate::spine::model::TrimResponseKind;
use crate::spine::render::memory_response_item;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageDetail;
use codex_protocol::spine_tree::SpineNodeContextBaselineSource;
use codex_protocol::spine_tree::SpineTreeNodeAccountingSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use serial_test::serial;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

#[path = "tests/checkpoint_failures.rs"]
mod checkpoint_failures;
#[path = "tests/clone_missing_memory.rs"]
mod clone_missing_memory;
#[path = "tests/clone_structural_pressure.rs"]
mod clone_structural_pressure;
#[path = "tests/close_commit_durable_failures.rs"]
mod close_commit_durable_failures;
#[path = "tests/close_commit_failures.rs"]
mod close_commit_failures;
#[path = "tests/close_lifecycle.rs"]
mod close_lifecycle;
#[path = "tests/close_output_projection.rs"]
mod close_output_projection;
#[path = "tests/close_reduce_edges.rs"]
mod close_reduce_edges;
#[path = "tests/close_retry.rs"]
mod close_retry;
#[path = "tests/close_source_plan.rs"]
mod close_source_plan;
#[path = "tests/closed_memory_accounting.rs"]
mod closed_memory_accounting;
#[path = "tests/commit_marker_carriers.rs"]
mod commit_marker_carriers;
#[path = "tests/commit_marker_prepare_failures.rs"]
mod commit_marker_prepare_failures;
#[path = "tests/commit_marker_replay.rs"]
mod commit_marker_replay;
#[path = "tests/commit_marker_required.rs"]
mod commit_marker_required;
#[path = "tests/commit_marker_resume_failures.rs"]
mod commit_marker_resume_failures;
#[path = "tests/commit_marker_root_compact.rs"]
mod commit_marker_root_compact;
#[path = "tests/compact_checkpoint_ambiguity.rs"]
mod compact_checkpoint_ambiguity;
#[path = "tests/compact_checkpoint_clone.rs"]
mod compact_checkpoint_clone;
#[path = "tests/compact_checkpoint_proofs.rs"]
mod compact_checkpoint_proofs;
#[path = "tests/compact_checkpoint_validation.rs"]
mod compact_checkpoint_validation;
#[path = "tests/error_classification.rs"]
mod error_classification;
#[path = "tests/fork_isolation.rs"]
mod fork_isolation;
#[path = "tests/m0_trace.rs"]
mod m0_trace;
#[path = "tests/materialize_history.rs"]
mod materialize_history;
#[path = "tests/materialize_projection_guards.rs"]
mod materialize_projection_guards;
#[path = "tests/message_anchors.rs"]
mod message_anchors;
#[path = "tests/next_failure.rs"]
mod next_failure;
#[path = "tests/next_lifecycle.rs"]
mod next_lifecycle;
#[path = "tests/next_transactions.rs"]
mod next_transactions;
#[path = "tests/open_lifecycle.rs"]
mod open_lifecycle;
#[path = "tests/pending_control.rs"]
mod pending_control;
#[path = "tests/pending_control_receipts.rs"]
mod pending_control_receipts;
#[path = "tests/prepared_commit.rs"]
mod prepared_commit;
#[path = "tests/provider_baseline.rs"]
mod provider_baseline;
#[path = "tests/raw_coverage.rs"]
mod raw_coverage;
#[path = "tests/rollback_checkpoint_continuation.rs"]
mod rollback_checkpoint_continuation;
#[path = "tests/rollback_checkpoint_fail_closed.rs"]
mod rollback_checkpoint_fail_closed;
#[path = "tests/rollback_checkpoint_restore.rs"]
mod rollback_checkpoint_restore;
#[path = "tests/rollback_sparse.rs"]
mod rollback_sparse;
#[path = "tests/root_compact_boundary.rs"]
mod root_compact_boundary;
#[path = "tests/root_compact_failures.rs"]
mod root_compact_failures;
#[path = "tests/root_compact_lifecycle.rs"]
mod root_compact_lifecycle;
#[path = "tests/root_compact_prepared.rs"]
mod root_compact_prepared;
#[path = "tests/root_compact_replay.rs"]
mod root_compact_replay;
#[path = "tests/runtime_lifecycle.rs"]
mod runtime_lifecycle;
#[path = "tests/store_basics.rs"]
mod store_basics;
#[path = "tests/toolcall_grouping.rs"]
mod toolcall_grouping;
#[path = "tests/toolcall_lexer.rs"]
mod toolcall_lexer;
#[path = "tests/tree_accounting.rs"]
mod tree_accounting;
#[path = "tests/tree_snapshot.rs"]
mod tree_snapshot;
#[path = "tests/trim_candidates.rs"]
mod trim_candidates;
#[path = "tests/trim_projection.rs"]
mod trim_projection;
#[path = "tests/trim_projection_slice.rs"]
mod trim_projection_slice;
#[path = "tests/trim_rollback_fork.rs"]
mod trim_rollback_fork;
#[path = "tests/trim_targeting.rs"]
mod trim_targeting;

// Shared raw/context fixtures.

fn rollout_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("rollout.jsonl")
}

fn eventually_load_or_create_writer(rollout: &std::path::Path, raw_len: u64) -> SpineRuntime {
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

fn eventually_set_replayed_writer(
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

fn text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn anchored_text_item(anchor: u64, text: &str) -> ResponseItem {
    text_item(&format!("[U{anchor}]\n{text}"))
}

fn multimodal_user_item() -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "first text".to_string(),
            },
            ContentItem::InputImage {
                image_url: "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR".to_string(),
                detail: Some(ImageDetail::High),
            },
            ContentItem::InputText {
                text: "second text".to_string(),
            },
        ],
        phase: None,
    }
}

fn assistant_text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn tool_req(raw_ordinal: u64, context_index: usize) -> ToolCallSegment {
    tool_segment(ToolCallSegmentKind::Request, raw_ordinal, context_index)
}

fn tool_resp(raw_ordinal: u64, context_index: usize) -> ToolCallSegment {
    tool_segment(ToolCallSegmentKind::Response, raw_ordinal, context_index)
}

fn tool_segment(
    kind: ToolCallSegmentKind,
    raw_ordinal: u64,
    context_index: usize,
) -> ToolCallSegment {
    ToolCallSegment {
        kind,
        seg: SegRef::ResponseItem {
            raw_ordinal,
            context_index,
        },
    }
}

fn completed_toolcall(call_id: &str, segments: Vec<ToolCallSegment>) -> CompletedToolCall {
    let request_count = segments
        .iter()
        .filter(|segment| segment.kind == ToolCallSegmentKind::Request)
        .count();
    CompletedToolCall {
        call_id: call_id.to_string(),
        request_call_ids: vec![call_id.to_string(); request_count],
        segments: segments
            .into_iter()
            .map(|segment| {
                let SegRef::ResponseItem {
                    raw_ordinal,
                    context_index,
                } = segment.seg
                else {
                    panic!("test helper only accepts raw response-item toolcall segments");
                };
                CompletedToolCallSegment {
                    kind: segment.kind,
                    raw_ordinal,
                    context_index,
                }
            })
            .collect(),
    }
}

fn event_tool_req(raw_ordinal: u64, context_index: u64) -> ToolCallEventSegment {
    event_tool_segment(ToolCallSegmentKind::Request, raw_ordinal, context_index)
}

fn event_tool_resp(raw_ordinal: u64, context_index: u64) -> ToolCallEventSegment {
    event_tool_segment(ToolCallSegmentKind::Response, raw_ordinal, context_index)
}

fn event_tool_segment(
    kind: ToolCallSegmentKind,
    raw_ordinal: u64,
    context_index: u64,
) -> ToolCallEventSegment {
    ToolCallEventSegment {
        kind,
        raw_ordinal,
        context_index,
    }
}

fn logged_events(runtime: &SpineRuntime) -> Vec<LoggedSpineLedgerEvent> {
    runtime.store.events_for_test().expect("events")
}

fn clone_for_rollout_with_raw_live(
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

fn root_compact_checkpoint_for_memory(
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

fn event_log(runtime: &SpineRuntime) -> Vec<SpineLedgerEvent> {
    logged_events(runtime)
        .into_iter()
        .map(|event| event.event)
        .collect()
}

fn event_log_debug(runtime: &SpineRuntime) -> Vec<String> {
    event_log(runtime)
        .into_iter()
        .map(|event| format!("{event:?}"))
        .collect()
}

fn assert_parse_stack_tree_and_events_unchanged(
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

fn ledger_event_debug(runtime: &SpineRuntime) -> Vec<String> {
    runtime
        .ledger
        .events
        .iter()
        .map(|event| format!("{event:?}"))
        .collect()
}

fn assert_pending_close_retry_state(runtime: &SpineRuntime, ledger_before: &[String]) {
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

fn assert_pending_compact_retry_state(runtime: &SpineRuntime, ledger_before: &[String]) {
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

fn current_context_len(runtime: &SpineRuntime, raw: &[Option<ResponseItem>]) -> usize {
    runtime
        .materialize_history(raw)
        .expect("materialize current h(PS)")
        .len()
}

fn spine_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

fn ordinary_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

fn function_output(call_id: &str) -> ResponseItem {
    function_output_text(call_id, "ok")
}

fn function_output_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: codex_protocol::models::FunctionCallOutputPayload::from_text(text.to_string()),
    }
}

fn function_output_content_items(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: codex_protocol::models::FunctionCallOutputPayload::from_content_items(vec![
            codex_protocol::models::FunctionCallOutputContentItem::InputText {
                text: text.to_string(),
            },
        ]),
    }
}

fn function_output_text_content(item: &ResponseItem) -> &str {
    let ResponseItem::FunctionCallOutput { output, .. } = item else {
        panic!("expected FunctionCallOutput, got {item:?}");
    };
    output.text_content().expect("text output")
}

fn response_item_trace_signature(item: &ResponseItem) -> String {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let text = content
                .iter()
                .filter_map(|item| match item {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.starts_with("<spine_memory>")
                && let Some(line) = text
                    .lines()
                    .find(|line| line.starts_with("# Spine Memory "))
            {
                return format!("memory:{line}");
            }
            let text = text
                .strip_prefix("[U")
                .and_then(|rest| rest.split_once("]\n").map(|(_, body)| body))
                .unwrap_or(&text);
            format!("{role}:{text}")
        }
        ResponseItem::FunctionCall {
            name,
            namespace,
            call_id,
            ..
        } => {
            if namespace.as_deref() == Some(SPINE_NAMESPACE) {
                format!("spine-call:{name}:{call_id}")
            } else {
                format!("tool-call:{name}:{call_id}")
            }
        }
        ResponseItem::FunctionCallOutput { call_id, output } => {
            let text = output.text_content().unwrap_or("<structured-output>");
            format!("tool-output:{call_id}:{text}")
        }
        other => format!("{other:?}"),
    }
}

fn materialized_trace_signature(
    runtime: &SpineRuntime,
    raw: &[Option<ResponseItem>],
) -> Vec<String> {
    runtime
        .materialize_history(raw)
        .expect("materialize h(PS)")
        .iter()
        .map(response_item_trace_signature)
        .collect()
}

fn custom_tool_output_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        call_id: call_id.to_string(),
        name: Some("custom_tool".to_string()),
        output: codex_protocol::models::FunctionCallOutputPayload::from_text(text.to_string()),
    }
}

fn custom_tool_output_text_content(item: &ResponseItem) -> &str {
    let ResponseItem::CustomToolCallOutput { output, .. } = item else {
        panic!("expected CustomToolCallOutput, got {item:?}");
    };
    output.text_content().expect("custom tool text output")
}

fn manual_toolcall_event(
    request_raw: u64,
    request_index: u64,
    response_raw: u64,
    response_index: u64,
) -> SpineLedgerEvent {
    SpineLedgerEvent::ToolCall {
        segments: vec![
            event_tool_req(request_raw, request_index),
            event_tool_resp(response_raw, response_index),
        ],
    }
}

fn memory_assembly_with_context_range(
    node_id: &str,
    source_context_range: Range<usize>,
) -> SpineCloseMemoryAssembly {
    let source_raw_range = u64::try_from(source_context_range.start).expect("range start fits u64")
        ..u64::try_from(source_context_range.end).expect("range end fits u64");
    memory_assembly_with_ranges(node_id, source_context_range, source_raw_range)
}

fn memory_assembly_with_ranges(
    node_id: &str,
    source_context_range: Range<usize>,
    source_raw_range: Range<u64>,
) -> SpineCloseMemoryAssembly {
    SpineCloseMemoryAssembly {
        body: format!("# Spine Memory {node_id}\n\nreal compact body for {node_id}\n"),
        source_context_range,
        source_raw_range,
        memory_output_tokens: Some(1_250),
    }
}

fn observe_spine_request(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    tool_name: &str,
    call_id: &str,
) -> (ResponseItem, u64, usize) {
    let request = spine_call(tool_name, call_id);
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(runtime, raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record spine request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe spine request");
    (request, request_ordinal, request_context_index)
}

fn observe_function_output(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
) -> (ResponseItem, u64, usize) {
    let output = function_output(call_id);
    let output_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let output_context_index = current_context_len(runtime, raw)
        .checked_add(1)
        .expect("output context index fits usize");
    raw.push(Some(output.clone()));
    runtime
        .observe_raw_items(1)
        .expect("record function output");
    runtime
        .observe_context_item(output_ordinal, output_context_index, &output)
        .expect("observe function output");
    (output, output_ordinal, output_context_index)
}

fn observe_item_at_context_index(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    item: ResponseItem,
    context_index: usize,
) -> (ResponseItem, u64, usize) {
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record raw item");
    runtime
        .observe_context_item(raw_ordinal, context_index, &item)
        .expect("observe context item");
    (item, raw_ordinal, context_index)
}

// Shared lifecycle and tree projection fixtures.

fn open_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    summary: &str,
) {
    observe_spine_request(runtime, raw, SPINE_TOOL_OPEN, call_id);
    runtime
        .stage_open(call_id.to_string(), summary.to_string())
        .expect("stage open");

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output(call_id, None)
        .expect("commit open");
}

fn open_task_with_token_baselines(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    summary: &str,
    token_baselines: SpineTokenBaselines,
) {
    observe_spine_request(runtime, raw, SPINE_TOOL_OPEN, call_id);
    runtime
        .stage_open(call_id.to_string(), summary.to_string())
        .expect("stage open");

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output_with_token_baselines(call_id, None, token_baselines)
        .expect("commit open");
}

fn append_msg(runtime: &mut SpineRuntime, raw: &mut Vec<Option<ResponseItem>>, text: &str) {
    let item = text_item(text);
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let context_index = current_context_len(runtime, raw);
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record msg");
    runtime
        .observe_context_item(raw_ordinal, context_index, &item)
        .expect("observe msg");
}

fn append_msg_with_context_index(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    text: &str,
    context_index: usize,
) {
    let item = text_item(text);
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record msg");
    runtime
        .observe_context_item(raw_ordinal, context_index, &item)
        .expect("observe msg");
}

fn close_memory_assembly_from_source_plan(
    runtime: &SpineRuntime,
    raw: &[Option<ResponseItem>],
    call_id: &str,
    node_id: &str,
) -> SpineCloseMemoryAssembly {
    let (node, suffix_start) = match runtime
        .pending_commit(call_id)
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close {
            node, suffix_start, ..
        }) => (node, suffix_start),
        other => panic!("expected pending close, got {other:?}"),
    };
    assert_eq!(node.to_string(), node_id);
    let host_history = runtime
        .materialize_history(raw)
        .expect("materialize host history before pending tool output");
    let toolcall_start = host_history.len();
    let source_plan = runtime
        .build_close_source_plan(&host_history, &node, suffix_start, toolcall_start, call_id)
        .expect("build close source plan");
    memory_assembly_with_ranges(
        node_id,
        source_plan.source_context_range,
        source_plan.source_raw_range,
    )
}

fn pending_close_source_plan(
    runtime: &SpineRuntime,
    host_history: &[ResponseItem],
    call_id: &str,
    node_id: &str,
) -> SpineCompactSourcePlan {
    let (node, suffix_start) = match runtime
        .pending_commit(call_id)
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close {
            node, suffix_start, ..
        }) => (node, suffix_start),
        other => panic!("expected pending close, got {other:?}"),
    };
    assert_eq!(node.to_string(), node_id);
    let toolcall_start = host_history
        .iter()
        .position(|item| matches!(item, ResponseItem::FunctionCall { call_id: existing, .. } if existing == call_id))
        .unwrap_or(host_history.len());
    runtime
        .build_close_source_plan(host_history, &node, suffix_start, toolcall_start, call_id)
        .expect("build close source plan")
}

fn close_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    node_id: &str,
) {
    observe_spine_request(runtime, raw, SPINE_TOOL_CLOSE, call_id);
    runtime
        .stage_close(call_id.to_string(), "test node memory".to_string())
        .expect("stage close");
    let memory_assembly = close_memory_assembly_from_source_plan(runtime, raw, call_id, node_id);

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output(call_id, Some(memory_assembly))
        .expect("commit close");
}

fn close_task_with_token_baselines(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    node_id: &str,
    token_baselines: SpineTokenBaselines,
) {
    observe_spine_request(runtime, raw, SPINE_TOOL_CLOSE, call_id);
    runtime
        .stage_close(call_id.to_string(), "test node memory".to_string())
        .expect("stage close");
    let memory_assembly = close_memory_assembly_from_source_plan(runtime, raw, call_id, node_id);

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output_with_token_baselines(call_id, Some(memory_assembly), token_baselines)
        .expect("commit close");
}

fn next_task(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    closing_node_id: &str,
    next_summary: &str,
) -> SpineCommitKind {
    observe_spine_request(runtime, raw, SPINE_TOOL_NEXT, call_id);
    runtime
        .stage_next(
            call_id.to_string(),
            next_summary.to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");
    let memory_assembly =
        close_memory_assembly_from_source_plan(runtime, raw, call_id, closing_node_id);

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output(call_id, Some(memory_assembly))
        .expect("commit next")
        .expect("next should commit")
}

fn next_task_with_token_baselines(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    call_id: &str,
    closing_node_id: &str,
    next_summary: &str,
    token_baselines: SpineTokenBaselines,
) -> SpineCommitKind {
    observe_spine_request(runtime, raw, SPINE_TOOL_NEXT, call_id);
    runtime
        .stage_next(
            call_id.to_string(),
            next_summary.to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");
    let memory_assembly =
        close_memory_assembly_from_source_plan(runtime, raw, call_id, closing_node_id);

    observe_function_output(runtime, raw, call_id);
    runtime
        .maybe_commit_output_with_token_baselines(call_id, Some(memory_assembly), token_baselines)
        .expect("commit next")
        .expect("next should commit")
}

fn snapshot_nodes_by_id(snapshot: &SpineTreeUpdateEvent) -> BTreeMap<&str, &SpineTreeNodeSnapshot> {
    snapshot
        .nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect()
}

fn assert_snapshot_is_self_contained_forest(snapshot: &SpineTreeUpdateEvent) {
    let ids = snapshot
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<BTreeSet<_>>();
    for node in &snapshot.nodes {
        if let Some(parent_id) = node.parent_id.as_deref() {
            assert!(
                ids.contains(parent_id),
                "dangling parent {parent_id} in {snapshot:?}"
            );
        }
    }
}

// Clone and fork sidecar behavior.

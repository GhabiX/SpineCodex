use super::*;
use crate::spine::CHECKPOINT_VERSION;
use crate::spine::SpineCloneBoundary;
use crate::spine::archive::tree_meta;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::compact_checkpoint::CompactCheckpointMemoryItemRef;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::io::hash_response_items;
use crate::spine::model::PressureEvent;
use crate::spine::model::ToolCallEventSegment;
use crate::spine::model::ToolCallSegment;
use crate::spine::model::TrimResponseKind;
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

mod checkpoint_failures;
mod error_classification;
mod runtime_lifecycle;

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

// Error classification and fail-closed boundaries.

#[test]
fn m0_trace_golden_baseline_projects_tokens_to_hps() {
    let dir = tempfile::tempdir().expect("tempdir");

    {
        let rollout = dir.path().join("message-tool.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        append_msg(&mut runtime, &mut raw, "m0 user message");

        let request_context = current_context_len(&runtime, &raw);
        let (request, request_raw, _) = observe_item_at_context_index(
            &mut runtime,
            &mut raw,
            ordinary_call("shell_command", "m0-tool"),
            request_context,
        );
        let (output, output_raw, output_context) = observe_item_at_context_index(
            &mut runtime,
            &mut raw,
            function_output_text("m0-tool", "pwd ok"),
            request_context + 1,
        );
        runtime
            .observe_completed_toolcall(completed_toolcall(
                "m0-tool",
                vec![
                    tool_req(request_raw, request_context),
                    tool_resp(output_raw, output_context),
                ],
            ))
            .expect("observe ordinary toolcall");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "user:m0 user message".to_string(),
                response_item_trace_signature(&request),
                response_item_trace_signature(&output),
            ],
            "ordinary msg/tool trace must project as raw msg plus one completed toolcall leaf"
        );
    }

    {
        let rollout = dir.path().join("open.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        open_task(&mut runtime, &mut raw, "m0-open", "m0 child");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "spine-call:open:m0-open".to_string(),
                "tool-output:m0-open:ok".to_string(),
            ],
            "open emits open toolcall and makes that complete toolcall the child leaf"
        );
    }

    {
        let rollout = dir.path().join("close.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        append_msg(&mut runtime, &mut raw, "m0 close body");
        close_task(&mut runtime, &mut raw, "m0-close", "1.1");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "memory:# Spine Memory 1.1".to_string(),
                "spine-call:close:m0-close".to_string(),
                "tool-output:m0-close:ok".to_string(),
            ],
            "close emits close toolcall, replaces live suffix with one memory, and keeps the carrier toolcall in the parent"
        );
    }

    {
        let rollout = dir.path().join("next.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        append_msg(&mut runtime, &mut raw, "m0 next body");
        next_task(&mut runtime, &mut raw, "m0-next", "1.1", "m0 sibling");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "memory:# Spine Memory 1.1".to_string(),
                "spine-call:next:m0-next".to_string(),
                "tool-output:m0-next:ok".to_string(),
            ],
            "next emits close open toolcall, replaces the closed suffix with memory, and makes the carrier toolcall the sibling's first leaf"
        );
    }

    {
        let rollout = dir.path().join("grouped-close.jsonl");
        let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
        let mut raw = Vec::new();
        append_msg(&mut runtime, &mut raw, "m0 grouped close body");
        let (_close_request, close_request_raw, close_request_context) =
            observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "m0-group-close");
        runtime
            .stage_close(
                "m0-group-close".to_string(),
                "m0 grouped close memory".to_string(),
            )
            .expect("stage grouped close");
        let (_ordinary_request, ordinary_request_raw, ordinary_request_context) =
            observe_item_at_context_index(
                &mut runtime,
                &mut raw,
                ordinary_call("shell_command", "m0-group-tool"),
                close_request_context + 1,
            );
        let memory_assembly =
            close_memory_assembly_from_source_plan(&runtime, &raw, "m0-group-close", "1.1");
        let (_close_output, close_output_raw, close_output_context) = observe_item_at_context_index(
            &mut runtime,
            &mut raw,
            function_output("m0-group-close"),
            close_request_context + 2,
        );
        let (_ordinary_output, ordinary_output_raw, ordinary_output_context) =
            observe_item_at_context_index(
                &mut runtime,
                &mut raw,
                function_output_text("m0-group-tool", "group tool ok"),
                close_request_context + 3,
            );
        runtime
            .maybe_commit_output_with_toolcall_and_raw_items(
                "m0-group-close",
                Some(memory_assembly),
                SpineTokenBaselines::default(),
                CompletedToolCall {
                    call_id: "m0-group-close".to_string(),
                    request_call_ids: vec![
                        "m0-group-close".to_string(),
                        "m0-group-tool".to_string(),
                    ],
                    segments: vec![
                        CompletedToolCallSegment {
                            kind: ToolCallSegmentKind::Request,
                            raw_ordinal: close_request_raw,
                            context_index: close_request_context,
                        },
                        CompletedToolCallSegment {
                            kind: ToolCallSegmentKind::Request,
                            raw_ordinal: ordinary_request_raw,
                            context_index: ordinary_request_context,
                        },
                        CompletedToolCallSegment {
                            kind: ToolCallSegmentKind::Response,
                            raw_ordinal: close_output_raw,
                            context_index: close_output_context,
                        },
                        CompletedToolCallSegment {
                            kind: ToolCallSegmentKind::Response,
                            raw_ordinal: ordinary_output_raw,
                            context_index: ordinary_output_context,
                        },
                    ],
                },
                &raw,
            )
            .expect("commit grouped close")
            .expect("grouped close should commit");

        assert_eq!(
            materialized_trace_signature(&runtime, &raw),
            vec![
                "memory:# Spine Memory 1.1".to_string(),
                "spine-call:close:m0-group-close".to_string(),
                "tool-call:shell_command:m0-group-tool".to_string(),
                "tool-output:m0-group-close:ok".to_string(),
                "tool-output:m0-group-tool:group tool ok".to_string(),
            ],
            "grouped close must keep the full completed toolcall as one parent leaf after the close reduction"
        );
    }
}

#[test]
fn close_commit_without_completed_toolcall_evidence_does_not_write_marker_or_clear_anchor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "live suffix before close");
    observe_spine_request(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_CLOSE,
        "close-missing-carrier",
    );
    runtime
        .stage_close(
            "close-missing-carrier".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("close-missing-carrier")
        .expect("pending close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };

    let before_events = event_log_debug(&runtime);
    let before_stack = runtime.parse_stack().clone();
    let before_tree = runtime.render_tree().expect("render before failure");
    let err = runtime
        .maybe_commit_output(
            "close-missing-carrier",
            Some(memory_assembly_with_ranges("1.1", suffix_start..1, 0..1)),
        )
        .expect_err("close must not commit without completed toolcall evidence");
    assert!(
        err.to_string()
            .contains("spine.close commit requires completed toolcall evidence"),
        "unexpected close error: {err}"
    );
    assert!(
        runtime
            .store
            .commit_markers_for_test()
            .expect("read markers")
            .is_empty(),
        "failed close must not publish a commit marker"
    );
    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &before_stack,
        &before_tree,
        &before_events,
    );

    let (_output, output_raw, output_index) =
        observe_function_output(&mut runtime, &mut raw, "close-missing-carrier");
    runtime
        .maybe_commit_output_with_toolcall(
            "close-missing-carrier",
            Some(memory_assembly_with_ranges("1.1", suffix_start..1, 0..1)),
            SpineTokenBaselines::default(),
            CompletedToolCall {
                call_id: "close-missing-carrier".to_string(),
                request_call_ids: vec!["close-missing-carrier".to_string()],
                segments: vec![
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Request,
                        raw_ordinal: 1,
                        context_index: 1,
                    },
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: output_raw,
                        context_index: output_index,
                    },
                ],
            },
        )
        .expect("retry with durable carrier commits")
        .expect("commit kind");

    let markers = runtime
        .store
        .commit_markers_for_test()
        .expect("read markers after retry");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::Close);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 2);
    assert!(matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::ToolCall { segments })
            if segments == &vec![event_tool_req(1, 1), event_tool_resp(output_raw, output_index as u64)]
    ));
}

#[test]
fn next_commit_without_completed_toolcall_evidence_does_not_write_marker_or_open_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "live suffix before next");
    observe_spine_request(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_NEXT,
        "next-missing-carrier",
    );
    runtime
        .stage_next(
            "next-missing-carrier".to_string(),
            "retry sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");
    let suffix_start = match runtime
        .pending_commit("next-missing-carrier")
        .expect("pending next")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close-like next, got {other:?}"),
    };

    let before_events = event_log_debug(&runtime);
    let before_stack = runtime.parse_stack().clone();
    let before_tree = runtime.render_tree().expect("render before failure");
    let err = runtime
        .maybe_commit_output(
            "next-missing-carrier",
            Some(memory_assembly_with_ranges("1.1", suffix_start..1, 0..1)),
        )
        .expect_err("next must not commit without completed toolcall evidence");
    assert!(
        err.to_string()
            .contains("spine.next commit requires completed toolcall evidence"),
        "unexpected next error: {err}"
    );
    assert!(
        runtime
            .store
            .commit_markers_for_test()
            .expect("read markers")
            .is_empty(),
        "failed next must not publish a commit marker"
    );
    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &before_stack,
        &before_tree,
        &before_events,
    );

    let (_output, output_raw, output_index) =
        observe_function_output(&mut runtime, &mut raw, "next-missing-carrier");
    runtime
        .maybe_commit_output_with_toolcall(
            "next-missing-carrier",
            Some(memory_assembly_with_ranges("1.1", suffix_start..1, 0..1)),
            SpineTokenBaselines::default(),
            CompletedToolCall {
                call_id: "next-missing-carrier".to_string(),
                request_call_ids: vec!["next-missing-carrier".to_string()],
                segments: vec![
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Request,
                        raw_ordinal: 1,
                        context_index: 1,
                    },
                    CompletedToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: output_raw,
                        context_index: output_index,
                    },
                ],
            },
        )
        .expect("retry with durable carrier commits")
        .expect("commit kind");

    let markers = runtime
        .store
        .commit_markers_for_test()
        .expect("read markers after retry");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::CloseThenOpen);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 3);
    assert!(matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::ToolCall { segments })
            if segments == &vec![event_tool_req(1, 1), event_tool_resp(output_raw, output_index as u64)]
    ));
}

#[test]
fn close_prepare_store_failure_retains_retryable_close_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-before-close-fail", "child");
    append_msg(&mut runtime, &mut raw, "child work before close failure");
    let (_request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-store-fail");
    runtime
        .stage_close(
            "close-store-fail".to_string(),
            "memory that will fail before commit".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "close-store-fail", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "close-store-fail");

    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-close");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "close-store-fail",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "close-store-fail",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect_err("close prepare must fail while writing sidecar memory");
    assert_pending_close_retry_state(&runtime, &before_events);
}

#[test]
fn next_prepare_store_failure_retains_retryable_close_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-before-next-fail", "child");
    append_msg(&mut runtime, &mut raw, "child work before next failure");
    let (_request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_NEXT, "next-store-fail");
    runtime
        .stage_next(
            "next-store-fail".to_string(),
            "sibling that must not be installed".to_string(),
            "memory that will fail before next commit".to_string(),
        )
        .expect("stage next");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "next-store-fail", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "next-store-fail");

    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-next");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "next-store-fail",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "next-store-fail",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect_err("next prepare must fail while writing sidecar memory");
    assert_pending_close_retry_state(&runtime, &before_events);
}

#[test]
fn spine_error_classifies_missing_raw_coverage_as_sidecar_corruption() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    let raw = vec![Some(text_item("uncovered durable item"))];

    let err = runtime
        .validate_raw_coverage(&raw)
        .expect_err("missing durable raw coverage must fail closed");
    assert_eq!(err.class(), SpineErrorClass::SidecarCorruption);
    assert!(err.should_invalidate_runtime());
    assert!(
        err.to_string()
            .contains("spine sidecar is missing token coverage for raw ordinal 0"),
        "unexpected coverage error: {err}"
    );
    assert!(err.to_string().contains("token_seq="));
}

#[test]
fn close_commit_marker_is_required_for_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before close");
    close_task(&mut runtime, &mut raw, "close-marker", "1.1");

    let markers = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::Close);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 2);
    assert_eq!(markers[0].memory_refs.len(), 1);

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("Close ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for Close ledger event"),
        "unexpected resume error: {err}"
    );
}

#[test]
fn next_commit_marker_covers_close_then_open_without_next_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before next");
    next_task(&mut runtime, &mut raw, "next-marker", "1.1", "next sibling");

    let markers = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::CloseThenOpen);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 3);
    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Close { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::ToolCall { .. },
        ]
    ));

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("Close+Open ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for Close ledger event"),
        "unexpected resume error: {err}"
    );
}

#[test]
fn close_marker_does_not_replay_structural_close_without_live_toolcall_carrier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root child work before close");
    close_task(&mut runtime, &mut raw, "close-carrier-live", "1.1");
    let full_history = runtime
        .materialize_history(&raw)
        .expect("materialize closed history");
    assert_eq!(full_history.len(), 3);

    let err = SpineRuntime::load_with_raw_live_and_event_limit(
        SpineStore::for_rollout(&rollout).expect("source store"),
        vec![true, false, false],
        None,
    )
    .expect_err("replay with stale close carrier raw must fail closed");
    assert!(
        err.to_string().contains("raw-backed event at token_seq"),
        "unexpected stale close carrier replay error: {err}"
    );
}

#[test]
fn commit_marker_replay_classifies_committed_and_uncommitted_proof() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before replay classification",
    );
    close_task(&mut runtime, &mut raw, "close-replay-classification", "1.1");

    let marker = runtime
        .store
        .commit_markers_for_test()
        .expect("read close marker")
        .into_iter()
        .next()
        .expect("close marker should exist");
    let structural_event_seqs =
        commit_marker_structural_event_seqs(&marker).expect("marker structural seqs");
    let events = runtime
        .store
        .events()
        .expect("read events")
        .into_iter()
        .map(|event| (event.seq, event))
        .collect::<BTreeMap<_, _>>();
    let events_by_seq = events
        .iter()
        .map(|(seq, event)| (*seq, event))
        .collect::<BTreeMap<_, _>>();
    let mems = runtime
        .store
        .mems()
        .expect("read mem records")
        .into_iter()
        .map(|mem| (mem.compact_id.clone(), mem))
        .collect::<BTreeMap<_, _>>();
    let mems_by_id = mems
        .iter()
        .map(|(compact_id, mem)| (compact_id.as_str(), mem))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        classify_commit_marker_for_replay(
            &marker,
            &structural_event_seqs,
            &events_by_seq,
            &mems_by_id,
            RawMask::new(&[true, true, true]),
            false,
        )
        .expect("classify committed marker"),
        ReplayCommitClassification::Committed
    );
    assert_eq!(
        classify_commit_marker_for_replay(
            &marker,
            &structural_event_seqs,
            &events_by_seq,
            &mems_by_id,
            RawMask::new(&[true, false, false]),
            false,
        )
        .expect("classify uncommitted marker"),
        ReplayCommitClassification::Uncommitted
    );
}

#[test]
fn clone_does_not_copy_marker_structural_close_without_live_toolcall_carrier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let mut runtime = SpineRuntime::load_or_create(&source_rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "source work before close");
    close_task(&mut runtime, &mut raw, "close-not-cloned", "1.1");

    let boundary = SpineStore::clone_boundary_for_rollout(
        &source_rollout,
        u64::try_from(raw.len()).expect("raw len fits u64"),
    )
    .expect("capture clone boundary")
    .expect("source sidecar exists");
    let err = SpineStore::clone_for_rollout_with_raw_live(
        &boundary,
        &target_rollout,
        &[true, false, false],
    )
    .expect_err("clone sidecar without close carrier must fail closed");
    assert!(
        err.to_string().contains("clone raw live state"),
        "unexpected stale close carrier clone error: {err}"
    );
}

#[test]
fn root_compact_commit_marker_is_required_for_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root visible work before compact");
    runtime
        .root_compact("root compact marker body".to_string(), &raw)
        .expect("root compact");

    let markers = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers");
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].kind, SpineCommitKindMarker::RootCompact);
    assert_eq!(markers[0].token_seq_end, markers[0].token_seq_start + 1);
    assert!(markers[0].raw_live_hash.is_some());

    std::fs::remove_file(runtime.store.commit_path_for_test()).expect("remove commit markers");
    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("RootCompact ledger without commit marker must fail closed");
    assert!(
        err.to_string()
            .contains("missing Spine commit marker for RootCompact ledger event"),
        "unexpected resume error: {err}"
    );
}

#[test]
fn resume_ambiguous_partial_commit_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before ambiguous marker",
    );
    close_task(&mut runtime, &mut raw, "close-ambiguous-marker", "1.1");

    let mut duplicate = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers")
        .into_iter()
        .next()
        .expect("close marker should exist");
    duplicate.op_id = "duplicate-close-marker".to_string();
    runtime
        .store
        .append_commit_marker(&duplicate)
        .expect("append duplicate marker");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("ambiguous duplicate commit markers must fail closed");
    assert!(
        err.to_string()
            .contains("ambiguous Spine commit marker at token_seq"),
        "unexpected resume error: {err}"
    );
}

#[test]
fn resume_rejects_missing_memory_artifact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(
        &mut runtime,
        &mut raw,
        "root child work before missing memory",
    );
    close_task(&mut runtime, &mut raw, "close-missing-memory", "1.1");

    let marker = runtime
        .store
        .commit_markers_for_test()
        .expect("read commit markers")
        .into_iter()
        .next()
        .expect("close marker should exist");
    let memory = marker
        .memory_refs
        .first()
        .expect("close marker should reference memory");
    std::fs::remove_file(runtime.store.root.join(&memory.body_path))
        .expect("remove committed memory body");

    SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("missing committed memory artifact must fail closed");
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

#[test]
fn control_tool_receipt_defers_spine_transition_until_tool_output_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    let parse_stack_before_receipt = runtime.parse_stack().clone();
    let event_log_before_receipt = event_log_debug(&runtime);

    runtime
        .record_close_tool_receipt("close".to_string(), "test node memory".to_string())
        .expect("record close receipt");

    assert_eq!(runtime.parse_stack(), &parse_stack_before_receipt);
    assert_eq!(event_log_debug(&runtime), event_log_before_receipt);
    assert!(runtime.has_close_like_control_receipt("close"));
    assert!(matches!(
        runtime
            .pending_commit("close")
            .expect("receipt pending view"),
        Some(SpinePendingCommit::Close { .. })
    ));

    let memory_assembly = close_memory_assembly_from_source_plan(&runtime, &raw, "close", "1.1.1");
    observe_function_output(&mut runtime, &mut raw, "close");
    runtime
        .maybe_commit_output("close", Some(memory_assembly))
        .expect("commit receipt-backed close");

    assert!(!runtime.has_close_like_control_receipt("close"));
    assert!(
        runtime
            .pending_commit("close")
            .expect("receipt consumed")
            .is_none()
    );
    assert_ne!(runtime.parse_stack(), &parse_stack_before_receipt);
}

#[test]
fn duplicate_control_tool_receipt_preserves_original_receipt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .record_close_tool_receipt("close".to_string(), "first memory".to_string())
        .expect("record first receipt");

    let err = runtime
        .record_close_tool_receipt("close".to_string(), "second memory".to_string())
        .expect_err("duplicate receipt must fail");
    assert!(err.to_string().contains("duplicate Spine control receipt"));
    assert!(matches!(
        runtime.pending_commit("close").expect("receipt pending view"),
        Some(SpinePendingCommit::Close { memory, .. }) if memory == "first memory"
    ));
}

#[test]
fn abort_pending_clears_receipt_before_it_becomes_pending_transition() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(&mut runtime, &mut raw, "open", "child task");
    append_msg(&mut runtime, &mut raw, "work inside child");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .record_close_tool_receipt("close".to_string(), "test node memory".to_string())
        .expect("record close receipt");

    assert!(runtime.has_close_like_control_receipt("close"));
    assert!(runtime.abort_pending("close"));
    assert!(!runtime.has_close_like_control_receipt("close"));
    assert!(!runtime.control_call_ids.contains("close"));
    assert!(
        runtime
            .pending_commit("close")
            .expect("cleared receipt")
            .is_none()
    );
}

#[test]
fn prepare_close_commit_does_not_install_final_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task(
        &mut runtime,
        &mut raw,
        "open-staged-close",
        "staged close child",
    );
    append_msg(&mut runtime, &mut raw, "child work before staged close");
    let (request, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "staged-close");
    runtime
        .stage_close(
            "staged-close".to_string(),
            "staged close memory".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "staged-close", "1.1.1");
    let (_output, output_raw, output_context) =
        observe_function_output(&mut runtime, &mut raw, "staged-close");
    let before_tree = runtime
        .render_tree()
        .expect("render before prepared commit");

    let prepared = runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "staged-close",
            Some(memory_assembly),
            SpineTokenBaselines::default(),
            completed_toolcall(
                "staged-close",
                vec![
                    tool_segment(ToolCallSegmentKind::Request, request_raw, request_context),
                    tool_segment(ToolCallSegmentKind::Response, output_raw, output_context),
                ],
            ),
            &raw,
        )
        .expect("prepare close commit")
        .expect("prepared close commit");
    assert!(matches!(prepared.kind(), SpineCommitKind::Close { .. }));
    let publication_plan = prepared
        .publication_plan()
        .expect("close commit should carry publication plan");
    assert_eq!(publication_plan.operation(), "spine.close");
    assert_eq!(publication_plan.suffix_start(), 0);
    assert_eq!(publication_plan.replacement_prefix().len(), 1);
    assert_eq!(
        publication_plan.preserve_host_history_from(),
        request_context
    );
    assert!(
        publication_plan.append_current_tool_response_if_missing(),
        "close publication should append current output when host has not recorded it"
    );
    assert_eq!(
        runtime.render_tree().expect("render after prepared commit"),
        before_tree,
        "prepared close commit must not install the reduced ParseStack before host publication"
    );
    let before_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot before installing prepared commit");
    let before_nodes = snapshot_nodes_by_id(&before_snapshot);
    assert_ne!(
        before_nodes["1.1.1"].status,
        SpineTreeNodeStatus::Closed,
        "live tree must not expose closed-node publication before install"
    );

    runtime
        .persist_prepared_commit_side_effects(&prepared)
        .expect("persist prepared close side effects");
    runtime.install_prepared_commit(prepared);
    let after_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after installing prepared commit");
    let after_nodes = snapshot_nodes_by_id(&after_snapshot);
    assert_eq!(
        after_nodes["1.1.1"].status,
        SpineTreeNodeStatus::Closed,
        "installing prepared close commit should advance the live ParseStack"
    );
    assert_eq!(request, spine_call(SPINE_TOOL_CLOSE, "staged-close"));
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

// Tree projection and context accounting.

#[test]
fn initial_tree_snapshot_projects_root_epoch_with_live_first_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "1.1");
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes["1"].parent_id, None);
    assert_eq!(nodes["1"].summary, None);
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["1.1"].parent_id.as_deref(), Some("1"));
    assert_eq!(nodes["1.1"].summary, None);
    assert_eq!(nodes["1.1"].status, SpineTreeNodeStatus::Live);
}

#[test]
fn nested_tree_snapshot_promotes_only_missing_projection_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-child", "child task");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "1.1.1");
    assert_eq!(nodes["1"].parent_id, None);
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["1.1"].parent_id.as_deref(), Some("1"));
    assert_eq!(nodes["1.1"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["1.1.1"].parent_id.as_deref(), Some("1.1"));
    assert_eq!(nodes["1.1.1"].summary.as_deref(), Some("child task"));
    assert_eq!(nodes["1.1.1"].status, SpineTreeNodeStatus::Live);
}

#[test]
fn root_compact_tree_snapshot_promotes_new_root_epoch_holder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "root work");
    runtime
        .root_compact("root summary".to_string(), &raw)
        .expect("compact root");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "2.1");
    assert_eq!(nodes["1"].parent_id, None);
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Compacted);
    assert_eq!(nodes["2"].parent_id, None);
    assert_eq!(nodes["2"].summary, None);
    assert_eq!(nodes["2"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["2.1"].parent_id.as_deref(), Some("2"));
    assert_eq!(nodes["2.1"].summary, None);
    assert_eq!(nodes["2.1"].status, SpineTreeNodeStatus::Live);
}

#[test]
fn closed_child_tree_snapshot_keeps_visible_parent_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-child", "child task");
    append_msg(&mut runtime, &mut raw, "child work");
    close_task(&mut runtime, &mut raw, "close-child", "1.1.1");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "1.1");
    assert_eq!(nodes["1"].parent_id, None);
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Opened);
    assert_eq!(nodes["1.1"].parent_id.as_deref(), Some("1"));
    assert_eq!(nodes["1.1"].status, SpineTreeNodeStatus::Live);
    assert_eq!(nodes["1.1.1"].parent_id.as_deref(), Some("1.1"));
    assert_eq!(nodes["1.1.1"].summary.as_deref(), Some("child task"));
    assert_eq!(nodes["1.1.1"].status, SpineTreeNodeStatus::Closed);
}

#[test]
fn tree_snapshot_hides_closed_historical_subtree_descendants() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(
        &mut runtime,
        &mut raw,
        "open-historical",
        "historical parent",
    );
    open_task(&mut runtime, &mut raw, "open-child", "historical child");
    append_msg(&mut runtime, &mut raw, "historical child work");
    close_task(&mut runtime, &mut raw, "close-child", "1.1.1.1");
    append_msg(&mut runtime, &mut raw, "historical parent work");
    next_task(
        &mut runtime,
        &mut raw,
        "next-sibling",
        "1.1.1",
        "current sibling",
    );
    open_task(&mut runtime, &mut raw, "open-active-child", "active child");

    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("[1.1.1] Done historical parent"), "{tree}");
    assert!(!tree.contains("[1.1.1.1] Done historical child"), "{tree}");

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);

    assert_eq!(snapshot.active_node_id, "1.1.2.1");
    assert!(nodes.contains_key("1"));
    assert!(nodes.contains_key("1.1"));
    assert_eq!(nodes["1.1.1"].parent_id.as_deref(), Some("1.1"));
    assert_eq!(nodes["1.1.1"].status, SpineTreeNodeStatus::Closed);
    assert_eq!(nodes["1.1.1"].summary.as_deref(), Some("historical parent"));
    assert!(nodes.contains_key("1.1.2"));
    assert!(nodes.contains_key("1.1.2.1"));
    assert!(
        !nodes.contains_key("1.1.1.1"),
        "closed historical descendants must stay out of the TUI snapshot: {snapshot:?}"
    );
}

#[test]
fn closed_child_tree_records_raw_and_memory_context_accounting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "accounted child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output_with_open_input_tokens("open", None, Some(10_000))
        .expect("commit open");

    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output_with_open_input_tokens(
            "close",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                suffix_start..close_request_index,
            )),
            Some(17_500),
        )
        .expect("commit close");

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("closed child should reduce into ParseStack nodes")
    };
    let memory = nodes
        .iter()
        .find_map(|node| match node {
            SpineTreeNode::SpineTree { memory, .. } => Some(memory),
            _ => None,
        })
        .expect("closed child memory ref");
    assert_eq!(memory.open_input_tokens, Some(10_000));
    assert_eq!(memory.close_input_tokens, Some(17_500));
    assert_eq!(memory.closed_memory_context_tokens, None);
    let memory_output_tokens = memory
        .memory_output_tokens
        .expect("memory output token count");
    assert_eq!(memory_output_tokens, 1_250);

    let captured = runtime
        .capture_closed_memory_context_accounting(1_250)
        .expect("capture closed memory accounting");
    assert!(captured);
    let accounting = runtime.store.mem_accounting().expect("memory accounting");
    assert_eq!(accounting.len(), 1);
    assert_eq!(accounting[0].closed_memory_context_tokens, 1_250);
    assert_eq!(accounting[0].provider_input_tokens, 1_250);
    assert_eq!(accounting[0].replacement_prefix_baseline_tokens, 0);

    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("[1.1.1] Done accounted child"), "{tree}");
    assert!(
        tree.contains("(~7.50K source -> ~1.25K memory context)"),
        "{tree}"
    );
    let materialized_before_snapshot = runtime
        .materialize_history(&raw)
        .expect("materialize before snapshot");
    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("materialize after snapshot"),
        materialized_before_snapshot,
        "tree snapshot accounting must remain projection-only and not change h(PS)"
    );
    let snapshot_nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(
        snapshot_nodes["1.1.1"].accounting,
        Some(SpineTreeNodeAccountingSnapshot {
            current_node_context_tokens: None,
            current_node_context_problem: None,
            current_node_context_baseline_source: None,
            closed_source_suffix_tokens: Some(7_500),
            closed_memory_context_tokens: Some(1_250),
            memory_output_tokens: Some(1_250),
        })
    );

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let replayed_tree = replayed.render_tree().expect("render replayed tree");
    assert!(
        replayed_tree.contains("(~7.50K source -> ~1.25K memory context)"),
        "{replayed_tree}"
    );
    let replayed_snapshot = replayed.build_tree_snapshot().expect("replay snapshot");
    let replayed_nodes = snapshot_nodes_by_id(&replayed_snapshot);
    assert_eq!(
        replayed_nodes["1.1.1"].accounting,
        snapshot_nodes["1.1.1"].accounting
    );
    let materialized = replayed.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 3);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));
    assert_eq!(materialized[1], spine_call(SPINE_TOOL_CLOSE, "close"));
    assert_eq!(materialized[2], function_output("close"));
}

#[test]
fn close_source_plan_uses_current_hps_projection_indices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    for index in 0..5 {
        append_msg_with_context_index(&mut runtime, &mut raw, &format!("prefix {index}"), index);
    }
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_OPEN, "open-gap");
    runtime
        .stage_open("open-gap".to_string(), "gap task".to_string())
        .expect("stage open");
    observe_function_output(&mut runtime, &mut raw, "open-gap");
    runtime
        .maybe_commit_output("open-gap", None)
        .expect("commit open");

    append_msg(&mut runtime, &mut raw, "first live item");
    append_msg(&mut runtime, &mut raw, "second live item");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-gap");
    runtime
        .stage_close("close-gap".to_string(), "test node memory".to_string())
        .expect("stage close");

    let host_history = runtime
        .materialize_history(&raw)
        .expect("materialize current h(PS)");

    let source_plan = pending_close_source_plan(&runtime, &host_history, "close-gap", "1.1.1");
    let contexts = source_plan
        .entries
        .iter()
        .map(|entry| entry.context_index)
        .collect::<Vec<_>>();
    assert_eq!(contexts, vec![5, 6, 7, 8]);
    assert_eq!(source_plan.source_context_range, 5..9);
    assert_eq!(source_plan.source_raw_range, 5..9);
    let user_evidence = source_plan
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            SpineCompactSourceEntryKind::RawResponseItem {
                item,
                from_user: true,
                user_anchor,
                ..
            } => Some((item, user_anchor)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_evidence.len(), 2);
    assert_eq!(user_evidence[0].1, &Some(6));
    assert!(matches!(
        user_evidence[0].0,
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }] if text == "[U6]\nfirst live item"
            )
    ));
    assert_eq!(user_evidence[1].1, &Some(7));
    assert!(matches!(
        user_evidence[1].0,
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }] if text == "[U7]\nsecond live item"
            )
    ));
}

#[test]
fn closed_memory_context_accounting_rejects_invalid_first_provider_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-accounted-child",
        "accounted child",
        SpineTokenBaselines {
            provider_input_tokens: Some(10_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "inside");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-accounted-child",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(17_500),
        },
    );

    let captured = runtime
        .capture_closed_memory_context_accounting(17_500)
        .expect("invalid provider usage should not corrupt accounting");
    assert!(!captured);
    assert!(
        runtime
            .store
            .mem_accounting()
            .expect("memory accounting")
            .is_empty()
    );
    let tree = runtime.render_tree().expect("render tree");
    assert!(
        tree.contains("(~7.50K source -> ~1.25K memory output)"),
        "{tree}"
    );

    let second_capture = runtime
        .capture_closed_memory_context_accounting(1_250)
        .expect("first provider usage decision is single-shot");
    assert!(!second_capture);
}

#[test]
fn closed_memory_context_accounting_rejects_negative_memory_delta() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record before");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe before");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "accounted child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output_with_open_input_tokens("open", None, Some(10_000))
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output_with_open_input_tokens(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
            Some(17_500),
        )
        .expect("commit close");

    let captured = runtime
        .capture_closed_memory_context_accounting(9_999)
        .expect("negative memory delta should not corrupt accounting");
    assert!(!captured);
    assert!(
        runtime
            .store
            .mem_accounting()
            .expect("memory accounting")
            .is_empty()
    );

    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 4);
}

#[test]
fn closed_memory_context_accounting_missing_usage_consumes_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-poc-missing-usage",
        "poc missing usage",
        SpineTokenBaselines {
            provider_input_tokens: Some(10_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "inside");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-poc-missing-usage",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(17_500),
        },
    );

    assert!(
        runtime
            .store
            .mem_accounting()
            .expect("memory accounting")
            .is_empty(),
        "close should only stage pending accounting until a provider usage arrives"
    );

    let consumed = runtime
        .consume_closed_memory_context_accounting_without_provider_usage()
        .expect("missing provider usage consumes pending accounting");
    assert!(consumed);

    let captured = runtime
        .capture_closed_memory_context_accounting(2_500)
        .expect("later usage must not be accepted as first provider usage");
    assert!(
        !captured,
        "missing first provider usage must consume pending accounting"
    );
    let accounting = runtime.store.mem_accounting().expect("memory accounting");
    assert!(
        accounting.is_empty(),
        "missing provider usage must not fabricate a memory context size"
    );
}

#[test]
fn closed_memory_context_accounting_pending_survives_reload_before_first_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-poc-reload",
        "poc reload",
        SpineTokenBaselines {
            provider_input_tokens: Some(10_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "inside");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-poc-reload",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(17_500),
        },
    );
    assert!(
        runtime
            .store
            .mem_accounting()
            .expect("memory accounting")
            .is_empty(),
        "fixture should close memory before post-close provider usage"
    );
    let raw_len = runtime.raw_len;
    drop(runtime);

    let mut reloaded = eventually_load_or_create_writer(&rollout, raw_len);
    let captured = reloaded
        .capture_closed_memory_context_accounting(1_250)
        .expect("capture after reload should use durable pending accounting");
    assert!(
        captured,
        "pending memory accounting must be reconstructed from the sidecar"
    );
    let accounting = reloaded.store.mem_accounting().expect("memory accounting");
    assert_eq!(accounting.len(), 1);
    assert_eq!(accounting[0].closed_memory_context_tokens, 1_250);
}

#[test]
fn prepared_commit_side_effect_failure_leaves_parse_stack_unadvanced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.set_trim_enabled(true);

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-poc-install-fail",
        "poc install fail",
        SpineTokenBaselines {
            provider_input_tokens: Some(10_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "inside");
    observe_spine_request(
        &mut runtime,
        &mut raw,
        SPINE_TOOL_CLOSE,
        "close-poc-install-fail",
    );
    runtime
        .stage_close(
            "close-poc-install-fail".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage close");
    let memory_assembly =
        close_memory_assembly_from_source_plan(&runtime, &raw, "close-poc-install-fail", "1.1.1");
    let close_output = function_output_text(
        "close-poc-install-fail",
        &"large close output for trim candidate ".repeat(40),
    );
    let output_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits");
    let output_context_index = current_context_len(&runtime, &raw)
        .checked_add(1)
        .expect("output context index fits");
    raw.push(Some(close_output));
    runtime.observe_raw_items(1).expect("record output raw");
    runtime
        .observe_context_item(
            output_ordinal,
            output_context_index,
            raw.last()
                .and_then(Option::as_ref)
                .expect("close output item"),
        )
        .expect("observe close output");

    let completed_toolcall = completed_toolcall(
        "close-poc-install-fail",
        vec![
            tool_req(output_ordinal - 1, output_context_index - 1),
            tool_resp(output_ordinal, output_context_index),
        ],
    );
    let prepared = runtime
        .prepare_commit_output_with_toolcall_and_raw_items(
            "close-poc-install-fail",
            Some(memory_assembly),
            SpineTokenBaselines {
                provider_input_tokens: Some(17_500),
            },
            completed_toolcall,
            &raw,
        )
        .expect("prepare close")
        .expect("prepared close");
    let parse_stack_before_install = runtime.parse_stack().clone();
    let tree_before_install = runtime.render_tree().expect("tree before install");
    assert!(
        tree_before_install.contains("[1.1.1] Current poc install fail"),
        "{tree_before_install}"
    );

    let trim_path = runtime.store.trim_path_for_test();
    let parked_trim_path = dir.path().join("parked-trim-before-install.jsonl");
    std::fs::rename(&trim_path, &parked_trim_path).expect("park trim ledger");
    std::fs::create_dir_all(&trim_path).expect("block trim append with directory");

    let err = runtime
        .persist_prepared_commit_side_effects(&prepared)
        .expect_err("trim append failure should fail before install");
    assert!(
        err.to_string().contains("Is a directory")
            || err.to_string().contains("is a directory")
            || err.to_string().contains("directory"),
        "unexpected install error: {err}"
    );
    assert_eq!(
        runtime.parse_stack(),
        &parse_stack_before_install,
        "failed prepared side effects must not advance the parse stack"
    );
    let tree_after_failed_install = runtime
        .render_tree()
        .expect("tree after failed install still renders");
    assert!(
        tree_after_failed_install.contains("[1.1.1] Current poc install fail"),
        "{tree_after_failed_install}"
    );
}

#[test]
fn close_source_plan_rejects_host_history_not_matching_hps_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    for index in 0..5 {
        append_msg_with_context_index(&mut runtime, &mut raw, &format!("prefix {index}"), index);
    }
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_OPEN, "open-dup");
    runtime
        .stage_open(
            "open-dup".to_string(),
            "duplicate provenance task".to_string(),
        )
        .expect("stage open");
    observe_function_output(&mut runtime, &mut raw, "open-dup");
    runtime
        .maybe_commit_output("open-dup", None)
        .expect("commit open");

    append_msg(&mut runtime, &mut raw, "first live item");
    append_msg(&mut runtime, &mut raw, "second live item");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-dup");
    runtime
        .stage_close("close-dup".to_string(), "test node memory".to_string())
        .expect("stage close");

    let mut host_history = runtime
        .materialize_history(&raw)
        .expect("materialize current h(PS)");
    host_history.insert(8, text_item("host item not represented by h(PS)"));

    let (node, suffix_start) = match runtime
        .pending_commit("close-dup")
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close {
            node, suffix_start, ..
        }) => (node, suffix_start),
        other => panic!("expected pending close, got {other:?}"),
    };
    let toolcall_start = host_history
        .iter()
        .position(|item| matches!(item, ResponseItem::FunctionCall { call_id: existing, .. } if existing == "close-dup"))
        .unwrap_or(host_history.len());
    let err = runtime
        .build_close_source_plan(
            &host_history,
            &node,
            suffix_start,
            toolcall_start,
            "close-dup",
        )
        .expect_err("host/projection mismatch must fail");
    assert!(
        err.to_string().contains("h(PS) suffix projects"),
        "unexpected host/projection mismatch error: {err}"
    );
}

#[test]
fn close_source_plan_ignores_stale_persisted_leaf_context_indices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open-stale", "stale index task");
    append_msg(&mut runtime, &mut raw, "first live item");
    append_msg(&mut runtime, &mut raw, "second live item");

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack.symbols.last_mut() else {
        panic!("open task should have live suffix nodes");
    };
    let [
        SpineTreeNode::ToolCallAsLeafNode { segments },
        SpineTreeNode::MsgAsLeafNode { msg: first_msg, .. },
        SpineTreeNode::MsgAsLeafNode {
            msg: second_msg, ..
        },
    ] = nodes.as_mut_slice()
    else {
        panic!("unexpected live suffix nodes: {nodes:?}");
    };
    for (offset, segment) in segments.iter_mut().enumerate() {
        let SegRef::ResponseItem { context_index, .. } = &mut segment.seg else {
            panic!("toolcall segment should be raw response item");
        };
        *context_index = 11 + offset;
    }
    let SegRef::ResponseItem {
        context_index: first_context_index,
        ..
    } = first_msg
    else {
        panic!("first msg should be raw response item");
    };
    *first_context_index = 9;
    let SegRef::ResponseItem {
        context_index: second_context_index,
        ..
    } = second_msg
    else {
        panic!("second msg should be raw response item");
    };
    *second_context_index = 10;

    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-stale");
    runtime
        .stage_close("close-stale".to_string(), "test node memory".to_string())
        .expect("stage close");
    let host_history = runtime
        .materialize_history(&raw)
        .expect("materialize current h(PS)");
    let source_plan = pending_close_source_plan(&runtime, &host_history, "close-stale", "1.1.1");
    let contexts = source_plan
        .entries
        .iter()
        .map(|entry| entry.context_index)
        .collect::<Vec<_>>();
    assert_eq!(contexts, vec![0, 1, 2, 3]);
    assert_eq!(source_plan.source_context_range, 0..4);
}

// Provider input baseline capture and replay.

#[test]
fn provider_input_baseline_capture_records_structural_event() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "live suffix for root");
    let structural_seq_after_msg = runtime
        .build_tree_snapshot()
        .expect("snapshot")
        .snapshot_seq;
    let captured = runtime
        .capture_current_open_provider_baseline(9_000)
        .expect("capture provider baseline");
    assert!(captured);

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_eq!(snapshot.snapshot_seq, structural_seq_after_msg + 1);
    assert_eq!(runtime.store.event_count_for_test().expect("events"), 4);
    assert_eq!(runtime.store.pressure_events().expect("pressure").len(), 0);
    assert_eq!(runtime.current_open_input_tokens(), Some(9_000));
    assert_eq!(runtime.current_open_provider_input_tokens(), Some(9_000));
    assert_eq!(
        runtime.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
}

#[test]
fn provider_input_baseline_replays_after_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "live suffix");
    runtime
        .capture_current_open_provider_baseline(12_000)
        .expect("capture provider baseline");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_input_tokens(), Some(12_000));
    assert_eq!(replayed.current_open_provider_input_tokens(), Some(12_000));
    assert_eq!(
        replayed.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
}

#[test]
fn mismatched_legacy_open_baseline_encoding_fails_replay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    store
        .append_event(&SpineLedgerEvent::Open {
            child: NodeId::root_epoch(1).child(1),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: Some(12_345),
            open_context_tokens: Some(10_000),
            open_context_source: Some(ContextBaselineSource::ProviderAtOpen),
        })
        .expect("append mismatched open");

    let err = SpineRuntime::load_for_rollout(&rollout, 0)
        .expect_err("mismatched provider input encoding must fail closed");
    assert!(
        err.to_string().contains("mismatched provider input"),
        "unexpected error: {err}"
    );
}

#[test]
fn legacy_pressure_ledger_does_not_drive_live_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "live suffix");
    std::fs::write(
        runtime.store.pressure_path_for_test(),
        [
            format!(
                r#"{{"pressure_seq":0,"type":"open_context_baseline","node":[1,1],"observed_structural_seq":{},"observed_raw_ordinal":{},"observed_raw_live_hash":"{}","observed_context_index":{},"context_tokens":7000,"input_tokens":7500,"source":"estimated_from_live_suffix","estimated_live_suffix_tokens":500}}"#,
                runtime.store.next_event_seq().expect("next structural seq"),
                raw.len(),
                hash_raw_live(&vec![true; raw.len()]),
                raw.len()
            ),
            String::new(),
        ]
        .join("\n"),
    )
    .expect("write legacy pressure ledger");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_input_tokens(), None);
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
    assert_eq!(replayed.current_open_context_baseline_source(), None);
}

#[test]
fn closed_child_tree_snapshot_preserves_zero_source_suffix_accounting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-zero-child",
        "zero child",
        SpineTokenBaselines {
            provider_input_tokens: Some(5_000),
        },
    );
    append_msg(&mut runtime, &mut raw, "zero child work");
    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-zero-child",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(5_000),
        },
    );

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("closed child should reduce into ParseStack nodes")
    };
    let memory = nodes
        .iter()
        .find_map(|node| match node {
            SpineTreeNode::SpineTree { memory, .. } => Some(memory),
            _ => None,
        })
        .expect("closed child memory ref");
    assert_eq!(memory.open_context_tokens, Some(5_000));
    assert_eq!(memory.close_context_tokens, Some(5_000));
    assert_eq!(memory.closed_source_suffix_tokens, Some(0));

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    let nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(
        nodes["1.1.1"].accounting,
        Some(SpineTreeNodeAccountingSnapshot {
            current_node_context_tokens: None,
            current_node_context_problem: None,
            current_node_context_baseline_source: None,
            closed_source_suffix_tokens: Some(0),
            closed_memory_context_tokens: None,
            memory_output_tokens: Some(1_250),
        })
    );

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let replayed_snapshot = replayed.build_tree_snapshot().expect("replay snapshot");
    let replayed_nodes = snapshot_nodes_by_id(&replayed_snapshot);
    assert_eq!(
        replayed_nodes["1.1.1"].accounting,
        nodes["1.1.1"].accounting
    );
}

#[test]
fn close_prefers_structural_open_baseline_over_pressure_overlay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    open_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "open-structural-child",
        "structural child",
        SpineTokenBaselines {
            provider_input_tokens: Some(5_500),
        },
    );
    append_msg(&mut runtime, &mut raw, "child work");
    std::fs::write(
        runtime.store.pressure_path_for_test(),
        [
            format!(
                r#"{{"pressure_seq":0,"type":"open_context_baseline","node":[1,1,1],"observed_structural_seq":{},"observed_raw_ordinal":{},"observed_raw_live_hash":"{}","observed_context_index":{},"context_tokens":7000,"input_tokens":7500,"source":"estimated_from_live_suffix","estimated_live_suffix_tokens":500}}"#,
                runtime.store.next_event_seq().expect("next structural seq"),
                raw.len(),
                hash_raw_live(&vec![true; raw.len()]),
                raw.len()
            ),
            String::new(),
        ]
        .join("\n"),
    )
    .expect("write pressure overlay");
    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), Some(5_500));
    assert_eq!(
        replayed.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );

    close_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "close-structural-child",
        "1.1.1",
        SpineTokenBaselines {
            provider_input_tokens: Some(9_500),
        },
    );
    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    let nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(
        nodes["1.1.1"].accounting,
        Some(SpineTreeNodeAccountingSnapshot {
            current_node_context_tokens: None,
            current_node_context_problem: None,
            current_node_context_baseline_source: None,
            closed_source_suffix_tokens: Some(4_000),
            closed_memory_context_tokens: None,
            memory_output_tokens: Some(1_250),
        })
    );
}

#[test]
fn corrupt_legacy_pressure_records_do_not_fail_structural_replay() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    append_msg(&mut runtime, &mut raw, "live suffix");
    std::fs::write(
        runtime.store.pressure_path_for_test(),
        "not-json\n{\"pressure_seq\":77,\"type\":\"open_context_baseline\"",
    )
    .expect("corrupt pressure ledger");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine despite malformed pressure")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
}

// Root-depth lifecycle and spine.next transactions.

#[test]
fn root_depth_open_node_can_close_and_next_open_creates_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");

    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1"), "{tree}");
    assert!(tree.contains("[1] Current"), "{tree}");
    assert!(tree.contains("[1.1] Done"), "{tree}");
    assert!(!tree.contains("root"), "{tree}");

    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 3);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1")
                        && text.contains("real compact body for 1.1")
            )
    ));
    assert_eq!(materialized[1], spine_call(SPINE_TOOL_CLOSE, "close-1-1"));
    assert_eq!(materialized[2], function_output("close-1-1"));

    let snapshot = runtime.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    let nodes = snapshot_nodes_by_id(&snapshot);
    assert_eq!(snapshot.active_node_id, "1");
    assert_eq!(nodes["1"].status, SpineTreeNodeStatus::Live);
    assert_eq!(nodes["1.1"].parent_id.as_deref(), Some("1"));
    assert_eq!(nodes["1.1"].status, SpineTreeNodeStatus::Closed);

    open_task(&mut runtime, &mut raw, "open-1-2", "task 1.2");
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(open)),
            Symbol::SpineTreeNodes(open_nodes),
        ] if open.id == NodeId::root_epoch(1).child(2)
            && open.summary == "task 1.2"
            && matches!(
                open_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(3, 3), tool_resp(4, 4)]
            )
    ));
}

#[test]
fn spine_next_equivalent_to_close_then_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    open_task(&mut runtime, &mut raw, "open-child", "nested child");
    append_msg(&mut runtime, &mut raw, "nested child work");

    let commit = next_task(
        &mut runtime,
        &mut raw,
        "next-child",
        "1.1.1",
        "next sibling",
    );

    assert!(matches!(
        commit,
        SpineCommitKind::CloseThenOpen { open_index: 2 }
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root_child)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(next_sibling)),
            Symbol::SpineTreeNodes(next_nodes),
        ] if root_child.id == NodeId::root_epoch(1).child(1)
            && next_sibling.id == NodeId::root_epoch(1).child(1).child(2)
            && next_sibling.summary == "next sibling"
            && next_sibling.index == 2
            && next_sibling.open_context_tokens.is_none()
            && next_sibling.open_input_tokens.is_none()
            && matches!(
                next_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(4, 2), tool_resp(5, 3)]
            )
    ));

    let events = event_log(&runtime);
    assert_eq!(runtime.ledger.next_event_seq, 9);
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, SpineLedgerEvent::RootCompact { .. }))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SpineLedgerEvent::Close { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { child: initial, .. },
            SpineLedgerEvent::Msg { raw_ordinal: 0, .. },
            SpineLedgerEvent::Open { child: nested, .. },
            SpineLedgerEvent::ToolCall { .. },
            SpineLedgerEvent::Msg { raw_ordinal: 3, .. },
            SpineLedgerEvent::Close { node: closed, .. },
            SpineLedgerEvent::Open {
                child: next,
                index,
                summary,
                open_input_tokens: None,
                open_context_tokens: None,
                open_context_source: None,
                ..
            },
            SpineLedgerEvent::ToolCall { .. },
        ] if *initial == NodeId::root_epoch(1).child(1)
            && *nested == NodeId::root_epoch(1).child(1).child(1)
            && *closed == NodeId::root_epoch(1).child(1).child(1)
            && *next == NodeId::root_epoch(1).child(1).child(2)
            && *index == 2
            && summary == "next sibling"
    ));

    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 4);
    assert!(matches!(
        &materialized[1],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));
    assert_eq!(materialized[2], spine_call(SPINE_TOOL_NEXT, "next-child"));
    assert_eq!(materialized[3], function_output("next-child"));
}

#[test]
fn spine_next_defers_sibling_open_provider_baseline_until_post_replacement_usage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    open_task(&mut runtime, &mut raw, "open-child", "nested child");
    append_msg(&mut runtime, &mut raw, "nested child work");

    let token_baselines = SpineTokenBaselines {
        provider_input_tokens: Some(12_345),
    };
    let commit = next_task_with_token_baselines(
        &mut runtime,
        &mut raw,
        "next-child",
        "1.1.1",
        "next sibling",
        token_baselines,
    );

    assert!(matches!(
        commit,
        SpineCommitKind::CloseThenOpen { open_index: 2, .. }
    ));
    assert_eq!(runtime.current_open_input_tokens(), None);
    assert_eq!(runtime.current_open_provider_input_tokens(), None);
    assert_eq!(runtime.current_open_context_baseline_source(), None);
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(next_sibling)),
            Symbol::SpineTreeNodes(next_nodes),
        ] if next_sibling.id == NodeId::root_epoch(1).child(1).child(2)
            && next_sibling.summary == "next sibling"
            && next_sibling.index == 2
            && next_sibling.open_input_tokens.is_none()
            && next_sibling.open_context_tokens.is_none()
            && next_sibling.open_context_source.is_none()
            && matches!(
                next_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(4, 2), tool_resp(5, 3)]
            )
    ));

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::ToolCall { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Close { .. },
            SpineLedgerEvent::Open {
                child: next,
                index: 2,
                open_input_tokens: None,
                open_context_tokens: None,
                open_context_source: None,
                ..
            },
            SpineLedgerEvent::ToolCall { .. },
        ] if *next == NodeId::root_epoch(1).child(1).child(2)
    ));

    runtime
        .capture_current_open_provider_baseline(7_913)
        .expect("capture post-replacement provider baseline for next sibling");
    assert_eq!(runtime.current_open_input_tokens(), Some(7_913));
    assert_eq!(runtime.current_open_provider_input_tokens(), Some(7_913));
    assert_eq!(
        runtime.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
    assert!(matches!(
        event_log(&runtime).as_slice(),
        [
            ..,
            SpineLedgerEvent::OpenContextBaseline {
                node,
                open_input_tokens: 7_913,
                open_context_tokens: 7_913,
                open_context_source: ContextBaselineSource::ProviderAtOpen,
                ..
            },
        ] if *node == NodeId::root_epoch(1).child(1).child(2)
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_input_tokens(), Some(7_913));
    assert_eq!(replayed.current_open_provider_input_tokens(), Some(7_913));
    assert_eq!(
        replayed.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
}

#[test]
fn spine_next_close_failure_does_not_open_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    open_task(&mut runtime, &mut raw, "open-child", "nested child");
    append_msg(&mut runtime, &mut raw, "nested child work");

    let request = spine_call(SPINE_TOOL_NEXT, "bad-next");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record next request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe next request");
    runtime
        .stage_next(
            "bad-next".to_string(),
            "next sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");
    let output = function_output("bad-next");
    runtime.observe_raw_items(1).expect("record next output");
    raw.push(Some(output.clone()));
    runtime
        .observe_context_item(5, 5, &output)
        .expect("observe next output");

    let err = runtime
        .maybe_commit_output(
            "bad-next",
            Some(memory_assembly_with_context_range("1.1.1", 0..raw.len())),
        )
        .expect_err("bad compact range should fail next");
    assert!(
        err.to_string().contains("expected suffix start 1"),
        "unexpected next failure: {err}"
    );
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root_child)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(nested)),
            Symbol::SpineTreeNodes(_),
        ] if root_child.id == NodeId::root_epoch(1).child(1)
            && nested.id == NodeId::root_epoch(1).child(1).child(1)
    ));
    assert!(
        event_log(&runtime)
            .iter()
            .all(|event| !matches!(event, SpineLedgerEvent::Close { .. }))
    );
}

#[test]
fn checkpoint_after_root_depth_close_records_root_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let context = runtime.materialize_history(&raw).expect("materialize");

    runtime
        .checkpoint_before_user_msg(&rollout, runtime.raw_len, &raw)
        .expect("write root cursor checkpoint");
    let checkpoint = runtime
        .store
        .checkpoint_for_test(runtime.raw_len)
        .expect("read root cursor checkpoint");

    assert_eq!(checkpoint.cursor, "1");
    assert_eq!(
        checkpoint.h_ps_hash,
        hash_response_items(&context).expect("hash root cursor context")
    );
    assert!(matches!(
        checkpoint.parse_stack.symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::SpineTreeNodes(nodes),
        ] if nodes.len() == 2 && matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::SpineTree { meta, .. },
                SpineTreeNode::ToolCallAsLeafNode { segments },
            ]
                if meta.id == NodeId::root_epoch(1).child(1)
                    && segments == &vec![tool_req(1, 1), tool_resp(2, 2)]
        )
    ));
}

#[test]
fn close_at_root_cursor_fails_without_mutating_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let before = runtime.parse_stack().clone();
    let (_, request_raw, request_context) =
        observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-root");
    let err = runtime
        .stage_close("close-root".to_string(), "test node memory".to_string())
        .expect_err("root cursor close should fail at stage time");
    assert!(
        err.to_string().contains("cannot close root epoch cursor 1"),
        "unexpected root close error: {err}"
    );
    assert!(
        runtime
            .pending_commit("close-root")
            .expect("pending lookup after rejected close")
            .is_none(),
        "rejected root close must not install pending close intent"
    );
    assert_eq!(runtime.parse_stack(), &before);
    let (_, response_raw, response_context) =
        observe_function_output(&mut runtime, &mut raw, "close-root");
    let aborted_pending = runtime
        .commit_completed_toolcall_as_ordinary_with_raw_items(
            "close-root",
            completed_toolcall(
                "close-root",
                vec![
                    tool_req(request_raw, request_context),
                    tool_resp(response_raw, response_context),
                ],
            ),
            &raw,
        )
        .expect("commit rejected close transaction as ordinary toolcall");
    assert!(
        !aborted_pending,
        "invalid close must not consume a pending close symbol"
    );
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::SpineTreeNodes(nodes),
        ] if matches!(
            nodes.as_slice(),
            [
                SpineTreeNode::SpineTree { meta, .. },
                SpineTreeNode::ToolCallAsLeafNode { segments: close_segments },
                SpineTreeNode::ToolCallAsLeafNode { segments: rejected_segments },
            ] if meta.id == NodeId::root_epoch(1).child(1)
                && close_segments == &vec![tool_req(1, 1), tool_resp(2, 2)]
                && rejected_segments == &vec![
                    tool_req(request_raw, request_context),
                    tool_resp(response_raw, response_context),
                ]
        )
    ));
}

#[test]
fn next_at_root_cursor_fails_without_pending_transition() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let before = runtime.parse_stack().clone();
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_NEXT, "next-root");
    let err = runtime
        .stage_next(
            "next-root".to_string(),
            "must not open sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect_err("root cursor next should fail at stage time");
    assert!(
        err.to_string().contains("cannot close root epoch cursor 1"),
        "unexpected root next error: {err}"
    );
    assert!(
        runtime
            .pending_commit("next-root")
            .expect("pending lookup after rejected next")
            .is_none(),
        "rejected root next must not install pending close/open intent"
    );
    assert_eq!(runtime.parse_stack(), &before);
}

// Control carriers, parser shifts, and pending commits.

#[test]
fn ordinary_response_item_shifts_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = text_item("ordinary");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { raw_start: 0 },
            SpineLedgerEvent::Open { summary, .. },
            SpineLedgerEvent::Msg {
                raw_ordinal: 0,
                context_index: 0,
                from_user: true,
                user_anchor: Some(1),
            }
        ] if summary == "root"
    ));
    assert_eq!(
        runtime.parse_stack().symbols,
        vec![
            Symbol::Control(ControlSymbol::Init(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1),
                    0,
                    "root".to_string()
                )
                .expect("root meta")
            )),
            Symbol::Control(ControlSymbol::Open(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1).child(1),
                    0,
                    "root".to_string()
                )
                .expect("root open meta")
            )),
            Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                from_user: true,
                user_anchor: Some(1),
            }]),
        ]
    );
    let raw = vec![Some(item)];
    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }] if text == "[U1]\nordinary"
            )
    ));
}

#[test]
fn multimodal_user_message_receives_anchor_without_dropping_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = multimodal_user_item();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let raw = vec![Some(item)];
    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [
                    ContentItem::InputText { text },
                    ContentItem::InputImage { image_url, detail: Some(ImageDetail::High) },
                    ContentItem::InputText { text: second },
                ] if text == "[U1]\nfirst text"
                    && image_url == "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR"
                    && second == "second text"
            )
    ));
}

#[test]
fn image_only_user_message_receives_synthetic_anchor_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image_url: "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR".to_string(),
            detail: Some(ImageDetail::Low),
        }],
        phase: None,
    };
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let raw = vec![Some(item)];
    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [
                    ContentItem::InputText { text },
                    ContentItem::InputImage { image_url, detail: Some(ImageDetail::Low) },
                ] if text == "[U1]\n<image omitted detail=low>"
                    && image_url == "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR"
            )
    ));
}

#[test]
fn non_user_message_does_not_receive_user_anchor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = assistant_text_item("assistant note");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg {
                raw_ordinal: 0,
                context_index: 0,
                from_user: false,
                user_anchor: None,
            }
        ]
    ));
    let raw = vec![Some(item)];
    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::OutputText { text }] if text == "assistant note"
            )
    ));
}

#[test]
fn close_memory_rejects_unknown_user_anchor_reference() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "known user");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    let err = runtime
        .stage_close(
            "close".to_string(),
            "This memory cites [U99], which does not exist.".to_string(),
        )
        .expect_err("unknown user anchor must fail");
    assert!(
        err.to_string().contains("unknown user anchor [U99]"),
        "{err}"
    );
    runtime
        .stage_close(
            "close".to_string(),
            "This memory cites the existing [U1].".to_string(),
        )
        .expect("known user anchor should be accepted");
}

#[test]
fn ordinary_tool_items_shift_as_toolcall_token_and_render_full_transaction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ResponseItem::FunctionCall {
        id: None,
        name: "shell_command".to_string(),
        namespace: None,
        arguments: "{\"command\":\"pwd\"}".to_string(),
        call_id: "ordinary-tool".to_string(),
    };
    let output_1 = function_output("ordinary-tool");
    let output_2 = function_output("ordinary-tool");
    let raw = vec![
        Some(request.clone()),
        Some(output_1.clone()),
        Some(output_2.clone()),
    ];
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut parse_stack = runtime.parse_stack().clone();

    parse_stack
        .shift(
            SpineToken::ToolCall {
                segments: vec![tool_req(0, 0), tool_resp(1, 1), tool_resp(2, 2)],
            },
            &runtime.archive(),
        )
        .expect("shift completed toolcall");

    assert_eq!(
        parse_stack.symbols[2],
        Symbol::SpineTreeNodes(vec![SpineTreeNode::ToolCallAsLeafNode {
            segments: vec![tool_req(0, 0), tool_resp(1, 1), tool_resp(2, 2)],
        }])
    );
    assert_eq!(
        render_parse_stack_to_context(&parse_stack, &raw).expect("render ordinary toolcall"),
        vec![request, output_1, output_2]
    );
}

#[test]
fn ordinary_tool_transaction_observes_toolcall_leaf_and_replays() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ResponseItem::FunctionCall {
        id: None,
        name: "shell_command".to_string(),
        namespace: None,
        arguments: "{\"command\":\"pwd\"}".to_string(),
        call_id: "ordinary-tool".to_string(),
    };
    let output = function_output("ordinary-tool");
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record request raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("request alone is not a completed toolcall"),
        Vec::<ResponseItem>::new()
    );
    assert_eq!(runtime.parse_stack_msg_leaf_count_for_test(), 0);
    assert_eq!(runtime.parse_stack_toolcall_leaf_count_for_test(), 0);
    assert!(!matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::Msg { .. })
    ));

    runtime.observe_raw_items(1).expect("record output raw");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output raw/context");
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("response alone still waits for completed toolcall hook"),
        Vec::<ResponseItem>::new()
    );
    runtime
        .observe_completed_toolcall(completed_toolcall(
            "ordinary-tool",
            vec![tool_req(0, 0), tool_resp(1, 1)],
        ))
        .expect("observe completed toolcall");

    let rendered = runtime
        .materialize_history(&raw)
        .expect("toolcall renders full transaction");
    assert_eq!(rendered, vec![request.clone(), output.clone()]);
    assert_eq!(runtime.parse_stack_msg_leaf_count_for_test(), 0);
    assert_eq!(runtime.parse_stack_toolcall_leaf_count_for_test(), 1);
    assert!(matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::ToolCall { segments })
            if segments == &vec![event_tool_req(0, 0), event_tool_resp(1, 1)]
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    assert_eq!(
        replayed
            .materialize_history(&raw)
            .expect("replayed toolcall renders"),
        vec![request, output]
    );
    assert_eq!(replayed.parse_stack_msg_leaf_count_for_test(), 0);
    assert_eq!(replayed.parse_stack_toolcall_leaf_count_for_test(), 1);
}

#[test]
fn completed_toolcall_tags_long_text_tool_response_for_next_turn_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "x".repeat(600);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "rendered long output should expose trim id: {:?}",
        rendered[1]
    );
    assert!(
        function_output_text_content(&rendered[1]).contains(&long_text),
        "tagging must keep original visible output until trim"
    );
    assert_eq!(function_output_text_content(&output), long_text);
    let trim_events = runtime.store.trim_events().expect("trim events");
    assert!(matches!(
        trim_events.as_slice(),
        [LoggedTrimEvent {
            trim_seq: 0,
            event: TrimEvent::Candidate {
                trim_id,
                toolcall_seq: 2,
                raw_ordinal: 1,
                context_index: 1,
                call_id,
                response_kind: TrimResponseKind::FunctionCallOutput,
                ..
            }
        }] if trim_id == "trim_0" && call_id == "long-tool"
    ));
}

#[test]
fn trim_only_runtime_tags_and_trims_without_tree_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let output = function_output_text("long-tool", &"trim-only output ".repeat(50));
    let raw = vec![Some(output.clone())];
    let mut runtime =
        SpineRuntime::load_or_create_with_jit(&rollout, 0, false).expect("create trim runtime");

    assert!(
        !runtime.store.tree_path_for_test().exists(),
        "trim-only must not create the JIT parser tree ledger"
    );
    runtime.observe_raw_items(1).expect("record raw");
    runtime
        .observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id: "long-tool".to_string(),
                request_call_ids: vec!["long-tool".to_string()],
                segments: vec![CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 0,
                    context_index: 0,
                }],
            },
            &raw,
        )
        .expect("observe trim-only completed toolcall");

    let projected = runtime
        .project_raw_history_with_trim(&[output.clone()])
        .expect("project trim-only history");
    assert!(
        function_output_text_content(&projected[0]).starts_with("[TRIM_ID: trim_1]\n"),
        "trim-only projection should expose the generated trim id"
    );
    assert_eq!(
        runtime.trim_tool_response("trim_1").expect("trim succeeds"),
        SpineTrimOutcome::Cleared {
            trim_id: "trim_1".to_string()
        }
    );
    let cleared = runtime
        .project_raw_history_with_trim(&[output])
        .expect("project cleared trim-only history");
    assert_eq!(
        function_output_text_content(&cleared[0]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        !runtime.store.tree_path_for_test().exists(),
        "trim-only trim must still not create the JIT parser tree ledger"
    );
}

#[test]
fn trim_only_fork_clone_copies_trim_ledger_without_tree_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let output = function_output_text("long-tool", &"trim-only fork output ".repeat(50));
    let raw = vec![Some(output.clone())];
    let mut source = SpineRuntime::load_or_create_with_jit(&source_rollout, 0, false)
        .expect("create trim runtime");

    source.observe_raw_items(1).expect("record raw");
    source
        .observe_completed_toolcall_with_raw_items(
            CompletedToolCall {
                call_id: "long-tool".to_string(),
                request_call_ids: vec!["long-tool".to_string()],
                segments: vec![CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 0,
                    context_index: 0,
                }],
            },
            &raw,
        )
        .expect("observe trim-only completed toolcall");
    source.trim_tool_response("trim_1").expect("clear trim id");
    assert!(
        !source.store.tree_path_for_test().exists(),
        "trim-only source must not create the JIT parser tree ledger"
    );

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[true]);
    let target =
        SpineRuntime::load_or_create_with_jit(&target_rollout, 1, false).expect("load target");
    let projected = target
        .project_raw_history_with_trim(&[output])
        .expect("project target");
    assert_eq!(
        function_output_text_content(&projected[0]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        !target.store.tree_path_for_test().exists(),
        "trim-only clone must not create the JIT parser tree ledger"
    );
}

#[test]
fn completed_toolcall_tags_long_custom_tool_response_for_next_turn_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("custom_tool", "custom-long-tool");
    let long_text = "custom output ".repeat(60);
    let output = custom_tool_output_text("custom-long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("custom-long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert!(
        custom_tool_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "rendered custom output should expose trim id: {:?}",
        rendered[1]
    );
    assert!(
        custom_tool_output_text_content(&rendered[1]).contains(&long_text),
        "tagging must keep original custom output visible until trim"
    );
    assert_eq!(custom_tool_output_text_content(&output), long_text);
    let trim_events = runtime.store.trim_events().expect("trim events");
    assert!(matches!(
        trim_events.as_slice(),
        [LoggedTrimEvent {
            trim_seq: 0,
            event: TrimEvent::Candidate {
                trim_id,
                toolcall_seq: 2,
                raw_ordinal: 1,
                context_index: 1,
                call_id,
                response_kind: TrimResponseKind::CustomToolCallOutput,
                ..
            }
        }] if trim_id == "trim_0" && call_id == "custom-long-tool"
    ));
}

#[test]
fn completed_toolcall_does_not_tag_content_items_tool_response_for_trim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "content-items-tool");
    let output = function_output_content_items("content-items-tool", &"content item ".repeat(80));
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("content-items-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime.materialize_history(&raw).expect("materialize"),
        vec![request, output]
    );
    assert!(runtime.store.trim_events().expect("trim events").is_empty());
}

#[test]
fn completed_toolcall_does_not_tag_short_tool_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "short-tool");
    let output = function_output("short-tool");
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("short-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime.materialize_history(&raw).expect("materialize"),
        vec![request, output]
    );
    assert!(runtime.store.trim_events().expect("trim events").is_empty());
}

#[test]
fn trim_tool_response_clears_visible_projection_and_preserves_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime.trim_tool_response("trim_0").expect("trim succeeds"),
        SpineTrimOutcome::Cleared {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(rendered[0], request);
    assert_eq!(
        function_output_text_content(&rendered[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert_eq!(function_output_text_content(&output), long_text);
    assert_eq!(
        runtime
            .trim_tool_response("trim_0")
            .expect("repeat trim is idempotent"),
        SpineTrimOutcome::AlreadyCleared {
            trim_id: "trim_0".to_string()
        }
    );

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_history(&raw)
        .expect("replayed trim projection");
    assert_eq!(
        function_output_text_content(&replayed_rendered[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
}

#[test]
fn trim_slice_head_rewrites_visible_projection_and_preserves_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "abcdefg ".repeat(80);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert_eq!(
        runtime
            .slice_tool_response_head("trim_0", 7, &raw)
            .expect("slice succeeds"),
        SpineTrimOutcome::Sliced {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abcdefg");
    assert_eq!(function_output_text_content(&output), long_text);
    assert!(matches!(
        runtime.store.trim_events().expect("persisted trim events").as_slice(),
        [
            LoggedTrimEvent {
                event: TrimEvent::Candidate { trim_id, .. },
                ..
            },
            LoggedTrimEvent {
                event: TrimEvent::Sliced { trim_id: sliced_id, .. },
                ..
            }
        ] if trim_id == "trim_0" && sliced_id == "trim_0"
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_history(&raw)
        .expect("materialize replay");
    assert_eq!(
        function_output_text_content(&replayed_rendered[1]),
        "abcdefg"
    );
}

#[test]
fn trim_slice_tail_rewrites_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!("{}TAIL-END", "prefix ".repeat(90));
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_tail("trim_0", 8, &raw)
        .expect("slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "TAIL-END");
}

#[test]
fn trim_slice_anchor_window_rewrites_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!(
        "{}abc<needle>xyz{}",
        "left ".repeat(60),
        " right".repeat(60)
    );
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_anchor("trim_0", "<needle>", 3, 3, &raw)
        .expect("slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abc<needle>xyz");
}

#[test]
fn trim_slice_rejects_missing_anchor_without_projection_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    assert!(matches!(
        runtime.slice_tool_response_anchor("trim_0", "missing", 1, 1, &raw),
        Ok(SpineTrimOutcome::Miss { trim_id }) if trim_id == "trim_0"
    ));
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "missing anchor must not change visible projection"
    );
}

#[test]
fn trim_repeated_slice_applies_to_current_visible_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = format!("{}abcdef{}", "prefix ".repeat(60), " suffix".repeat(60));
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_anchor("trim_0", "abcdef", 0, 0, &raw)
        .expect("first slice succeeds");
    runtime
        .slice_tool_response_head("trim_0", 3, &raw)
        .expect("second slice succeeds");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(function_output_text_content(&rendered[1]), "abc");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load replayed runtime")
        .expect("runtime exists");
    let replayed_rendered = replayed
        .materialize_history(&raw)
        .expect("materialize replay");
    assert_eq!(function_output_text_content(&replayed_rendered[1]), "abc");
}

#[test]
fn trim_snip_after_slice_clears_visible_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");

    runtime
        .slice_tool_response_head("trim_0", 9, &raw)
        .expect("slice succeeds");
    assert_eq!(
        runtime.trim_tool_response("trim_0").expect("snip succeeds"),
        SpineTrimOutcome::Cleared {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(
        function_output_text_content(&rendered[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
}

#[test]
fn trim_tool_response_only_matches_latest_completed_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request_1 = ordinary_call("shell_command", "long-tool");
    let output_1 = function_output_text("long-tool", &"old output ".repeat(80));
    let request_2 = ordinary_call("shell_command", "short-tool");
    let output_2 = function_output("short-tool");
    let raw = vec![
        Some(request_1.clone()),
        Some(output_1.clone()),
        Some(request_2.clone()),
        Some(output_2.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record first raw");
    runtime
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    runtime
        .observe_context_item(1, 1, &output_1)
        .expect("observe first output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe first completed toolcall");
    runtime.observe_raw_items(2).expect("record second raw");
    runtime
        .observe_context_item(2, 2, &request_2)
        .expect("observe second request");
    runtime
        .observe_context_item(3, 3, &output_2)
        .expect("observe second output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("short-tool", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe second completed toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("trim_0")
            .expect("old trim id misses after newer completed toolcall"),
        SpineTrimOutcome::Miss {
            trim_id: "trim_0".to_string()
        }
    );
    assert!(matches!(
        runtime.store.trim_events().expect("trim events").as_slice(),
        [LoggedTrimEvent {
            event: TrimEvent::Candidate { trim_id, .. },
            ..
        }] if trim_id == "trim_0"
    ));
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "miss must not clear the old output"
    );
}

#[test]
fn trim_tool_response_does_not_retry_old_id_after_missed_attempt_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let trim_request = spine_call(SPINE_TOOL_TRIM, "trim-miss");
    let trim_output = function_output_text("trim-miss", "Do not retry this trim id.");
    let raw = vec![
        Some(request.clone()),
        Some(output.clone()),
        Some(trim_request.clone()),
        Some(trim_output.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record target raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe target request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe target output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe target completed toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("unknown_trim")
            .expect("unknown trim id misses"),
        SpineTrimOutcome::Miss {
            trim_id: "unknown_trim".to_string()
        }
    );

    runtime
        .observe_raw_items(2)
        .expect("record committed trim attempt raw");
    runtime
        .observe_context_item(2, 2, &trim_request)
        .expect("observe trim attempt request");
    runtime
        .observe_context_item(3, 3, &trim_output)
        .expect("observe trim attempt output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("trim-miss", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe committed trim attempt as latest toolcall");

    assert_eq!(
        runtime
            .trim_tool_response("trim_0")
            .expect("old target trim id is no longer in previous completed toolcall"),
        SpineTrimOutcome::Miss {
            trim_id: "trim_0".to_string()
        }
    );
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "missed attempt commit must not make the older target retryable"
    );
    assert!(
        function_output_text_content(&rendered[1]).contains(&long_text),
        "missed attempt commit must leave the older target body intact under the tag"
    );
}

#[test]
fn feedback_markdown_append_creates_file_and_preserves_existing_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine runtime");
    let store = &runtime.store;

    store
        .append_feedback_markdown("## first\n\nInitial feedback")
        .expect("append first feedback");
    store
        .append_feedback_markdown("## second\n\nFollow-up feedback")
        .expect("append second feedback");

    let body =
        std::fs::read_to_string(store.feedback_path_for_test()).expect("read feedback markdown");
    assert_eq!(
        body,
        "## first\n\nInitial feedback\n\n## second\n\nFollow-up feedback\n"
    );
}

#[test]
fn missing_trim_ledger_fails_closed_instead_of_restoring_raw_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let raw = vec![Some(request), Some(output)];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, raw[0].as_ref().expect("request"))
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, raw[1].as_ref().expect("output"))
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");
    let rendered = runtime.materialize_history(&raw).expect("materialize");
    assert!(
        function_output_text_content(&rendered[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "corruption fixture should have trim projection before the ledger is moved aside"
    );

    let parked_trim_ledger = dir.path().join("parked-trim.jsonl");
    std::fs::rename(runtime.store.trim_path_for_test(), &parked_trim_ledger)
        .expect("park trim ledger to simulate corruption");
    let err = match SpineRuntime::load_for_rollout_items(&rollout, &raw, &[]) {
        Err(err) => err,
        Ok(_) => panic!("missing trim ledger must fail closed"),
    };
    assert!(
        err.to_string()
            .contains("missing required Spine trim ledger"),
        "unexpected missing trim ledger error: {err}"
    );
}

#[test]
fn rollback_before_trim_clear_restores_tagged_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let long_text = "important raw output ".repeat(40);
    let output = function_output_text("long-tool", &long_text);
    let mut raw = vec![Some(request.clone()), Some(output.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe completed toolcall");
    runtime
        .checkpoint_before_user_msg(&rollout, 2, &raw)
        .expect("checkpoint before trim clear");
    runtime
        .trim_tool_response("trim_0")
        .expect("clear trim target");
    raw.push(None);
    runtime
        .observe_raw_items(1)
        .expect("record rolled-back raw");
    runtime
        .observe_context_item(2, 2, &text_item("rolled back"))
        .expect("observe rolled-back msg");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[2])
        .expect("load rollback")
        .expect("sidecar exists");
    let materialized = replayed.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized[0], request);
    let rolled_back_output = function_output_text_content(&materialized[1]);
    assert!(
        rolled_back_output.starts_with("[TRIM_ID: trim_0]\n"),
        "rollback before clear must keep the candidate tag visible, got: {rolled_back_output:?}"
    );
    assert!(
        rolled_back_output.contains(&long_text),
        "rollback before clear must restore the original visible body under the tag"
    );
}

#[test]
fn rollback_before_trim_candidate_removes_trim_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ordinary_call("shell_command", "long-tool");
    let output = function_output_text("long-tool", &"important raw output ".repeat(40));
    let raw_after_rollback = vec![None, None];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .checkpoint_before_user_msg(&rollout, 0, &[])
        .expect("checkpoint before candidate");
    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe output");
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &[Some(request), Some(output)],
        )
        .expect("observe completed toolcall");

    let mut replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[0])
        .expect("load rollback")
        .expect("sidecar exists");
    assert!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize")
            .is_empty()
    );
    assert_eq!(
        replayed
            .trim_tool_response("trim_0")
            .expect("trim id should be outside rollback-visible state"),
        SpineTrimOutcome::Miss {
            trim_id: "trim_0".to_string()
        }
    );
}

#[test]
fn fork_after_trim_clear_preserves_projection_and_allocates_non_colliding_trim_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let request_1 = ordinary_call("shell_command", "first-long-tool");
    let output_1 = function_output_text("first-long-tool", &"first raw output ".repeat(50));
    let request_2 = ordinary_call("shell_command", "second-long-tool");
    let output_2 = function_output_text("second-long-tool", &"second raw output ".repeat(50));
    let raw = vec![
        Some(request_1.clone()),
        Some(output_1.clone()),
        Some(request_2.clone()),
        Some(output_2.clone()),
    ];
    let mut source = SpineRuntime::load_or_create(&source_rollout, 0).expect("create source");

    source.observe_raw_items(2).expect("record source raw");
    source
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    source
        .observe_context_item(1, 1, &output_1)
        .expect("observe first output");
    source
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("first-long-tool", vec![tool_req(0, 0), tool_resp(1, 1)]),
            &raw,
        )
        .expect("observe first completed toolcall");
    source
        .trim_tool_response("trim_0")
        .expect("clear first long output");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[true, true]);
    let target = SpineRuntime::load_for_rollout_items(&target_rollout, &raw[..2], &[])
        .expect("load target")
        .expect("target sidecar exists");
    let target_visible = target
        .materialize_history(&raw[..2])
        .expect("materialize target");
    assert_eq!(
        function_output_text_content(&target_visible[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    drop(target);

    let mut forked = SpineRuntime::load_or_create(&target_rollout, 2).expect("load fork writer");
    forked.observe_raw_items(2).expect("record second raw");
    forked
        .observe_context_item(2, 2, &request_2)
        .expect("observe second request");
    forked
        .observe_context_item(3, 3, &output_2)
        .expect("observe second output");
    forked
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall("second-long-tool", vec![tool_req(2, 2), tool_resp(3, 3)]),
            &raw,
        )
        .expect("observe second completed toolcall");

    let fork_visible = forked.materialize_history(&raw).expect("materialize fork");
    assert_eq!(
        function_output_text_content(&fork_visible[1]),
        crate::spine::model::TOOL_RESULT_CLEARED_MESSAGE
    );
    assert!(
        function_output_text_content(&fork_visible[3]).starts_with("[TRIM_ID: trim_2]\n"),
        "fork must continue after copied candidate+clear seqs without reusing trim_0"
    );
}

#[test]
fn completed_toolcall_groups_request_and_all_outputs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request = ResponseItem::FunctionCall {
        id: None,
        name: "shell_command".to_string(),
        namespace: None,
        arguments: "{\"command\":\"pwd\"}".to_string(),
        call_id: "ordinary-tool".to_string(),
    };
    let output_1 = function_output("ordinary-tool");
    let output_2 = function_output("ordinary-tool");
    let raw = vec![
        Some(request.clone()),
        Some(output_1.clone()),
        Some(output_2.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(3).expect("record tool raw");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe request");
    runtime
        .observe_context_item(1, 1, &output_1)
        .expect("observe first output");
    runtime
        .observe_context_item(2, 2, &output_2)
        .expect("observe second output");
    runtime
        .observe_completed_toolcall(completed_toolcall(
            "ordinary-tool",
            vec![tool_req(0, 0), tool_resp(1, 1), tool_resp(2, 2)],
        ))
        .expect("observe completed multi-response toolcall");

    assert_eq!(
        runtime.parse_stack().symbols[2],
        Symbol::SpineTreeNodes(vec![SpineTreeNode::ToolCallAsLeafNode {
            segments: vec![tool_req(0, 0), tool_resp(1, 1), tool_resp(2, 2)],
        }])
    );
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("render multi-response toolcall"),
        vec![request, output_1, output_2]
    );
}

#[test]
fn completed_toolcall_preserves_multiple_requests_and_clears_all_request_anchors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request_1 = ordinary_call("shell_command", "tool-1");
    let request_2 = ordinary_call("tool_search", "tool-2");
    let output_1 = function_output("tool-1");
    let output_2 = function_output("tool-2");
    let raw = vec![
        Some(request_1.clone()),
        Some(request_2.clone()),
        Some(output_1.clone()),
        Some(output_2.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(4).expect("record grouped raw");
    runtime
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    runtime
        .observe_context_item(1, 1, &request_2)
        .expect("observe second request");
    runtime
        .observe_context_item(2, 2, &output_1)
        .expect("observe first response");
    runtime
        .observe_context_item(3, 3, &output_2)
        .expect("observe second response");
    runtime
        .observe_completed_toolcall(CompletedToolCall {
            call_id: "tool-1".to_string(),
            request_call_ids: vec!["tool-1".to_string(), "tool-2".to_string()],
            segments: vec![
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 0,
                    context_index: 0,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 1,
                    context_index: 1,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 2,
                    context_index: 2,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 3,
                    context_index: 3,
                },
            ],
        })
        .expect("observe grouped completed toolcall");

    assert!(matches!(
        event_log(&runtime).last(),
        Some(SpineLedgerEvent::ToolCall { segments })
            if segments == &vec![
                event_tool_req(0, 0),
                event_tool_req(1, 1),
                event_tool_resp(2, 2),
                event_tool_resp(3, 3),
            ]
    ));
    assert_eq!(
        runtime.parse_stack().symbols[2],
        Symbol::SpineTreeNodes(vec![SpineTreeNode::ToolCallAsLeafNode {
            segments: vec![
                tool_req(0, 0),
                tool_req(1, 1),
                tool_resp(2, 2),
                tool_resp(3, 3),
            ],
        }])
    );
    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("render grouped toolcall"),
        vec![request_1, request_2.clone(), output_1, output_2]
    );

    runtime.observe_raw_items(1).expect("record reused request");
    runtime
        .observe_context_item(4, 4, &request_2)
        .expect("completed grouped toolcall clears every request anchor");
}

#[test]
fn completed_toolcall_rejects_request_after_response() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let request_1 = ordinary_call("shell_command", "tool-1");
    let output_1 = function_output("tool-1");
    let request_2 = ordinary_call("tool_search", "tool-2");
    let output_2 = function_output("tool-2");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .observe_raw_items(4)
        .expect("record interleaved raw");
    runtime
        .observe_context_item(0, 0, &request_1)
        .expect("observe first request");
    runtime
        .observe_context_item(1, 1, &output_1)
        .expect("observe first response");
    runtime
        .observe_context_item(2, 2, &request_2)
        .expect("observe second request");
    runtime
        .observe_context_item(3, 3, &output_2)
        .expect("observe second response");

    let err = runtime
        .observe_completed_toolcall(CompletedToolCall {
            call_id: "tool-1".to_string(),
            request_call_ids: vec!["tool-1".to_string(), "tool-2".to_string()],
            segments: vec![
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 0,
                    context_index: 0,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 1,
                    context_index: 1,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 2,
                    context_index: 2,
                },
                CompletedToolCallSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 3,
                    context_index: 3,
                },
            ],
        })
        .expect_err("toolcall must have all requests before responses");
    assert!(
        err.to_string().contains("appears after a response segment"),
        "unexpected completed toolcall error: {err}"
    );
}

#[test]
fn spine_tree_toolcall_is_plain_toolcall_leaf_for_replay_coverage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let tree_request = spine_call(SPINE_TOOL_TREE, "tree-call");
    let tree_output = function_output("tree-call");
    let final_message = text_item("tree done");
    let raw = vec![
        Some(tree_request.clone()),
        Some(tree_output.clone()),
        Some(final_message.clone()),
    ];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .observe_raw_items(1)
        .expect("record tree request raw");
    runtime
        .observe_context_item(0, 0, &tree_request)
        .expect("observe tree request");
    runtime
        .observe_raw_items(1)
        .expect("record tree output raw");
    runtime
        .observe_context_item(1, 1, &tree_output)
        .expect("observe tree output");
    runtime
        .observe_completed_toolcall(completed_toolcall(
            "tree-call",
            vec![tool_req(0, 0), tool_resp(1, 1)],
        ))
        .expect("observe completed spine.tree toolcall");
    runtime
        .observe_raw_items(1)
        .expect("record final message raw");
    runtime
        .observe_context_item(2, 2, &final_message)
        .expect("observe final message");

    assert_eq!(
        runtime
            .materialize_history(&raw)
            .expect("tree request/output stay ordinary toolcall"),
        vec![
            tree_request.clone(),
            tree_output.clone(),
            anchored_text_item(1, "tree done")
        ]
    );
    let replayed = SpineRuntime::load_for_rollout(&rollout, 3)
        .expect("tree toolcall should replay without missing coverage")
        .expect("sidecar exists");
    assert_eq!(
        replayed
            .materialize_history(&raw)
            .expect("replayed tree request/output stay ordinary toolcall"),
        vec![
            tree_request,
            tree_output,
            anchored_text_item(1, "tree done")
        ]
    );
}

#[test]
fn end_token_is_retained_as_control_epsilon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = text_item("ordinary");
    let raw = vec![Some(item.clone())];
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let mut parse_stack = runtime.parse_stack().clone();
    parse_stack
        .shift(SpineToken::End, &runtime.archive())
        .expect("shift End");

    assert!(matches!(
        parse_stack.symbols.last(),
        Some(Symbol::Control(ControlSymbol::End))
    ));
    assert_eq!(
        render_parse_stack_to_context(&parse_stack, &raw).expect("render context"),
        vec![anchored_text_item(1, "ordinary")]
    );
    let tree = parse_stack.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Current"), "{tree}");
}

#[test]
fn materialize_history_requires_visible_msg_raw_item() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = text_item("ordinary");
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let err = runtime
        .materialize_history(&[None])
        .expect_err("h(PS) must render visible Msg from ParseStack, not raw gaps");
    assert!(
        err.to_string()
            .contains("missing raw item for visible Msg raw ordinal 0"),
        "unexpected materialization error: {err}"
    );
}

#[test]
fn spine_open_lexer_emits_open_then_toolcall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let request = spine_call(SPINE_TOOL_OPEN, "open");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    let output = function_output("open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &output)
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    let events = event_log(&runtime);
    assert_eq!(events.len(), 4);
    assert!(matches!(events[0], SpineLedgerEvent::Init { raw_start: 0 }));
    assert!(matches!(
        &events[1],
        SpineLedgerEvent::Open {
            boundary: 0,
            summary,
            ..
        } if summary == "root"
    ));
    assert!(matches!(
        &events[2],
        SpineLedgerEvent::Open {
            boundary: 0,
            summary,
            ..
        } if summary == "child"
    ));
    assert!(matches!(
        &events[3],
        SpineLedgerEvent::ToolCall { segments }
            if segments == &vec![event_tool_req(0, 0), event_tool_resp(1, 1)]
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(meta)),
            Symbol::Control(ControlSymbol::Open(child)),
            Symbol::SpineTreeNodes(nodes),
        ] if meta.summary == "root"
            && meta.id == NodeId::root_epoch(1).child(1)
            && child.summary == "child"
            && child.id == NodeId::root_epoch(1).child(1).child(1)
            && child.index == 0
            && matches!(
                nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
            )
    ));
    assert_eq!(
        runtime
            .materialize_history(&[Some(request.clone()), Some(output.clone())])
            .expect("materialize history"),
        vec![request, output]
    );
}

#[test]
fn duplicate_open_call_id_does_not_create_second_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    let request = spine_call(SPINE_TOOL_OPEN, "dup-open");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe first open request");

    runtime
        .observe_raw_items(1)
        .expect("record duplicate request");
    let err = runtime
        .observe_context_item(1, 1, &request)
        .expect_err("duplicate open request anchor must fail fast");
    assert!(
        err.to_string()
            .contains("duplicate spine.open request anchor for dup-open"),
        "unexpected duplicate error: {err}"
    );

    runtime
        .stage_open("dup-open".to_string(), "only child".to_string())
        .expect("stage open");
    let output = function_output("dup-open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &output)
        .expect("observe open output");
    runtime
        .maybe_commit_output("dup-open", None)
        .expect("commit open");
    let events_after_first_commit = event_log(&runtime);
    let event_debug_after_first_commit = event_log_debug(&runtime);
    assert_eq!(
        events_after_first_commit
            .iter()
            .filter(
                |event| matches!(event, SpineLedgerEvent::Open { summary, .. } if summary == "only child")
            )
            .count(),
        1
    );
    assert_eq!(
        runtime
            .maybe_commit_output("dup-open", None)
            .expect("duplicate output commit should be no-op"),
        None
    );
    assert_eq!(event_log_debug(&runtime), event_debug_after_first_commit);
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("Spine Task Tree:"), "{tree}");
    assert!(tree.contains("- [1.1] Open"), "{tree}");
    assert!(tree.contains("- [1.1.1] Current only child"), "{tree}");
}

#[test]
fn ledger_cache_uses_sparse_max_seq_on_load_and_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_logged_event(&LoggedSpineLedgerEvent {
            seq: 0,
            event: SpineLedgerEvent::Init { raw_start: 0 },
        })
        .expect("append sparse init");
    store
        .append_logged_event(&LoggedSpineLedgerEvent {
            seq: 7,
            event: SpineLedgerEvent::Open {
                child: NodeId::root_epoch(1).child(1),
                boundary: 0,
                index: 0,
                summary: "root".to_string(),
                open_input_tokens: None,
                open_context_tokens: None,
                open_context_source: None,
            },
        })
        .expect("append sparse root open");

    let mut runtime = SpineRuntime::load_for_rollout(&rollout, 0)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(runtime.ledger.next_event_seq, 8);
    assert_eq!(
        runtime
            .build_tree_snapshot()
            .expect("snapshot")
            .snapshot_seq,
        8
    );

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("after sparse ledger"))
        .expect("append msg");

    assert_eq!(runtime.ledger.next_event_seq, 9);
    let events = logged_events(&runtime);
    assert!(matches!(
        events.last(),
        Some(LoggedSpineLedgerEvent {
            seq: 8,
            event: SpineLedgerEvent::Msg { raw_ordinal: 0, .. }
        })
    ));
}

#[test]
fn open_append_failure_does_not_publish_parse_stack_or_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let parse_stack_before = runtime.parse_stack().clone();
    let ledger_events_before = runtime
        .ledger
        .events
        .iter()
        .map(|event| format!("{event:?}"))
        .collect::<Vec<_>>();
    let next_event_seq_before = runtime.ledger.next_event_seq;

    let request = spine_call(SPINE_TOOL_OPEN, "open-fails");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &request)
        .expect("observe open request");
    runtime
        .stage_open("open-fails".to_string(), "unpublished child".to_string())
        .expect("stage open");

    let blocked_root = dir.path().join("not-a-dir");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .maybe_commit_output("open-fails", None)
        .expect_err("open append should fail");
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(
        runtime
            .ledger
            .events
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>(),
        ledger_events_before
    );
    assert_eq!(runtime.ledger.next_event_seq, next_event_seq_before);
}

#[test]
fn abort_matching_pending_clears_control_call_without_durable_mutation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "work before interrupted next");
    let request = spine_call(SPINE_TOOL_NEXT, "stale-next");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record next request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe next request");
    runtime
        .stage_next(
            "stale-next".to_string(),
            "interrupted sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("stage next");

    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    assert!(runtime.control_call_ids.contains("stale-next"));
    assert!(matches!(
        runtime
            .pending_commit("stale-next")
            .expect("pending commit"),
        Some(SpinePendingCommit::Close { .. })
    ));

    assert!(runtime.abort_pending("stale-next"));
    assert!(
        runtime
            .pending_commit("stale-next")
            .expect("pending should be cleared")
            .is_none()
    );
    assert!(!runtime.control_call_ids.contains("stale-next"));
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);

    let next_request = spine_call(SPINE_TOOL_NEXT, "fresh-next");
    let next_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let next_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(next_request.clone()));
    runtime
        .observe_raw_items(1)
        .expect("record fresh next request");
    runtime
        .observe_context_item(next_ordinal, next_context_index, &next_request)
        .expect("observe fresh next request");
    runtime
        .stage_next(
            "fresh-next".to_string(),
            "fresh sibling".to_string(),
            "test node memory".to_string(),
        )
        .expect("fresh transition should stage after abort");
}

#[test]
fn abort_non_matching_pending_keeps_transition_until_stale_abort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "work before close");
    let request = spine_call(SPINE_TOOL_CLOSE, "close");
    let request_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let request_context_index = current_context_len(&runtime, &raw);
    raw.push(Some(request.clone()));
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(request_ordinal, request_context_index, &request)
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");

    assert!(!runtime.abort_pending("other-call"));
    assert!(runtime.control_call_ids.contains("close"));
    assert!(matches!(
        runtime.pending_commit("close").expect("pending close"),
        Some(SpinePendingCommit::Close { .. })
    ));

    assert_eq!(runtime.abort_any_pending().as_deref(), Some("close"));
    assert!(!runtime.control_call_ids.contains("close"));
    assert!(runtime.pending_commit("close").expect("cleared").is_none());
}

#[test]
fn try_commit_internal_failure_does_not_silently_abort_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    append_msg(&mut runtime, &mut raw, "work to compact");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    let mem_path = runtime.store.mem_path();
    std::fs::create_dir_all(&mem_path).expect("block mem ledger append with directory");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range(
                "1.1",
                suffix_start..raw.len(),
            )),
        )
        .expect_err("append_mem failure should fail commit");
    assert!(
        !err.to_string().is_empty(),
        "expected append_mem failure to surface"
    );
    assert!(matches!(
        runtime.pending_commit("close").expect("pending retained"),
        Some(SpinePendingCommit::Close { .. })
    ));
    assert!(
        runtime
            .stage_next(
                "new-next".to_string(),
                "blocked sibling".to_string(),
                "test node memory".to_string(),
            )
            .expect_err("pending must still block new transition")
            .to_string()
            .contains("another spine transition is already pending")
    );
    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);
}

// Clone and fork sidecar behavior.

#[test]
fn clone_for_rollout_fails_closed_when_visible_memory_body_is_missing() {
    assert_clone_for_rollout_fails_closed_when_visible_memory_body_is_missing();
}

#[test]
fn fork_missing_memory_artifact_fails_closed() {
    assert_clone_for_rollout_fails_closed_when_visible_memory_body_is_missing();
}

fn assert_clone_for_rollout_fails_closed_when_visible_memory_body_is_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let node = NodeId::root_epoch(1).child(1);
    source
        .append_event(&SpineLedgerEvent::Open {
            child: node.clone(),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: None,
            open_context_tokens: None,
            open_context_source: None,
        })
        .expect("append open");
    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 0,
            context_index: 0,
            from_user: true,
            user_anchor: None,
        })
        .expect("append msg");
    let close_seq = source
        .append_event(&SpineLedgerEvent::Close {
            node: node.clone(),
            boundary: 3,
            summary: "closed".to_string(),
            close_input_tokens: None,
            close_context_tokens: None,
        })
        .expect("append close");
    source
        .append_event(&manual_toolcall_event(1, 1, 2, 2))
        .expect("append close carrier toolcall");
    let body = "missing body";
    let mem = MemRecord {
        compact_id: "mem-missing".to_string(),
        kind: MemKind::Suffix,
        node: node.clone(),
        raw_start: 0,
        raw_end: 1,
        context_start: 0,
        context_end: 1,
        raw_live_hash: None,
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: "memory/mem-missing.md".to_string(),
        body_hash: sha1_hex(body.as_bytes()),
    };
    source.append_mem(&mem).expect("append missing mem ref");
    source
        .append_commit_marker(&SpineCommitMarker {
            version: COMMIT_MARKER_VERSION,
            op_id: "missing-body-close".to_string(),
            kind: SpineCommitKindMarker::Close,
            token_seq_start: close_seq,
            token_seq_end: close_seq + 2,
            raw_boundary: 3,
            raw_live_hash: None,
            memory_refs: vec![SpineCommitMemoryRef {
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
            }],
        })
        .expect("append close commit marker");

    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout, 3)
        .expect("capture clone boundary")
        .expect("source sidecar exists");
    let err = SpineStore::clone_for_rollout_with_raw_live(
        &boundary,
        &target_rollout,
        &[true, true, true],
    )
    .expect_err("missing visible memory body must fail closed");
    assert!(
        err.to_string().contains("No such file") || err.to_string().contains("os error 2"),
        "unexpected clone error: {err}"
    );
    assert!(
        !SpineStore::has_for_rollout(&target_rollout).expect("check unpublished target"),
        "failed clone must not publish the target locator"
    );

    let restored_body_path = source
        .write_memory_body(&mem.compact_id, body)
        .expect("restore missing body");
    assert_eq!(restored_body_path, mem.body_path);
    SpineStore::clone_for_rollout_with_raw_live(&boundary, &target_rollout, &[true, true, true])
        .expect("retry clone after restoring missing body");
    let target = SpineStore::for_rollout(&target_rollout).expect("target store after retry");
    let target_mems = target.mems().expect("target mems after retry");
    assert_eq!(
        target_mems
            .iter()
            .map(|record| record.compact_id.as_str())
            .collect::<Vec<_>>(),
        vec!["mem-missing"]
    );
    assert_eq!(
        target
            .read_memory_body(&target_mems[0])
            .expect("read cloned memory body"),
        body
    );
    assert_eq!(
        target
            .commit_markers_for_test()
            .expect("target commit markers")
            .len(),
        1
    );
}

#[test]
fn clone_for_rollout_rewrites_compact_checkpoint_memory_refs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let body_path = source
        .write_memory_body("root-1-1", body)
        .expect("write source body");
    let mem = MemRecord {
        compact_id: "root-1-1".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path.clone(),
        body_hash: sha1_hex(body.as_bytes()),
    };
    source.append_mem(&mem).expect("append mem");
    source
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 0,
            mem: "root-1-1".to_string(),
            next_open_index: 1,
            raw_live_hash: hash_raw_live(&[]),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");
    source
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &source_rollout,
            &mem,
            body,
            1,
            2,
            "memory/source-only.md".to_string(),
        ))
        .expect("append compact checkpoint");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[]);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    let checkpoint = target
        .compact_checkpoints()
        .expect("read target checkpoints")
        .pop()
        .expect("target checkpoint");

    assert_eq!(
        checkpoint.rollout_path,
        target_rollout.display().to_string()
    );
    assert_eq!(checkpoint.memory_refs[0].body_path, body_path);
    target
        .validate_compact_checkpoint_for_boundary(
            &target_rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect("cloned checkpoint should validate against target sidecar");
}

#[test]
fn compact_checkpoint_without_root_compact_marker_fails_validation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let body_path = store
        .write_memory_body("root-1-0", body)
        .expect("write body");
    let mem = MemRecord {
        compact_id: "root-1-0".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path.clone(),
        body_hash: sha1_hex(body.as_bytes()),
    };
    store.append_mem(&mem).expect("append mem");
    store
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &rollout, &mem, body, 0, 1, body_path,
        ))
        .expect("append compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect_err("checkpoint without RootCompact marker must fail closed");
    assert!(
        err.to_string().contains("is not preceded by RootCompact"),
        "unexpected checkpoint validation error: {err}"
    );
}

#[test]
fn compact_checkpoint_with_mismatched_root_memory_ref_fails_validation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let body_path = store
        .write_memory_body("root-1-0", body)
        .expect("write body");
    let mem = MemRecord {
        compact_id: "root-1-0".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path.clone(),
        body_hash: sha1_hex(body.as_bytes()),
    };
    store.append_mem(&mem).expect("append mem");
    store
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 0,
            mem: mem.compact_id.clone(),
            next_open_index: 1,
            raw_live_hash: hash_raw_live(&[]),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");
    let mut checkpoint = root_compact_checkpoint_for_memory(&rollout, &mem, body, 1, 2, body_path);
    checkpoint.memory_refs[0].source_token_seq_start = 0;
    store
        .append_compact_checkpoint(&checkpoint)
        .expect("append compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect_err("mismatched root compact memory ref must fail closed");
    assert!(
        err.to_string()
            .contains("does not match committed memory record"),
        "unexpected checkpoint validation error: {err}"
    );
}

#[test]
fn replacement_history_memory_ref_span_hash_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");

    let root_body = "root compact body";
    let root_body_path = store
        .write_memory_body("root-1-0", root_body)
        .expect("write root body");
    let root_mem = MemRecord {
        compact_id: "root-1-0".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: root_body_path.clone(),
        body_hash: sha1_hex(root_body.as_bytes()),
    };
    store.append_mem(&root_mem).expect("append root mem");

    let suffix_body = "suffix memory body";
    let suffix_body_path = store
        .write_memory_body("suffix-1-1", suffix_body)
        .expect("write suffix body");
    let suffix_mem = MemRecord {
        compact_id: "suffix-1-1".to_string(),
        kind: MemKind::Suffix,
        node: NodeId::root_epoch(1).child(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 1,
        context_end: 2,
        raw_live_hash: None,
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: suffix_body_path.clone(),
        body_hash: sha1_hex(suffix_body.as_bytes()),
    };
    store.append_mem(&suffix_mem).expect("append suffix mem");
    store
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 0,
            mem: root_mem.compact_id.clone(),
            next_open_index: 1,
            raw_live_hash: hash_raw_live(&[]),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");

    let replacement_history = vec![
        memory_response_item(root_body),
        memory_response_item(suffix_body),
    ];
    let replacement_history_hash =
        hash_response_items(&replacement_history).expect("hash replacement_history");
    let mut checkpoint =
        root_compact_checkpoint_for_memory(&rollout, &root_mem, root_body, 1, 2, root_body_path);
    checkpoint.context_len = replacement_history.len();
    checkpoint.h_ps_hash = replacement_history_hash.clone();
    checkpoint.replacement_history_hash = replacement_history_hash;
    checkpoint
        .memory_item_refs
        .push(CompactCheckpointMemoryItemRef {
            compact_id: suffix_mem.compact_id.clone(),
            context_index: 1,
            item_hash: hash_response_items(&[memory_response_item(suffix_body)])
                .expect("hash suffix memory item"),
        });
    checkpoint.memory_refs.push(CheckpointMemoryRef {
        compact_id: suffix_mem.compact_id.clone(),
        node_id: suffix_mem.node.to_string(),
        body_path: suffix_body_path,
        body_hash: suffix_mem.body_hash.clone(),
        source_raw_start: suffix_mem.raw_start,
        source_raw_end: suffix_mem.raw_end,
        source_context_start: 0,
        source_context_end: suffix_mem.context_end,
        source_token_seq_start: 0,
        source_token_seq_end: 1,
        open_input_tokens: suffix_mem.open_input_tokens,
        close_input_tokens: suffix_mem.close_input_tokens,
        open_context_tokens: suffix_mem.open_context_tokens,
        close_context_tokens: suffix_mem.close_context_tokens,
        closed_source_suffix_tokens: suffix_mem.closed_source_suffix_tokens,
        closed_memory_context_tokens: suffix_mem.closed_memory_context_tokens,
        open_context_source: suffix_mem.open_context_source,
        memory_output_tokens: suffix_mem.memory_output_tokens,
    });
    store
        .append_compact_checkpoint(&checkpoint)
        .expect("append corrupted compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(&rollout, &[], &[], 0, &replacement_history)
        .expect_err("corrupted suffix memory span must fail closed");
    assert!(
        err.to_string()
            .contains("does not match committed memory record"),
        "unexpected checkpoint validation error: {err}"
    );
}

#[test]
fn compact_checkpoint_same_boundary_hash_multiple_token_seq_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let body_path = store
        .write_memory_body("root-1-0", body)
        .expect("write body");
    let mem = MemRecord {
        compact_id: "root-1-0".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path.clone(),
        body_hash: sha1_hex(body.as_bytes()),
    };
    store.append_mem(&mem).expect("append mem");
    store
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 0,
            mem: mem.compact_id.clone(),
            next_open_index: 1,
            raw_live_hash: hash_raw_live(&[]),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append first root compact");
    store
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &rollout,
            &mem,
            body,
            1,
            2,
            body_path.clone(),
        ))
        .expect("append valid compact checkpoint");
    store
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 0,
            context_index: 0,
            from_user: true,
            user_anchor: None,
        })
        .expect("append non-root marker at second checkpoint predecessor");
    store
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &rollout, &mem, body, 3, 4, body_path,
        ))
        .expect("append ambiguous newer compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect_err("multiple compact token seq candidates must fail closed");
    assert!(
        err.to_string()
            .contains("ambiguous spine compact checkpoint token_seq"),
        "unexpected checkpoint validation error: {err}"
    );
}

#[test]
fn compact_checkpoint_same_boundary_hash_token_seq_multiple_records_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let store = SpineStore::create_for_rollout(&rollout).expect("create store");
    store
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    let body = "root compact body";
    let body_path = store
        .write_memory_body("root-1-0", body)
        .expect("write body");
    let mem = MemRecord {
        compact_id: "root-1-0".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 0,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(hash_raw_live(&[])),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path.clone(),
        body_hash: sha1_hex(body.as_bytes()),
    };
    store.append_mem(&mem).expect("append mem");
    store
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 0,
            mem: mem.compact_id.clone(),
            next_open_index: 1,
            raw_live_hash: hash_raw_live(&[]),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");

    let mut corrupted =
        root_compact_checkpoint_for_memory(&rollout, &mem, body, 1, 2, body_path.clone());
    corrupted.context_len += 1;
    store
        .append_compact_checkpoint(&corrupted)
        .expect("append corrupted compact checkpoint");
    store
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &rollout, &mem, body, 1, 2, body_path,
        ))
        .expect("append duplicate valid compact checkpoint");

    let err = store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &[],
            &[],
            0,
            &[memory_response_item(body)],
        )
        .expect_err("multiple compact proof records must fail closed");
    assert!(
        err.to_string()
            .contains("ambiguous spine compact checkpoint proof"),
        "unexpected checkpoint validation error: {err}"
    );
}

#[test]
fn clone_for_rollout_keeps_compact_checkpoint_for_matching_raw_live_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    source
        .append_event(&SpineLedgerEvent::Open {
            child: NodeId::root_epoch(1).child(1),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: None,
            open_context_tokens: None,
            open_context_source: None,
        })
        .expect("append open");
    let raw_live = vec![true, false];
    let raw_live_hash = hash_raw_live(&raw_live);
    let body = "root compact after rollback hole";
    let body_path = source
        .write_memory_body("root-1-2", body)
        .expect("write source body");
    let mem = MemRecord {
        compact_id: "root-1-2".to_string(),
        kind: MemKind::RootEpoch,
        node: NodeId::root_epoch(1),
        raw_start: 0,
        raw_end: 2,
        context_start: 0,
        context_end: 1,
        raw_live_hash: Some(raw_live_hash.clone()),
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: None,
        body_path: body_path,
        body_hash: sha1_hex(body.as_bytes()),
    };
    source.append_mem(&mem).expect("append mem");
    source
        .append_event(&SpineLedgerEvent::RootCompact {
            node: NodeId::root_epoch(1),
            boundary: 2,
            mem: "root-1-2".to_string(),
            next_open_index: 1,
            raw_live_hash: raw_live_hash.clone(),
            next_open_input_tokens: None,
            next_open_context_tokens: None,
        })
        .expect("append root compact");
    source
        .append_compact_checkpoint(&root_compact_checkpoint_for_memory(
            &source_rollout,
            &mem,
            body,
            2,
            3,
            "memory/source-only.md".to_string(),
        ))
        .expect("append compact checkpoint");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &raw_live);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert_eq!(
        target
            .compact_checkpoints()
            .expect("read target checkpoints")
            .len(),
        1
    );
    target
        .validate_compact_checkpoint_for_boundary(
            &target_rollout,
            &raw_live,
            &[],
            2,
            &[memory_response_item(body)],
        )
        .expect("rollback-hole checkpoint should validate against target sidecar");
}

#[test]
fn clone_boundary_excludes_future_compact_checkpoint_and_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let mut raw = Vec::new();
    let mut source_runtime =
        SpineRuntime::load_or_create(&source_rollout, 0).expect("create source runtime");
    append_msg(&mut source_runtime, &mut raw, "boundary-visible");
    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout, 1)
        .expect("capture boundary")
        .expect("source sidecar exists");

    source_runtime
        .root_compact_with_checkpoint(
            &source_rollout,
            "future compact body".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("future root compact");

    SpineStore::clone_for_rollout_with_raw_live(&boundary, &target_rollout, &[true])
        .expect("clone sidecar");
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert!(
        target.mems().expect("read target mem records").is_empty(),
        "fork boundary must not clone future memory bodies"
    );
    assert!(
        target
            .compact_checkpoints()
            .expect("read target compact checkpoints")
            .is_empty(),
        "fork boundary must not clone future compact checkpoints"
    );
    assert_eq!(
        target
            .events()
            .expect("read target events")
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[test]
fn clone_preserves_structural_seq_gaps_and_appends_after_max() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    source
        .append_event(&SpineLedgerEvent::Open {
            child: NodeId::root_epoch(1).child(1),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: None,
            open_context_tokens: None,
            open_context_source: None,
        })
        .expect("append root open");
    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 0,
            context_index: 0,
            from_user: true,
            user_anchor: None,
        })
        .expect("append dropped msg");
    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 1,
            context_index: 1,
            from_user: true,
            user_anchor: None,
        })
        .expect("append kept msg");

    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[false, true]);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    let cloned_events = target.events().expect("read target events");
    assert_eq!(
        cloned_events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 3]
    );

    let next_seq = target
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 2,
            context_index: 2,
            from_user: true,
            user_anchor: None,
        })
        .expect("append after gapped clone");
    assert_eq!(next_seq, 4);
}

#[test]
fn clone_preserves_pressure_seq_and_structural_refs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    source
        .append_event(&SpineLedgerEvent::Open {
            child: NodeId::root_epoch(1).child(1),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: None,
            open_context_tokens: None,
            open_context_source: None,
        })
        .expect("append root open");
    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 0,
            context_index: 0,
            from_user: true,
            user_anchor: None,
        })
        .expect("append dropped msg");
    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 1,
            context_index: 1,
            from_user: true,
            user_anchor: None,
        })
        .expect("append kept msg");
    source
        .append_pressure_event(&PressureEvent::OpenContextBaseline {
            node: NodeId::root_epoch(1).child(1),
            open_structural_seq: Some(1),
            observed_structural_seq: 3,
            observed_raw_ordinal: 2,
            observed_raw_live_hash: Some(hash_raw_live(&[false, true])),
            observed_context_index: 2,
            context_tokens: 7_000,
            input_tokens: Some(7_500),
            source: ContextBaselineSource::EstimatedFromLiveSuffix,
            estimated_live_suffix_tokens: Some(500),
        })
        .expect("append pressure event");

    let raw_items = vec![None, Some(text_item("kept"))];
    clone_for_rollout_with_raw_live(&source_rollout, &target_rollout, &[false, true]);
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert_eq!(
        target
            .events()
            .expect("read target events")
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 3]
    );
    assert_eq!(
        target
            .pressure_events()
            .expect("read target pressure")
            .iter()
            .map(|event| event.pressure_seq)
            .collect::<Vec<_>>(),
        vec![0]
    );

    let replayed = SpineRuntime::load_for_rollout_items(&target_rollout, &raw_items, &[])
        .expect("load target")
        .expect("target sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), None);

    let next_pressure_seq = target
        .append_pressure_event(&PressureEvent::OpenContextBaseline {
            node: NodeId::root_epoch(1).child(1),
            open_structural_seq: Some(1),
            observed_structural_seq: 4,
            observed_raw_ordinal: 2,
            observed_raw_live_hash: Some(hash_raw_live(&[false, true])),
            observed_context_index: 2,
            context_tokens: 8_000,
            input_tokens: Some(8_500),
            source: ContextBaselineSource::EstimatedFromLiveSuffix,
            estimated_live_suffix_tokens: Some(500),
        })
        .expect("append pressure after clone");
    assert_eq!(next_pressure_seq, 1);
}

#[test]
fn clone_boundary_excludes_future_structural_and_pressure_records() {
    assert_clone_boundary_excludes_future_structural_and_pressure_records();
}

#[test]
fn fork_preserves_context_pressure_metadata() {
    assert_clone_boundary_excludes_future_structural_and_pressure_records();
}

fn assert_clone_boundary_excludes_future_structural_and_pressure_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_rollout = dir.path().join("source.jsonl");
    let target_rollout = dir.path().join("target.jsonl");
    let source = SpineStore::create_for_rollout(&source_rollout).expect("create source store");
    source
        .append_event(&SpineLedgerEvent::Init { raw_start: 0 })
        .expect("append init");
    source
        .append_event(&SpineLedgerEvent::Open {
            child: NodeId::root_epoch(1).child(1),
            boundary: 0,
            index: 0,
            summary: "root".to_string(),
            open_input_tokens: None,
            open_context_tokens: None,
            open_context_source: None,
        })
        .expect("append root open");
    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 0,
            context_index: 0,
            from_user: true,
            user_anchor: None,
        })
        .expect("append kept msg");
    source
        .append_pressure_event(&PressureEvent::OpenContextBaseline {
            node: NodeId::root_epoch(1).child(1),
            open_structural_seq: Some(1),
            observed_structural_seq: 3,
            observed_raw_ordinal: 1,
            observed_raw_live_hash: Some(hash_raw_live(&[true])),
            observed_context_index: 1,
            context_tokens: 7_000,
            input_tokens: Some(7_500),
            source: ContextBaselineSource::EstimatedFromLiveSuffix,
            estimated_live_suffix_tokens: Some(500),
        })
        .expect("append checkpoint-visible pressure");
    let boundary = SpineCloneBoundary {
        source_rollout_path: source_rollout,
        raw_ordinal_limit: 1,
        structural_seq_limit: source.next_event_seq().expect("structural seq limit"),
        pressure_seq_watermark: source
            .next_pressure_seq()
            .expect("pressure seq limit")
            .checked_sub(1),
        trim_seq_watermark: source
            .next_trim_seq()
            .expect("trim seq limit")
            .checked_sub(1),
        trim_toolcall_seq_limit: source.next_event_seq().expect("structural seq limit"),
    };

    source
        .append_event(&SpineLedgerEvent::Msg {
            raw_ordinal: 0,
            context_index: 0,
            from_user: true,
            user_anchor: None,
        })
        .expect("append future structural event");
    source
        .append_pressure_event(&PressureEvent::OpenContextBaseline {
            node: NodeId::root_epoch(1).child(1),
            open_structural_seq: Some(1),
            observed_structural_seq: 4,
            observed_raw_ordinal: 1,
            observed_raw_live_hash: Some(hash_raw_live(&[true])),
            observed_context_index: 1,
            context_tokens: 11_000,
            input_tokens: Some(11_500),
            source: ContextBaselineSource::EstimatedFromLiveSuffix,
            estimated_live_suffix_tokens: Some(500),
        })
        .expect("append future pressure");

    SpineStore::clone_for_rollout_with_raw_live(&boundary, &target_rollout, &[true])
        .expect("clone sidecar");
    let target = SpineStore::for_rollout(&target_rollout).expect("target store");
    assert_eq!(
        target
            .events()
            .expect("read target events")
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        target
            .pressure_events()
            .expect("read target pressure")
            .iter()
            .map(|event| event.pressure_seq)
            .collect::<Vec<_>>(),
        vec![0]
    );
    let replayed =
        SpineRuntime::load_for_rollout_items(&target_rollout, &[Some(text_item("kept"))], &[])
            .expect("load target")
            .expect("target sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
}

// Close, reduce, and materialized history.

#[test]
fn spine_close_output_does_not_shift_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 0..3)),
        )
        .expect("commit close");

    let events = event_log(&runtime);
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                SpineLedgerEvent::Msg { raw_ordinal, .. } => Some(*raw_ordinal),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![2],
        "only the real child suffix item should shift as Msg"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            SpineLedgerEvent::Msg {
                raw_ordinal: 3 | 4,
                ..
            }
        )),
        "close request/output carriers must not shift as Msg"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SpineLedgerEvent::Close { .. }))
            .count(),
        1
    );
    assert!(matches!(
        events.last(),
        Some(SpineLedgerEvent::ToolCall { .. })
    ));
    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("close should reduce task tree into a tree node inside Nodes")
    };
    assert_eq!(nodes.len(), 2);
    let SpineTreeNode::SpineTree {
        meta,
        children,
        memory_path,
        trajs_path,
        ..
    } = &nodes[0]
    else {
        panic!("close should reduce to SpineTree")
    };
    assert!(matches!(
        &nodes[1],
        SpineTreeNode::ToolCallAsLeafNode { segments }
            if segments == &vec![tool_req(3, 1), tool_resp(4, 2)]
    ));
    assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
    assert_eq!(meta.index, 0);
    assert_eq!(meta.summary, "child");
    assert!(matches!(
        children.as_slice(),
        [
            SpineTreeNode::ToolCallAsLeafNode {
                segments,
            },
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 2,
                    context_index: 2,
                },
                ..
            },
        ] if segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
    ));
    assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
    assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));

    let memory_archive =
        std::fs::read_to_string(runtime.store.root.join(memory_path)).expect("memory archive");
    assert!(memory_archive.contains("compact_id: mem-1-1-1-0-3"));
    assert!(memory_archive.contains("source_context_range: [0..4)"));
    assert!(memory_archive.contains("# Spine Memory 1.1.1"));
    let trajs_archive =
        std::fs::read_to_string(runtime.store.root.join(trajs_path)).expect("trajs archive");
    assert!(trajs_archive.contains("raw raw_ordinal=2 context_index=2"));
}

#[test]
fn empty_task_tree_reduce_fails_without_archive_side_effects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let archive = runtime.archive();
    let node_id = NodeId::root_epoch(1).child(1);
    let open = Symbol::Control(ControlSymbol::Open(
        tree_meta(&archive, node_id.clone(), 0, "empty".to_string()).expect("meta"),
    ));
    let memory = memory_ref(
        &archive,
        "empty-memory".to_string(),
        node_id,
        sha1_hex(b"empty"),
        0..0,
        0..0,
        0..0,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let mut parse_stack = ParseStack {
        symbols: vec![open, Symbol::Control(ControlSymbol::Close(memory))],
    };

    let err = parse_stack
        .shift(SpineToken::End, &archive)
        .expect_err("open close without Nodes must fail");
    assert!(
        err.to_string()
            .contains("spine.close requires non-empty live suffix"),
        "unexpected empty task close error: {err}"
    );
    assert!(
        !runtime.store.root.join("nodes/1/1").exists(),
        "empty close must not archive a TaskTree"
    );
}

#[test]
fn task_tree_reduce_archive_failure_leaves_symbols_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = SpineArchive::staged_with_memory_body(
        dir.path().to_path_buf(),
        "bad-memory".to_string(),
        "wrong body".to_string(),
    );
    let node_id = NodeId::root_epoch(1).child(1);
    let meta = tree_meta(&archive, node_id.clone(), 0, "child".to_string()).expect("meta");
    let memory = memory_ref(
        &archive,
        "bad-memory".to_string(),
        node_id,
        sha1_hex(b"expected body"),
        0..1,
        0..1,
        1..2,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let mut parse_stack = ParseStack {
        symbols: vec![
            Symbol::Control(ControlSymbol::Open(meta)),
            Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                from_user: true,
                user_anchor: Some(1),
            }]),
            Symbol::Control(ControlSymbol::Close(memory)),
        ],
    };
    let before = parse_stack.symbols.clone();

    let err = parse_stack
        .shift(SpineToken::End, &archive)
        .expect_err("archive failure must abort close reduction");
    assert!(
        err.to_string().contains("staged memory body hash mismatch"),
        "unexpected archive failure: {err}"
    );
    assert_eq!(
        parse_stack.symbols, before,
        "failed close reduction must not pop/truncate the live symbols"
    );
}

#[test]
fn open_toolcall_leaf_makes_close_suffix_non_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "empty child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(2, 2, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(3, 3, &function_output("close"))
        .expect("observe close output");

    let commit = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range("1.1.1", 0..2)),
        )
        .expect("close open-only child")
        .expect("close should commit");
    assert!(matches!(commit, SpineCommitKind::Close));
    assert_eq!(runtime.store.mems().expect("read mems").len(), 1);
    assert!(
        runtime.store.root.join("memory/mem-1-1-1-0-2.md").exists(),
        "close must archive memory for the open toolcall suffix"
    );
    assert!(
        runtime.store.root.join("nodes/1/1/1").exists(),
        "close must archive the child TaskTree"
    );
}

#[test]
fn duplicate_close_call_id_does_not_create_second_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "dup-close"))
        .expect("observe close request");
    runtime
        .stage_close("dup-close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("dup-close")
        .expect("pending close should be readable")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("dup-close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "dup-close",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                suffix_start..close_request_index,
            )),
        )
        .expect("commit close");

    let events_after_first_commit = event_log_debug(&runtime);
    let mems_after_first_commit = runtime.store.mems().expect("read mems");
    assert_eq!(mems_after_first_commit.len(), 1);
    assert_eq!(
        runtime
            .maybe_commit_output(
                "dup-close",
                Some(memory_assembly_with_context_range("1.1.1", suffix_start..5)),
            )
            .expect("duplicate close output commit should be no-op"),
        None
    );
    assert_eq!(event_log_debug(&runtime), events_after_first_commit);
    assert_eq!(
        runtime
            .store
            .mems()
            .expect("read mems after duplicate")
            .len(),
        1
    );
}

#[test]
fn close_failure_does_not_mutate_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let err = runtime
        .maybe_commit_output("close", None)
        .expect_err("close without compact output must fail");
    assert!(
        err.to_string()
            .contains("spine.close requires a validated source plan for memory assembly"),
        "unexpected close failure: {err}"
    );

    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_some()
    );
}

#[test]
fn close_artifact_write_failure_does_not_publish_parse_stack_or_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let body_path = runtime.store.root.join("memory/mem-1-1-1-2-5.md");
    if let Some(parent) = body_path.parent() {
        std::fs::create_dir_all(parent).expect("create memory dir");
    }
    std::fs::create_dir_all(&body_path).expect("block memory body write with directory");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range("1.1.1", 2..5)),
        )
        .expect_err("artifact write failure should fail commit");
    assert!(
        !err.to_string().is_empty(),
        "expected artifact write failure to surface"
    );
    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert!(
        !runtime.store.root.join("nodes/1/1/1/Memory.md").exists(),
        "artifact failure must not flush node Memory.md"
    );
    assert!(
        matches!(
            runtime.pending_commit("close").expect("pending retained"),
            Some(SpinePendingCommit::Close { .. })
        ),
        "failed artifact commit should retain pending close"
    );
}

#[test]
fn close_persistence_failure_leaves_retryable_close_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    std::fs::create_dir(runtime.store.mem_path()).expect("poison mem ledger path");

    let err = runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                suffix_start..close_request_index,
            )),
        )
        .expect_err("close mem persistence failure must fail");
    assert!(
        err.to_string().contains("Is a directory")
            || err.to_string().contains("os error 21")
            || err.to_string().contains("Permission denied"),
        "unexpected close persistence failure: {err}"
    );

    assert_eq!(
        runtime.render_tree().expect("render tree after failure"),
        tree_before,
        "failed close must not publish the reduced task tree"
    );
    assert_eq!(
        event_log_debug(&runtime),
        events_before,
        "failed close must not publish ledger events"
    );
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Close(_)))),
        "failed close must retain the zero-width Close token for retry"
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_some()
    );
}

#[test]
fn close_retry_reduces_existing_pending_close_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    open_task(&mut runtime, &mut raw, "open", "child");
    append_msg(&mut runtime, &mut raw, "inside");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-retry");
    runtime
        .stage_close("close-retry".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime
        .pending_commit("close-retry")
        .expect("pending close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = current_context_len(&runtime, &raw) - 1;
    observe_function_output(&mut runtime, &mut raw, "close-retry");
    let memory_assembly =
        memory_assembly_with_context_range("1.1.1", suffix_start..close_request_index);

    let prepared = runtime
        .prepare_close_commit(
            Some(memory_assembly.clone()),
            SpineTokenBaselines::default(),
        )
        .expect("prepare close commit");
    runtime
        .parse_stack
        .shift_pending_close(prepared.memory.clone(), &runtime.archive())
        .expect("simulate retryable pending Close token");
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            ..,
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Close(_))
        ]
    ));

    let commit = runtime
        .maybe_commit_output("close-retry", Some(memory_assembly))
        .expect("retry close")
        .expect("close should commit on retry");
    assert!(matches!(commit, SpineCommitKind::Close { .. }));
    assert!(!matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            ..,
            Symbol::Control(ControlSymbol::Open(_)),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Close(_))
        ]
    ));
    assert_eq!(
        runtime
            .store
            .commit_markers_for_test()
            .expect("commit markers")
            .len(),
        1
    );
}

#[test]
fn close_retry_reuses_matching_prepared_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(1, 1, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("record child raw");
    runtime
        .observe_context_item(2, 2, &text_item("inside"))
        .expect("observe child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(3, 3, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    let close_request_index = 3;
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(4, 4, &function_output("close"))
        .expect("observe close output");

    let memory_assembly =
        memory_assembly_with_context_range("1.1.1", suffix_start..close_request_index);
    let compact_id = "mem-1-1-1-0-3";
    let prepared_mem = MemRecord {
        compact_id: compact_id.to_string(),
        kind: MemKind::Suffix,
        node: NodeId(vec![1, 1, 1]),
        raw_start: 0,
        raw_end: 3,
        context_start: suffix_start,
        context_end: close_request_index,
        raw_live_hash: None,
        open_input_tokens: None,
        close_input_tokens: None,
        open_context_tokens: None,
        close_context_tokens: None,
        closed_source_suffix_tokens: None,
        closed_memory_context_tokens: None,
        open_context_source: None,
        memory_output_tokens: memory_assembly.memory_output_tokens,
        body_path: format!("memory/{compact_id}.md"),
        body_hash: sha1_hex(memory_assembly.body.as_bytes()),
    };
    runtime
        .store
        .write_memory_body(&prepared_mem.compact_id, &memory_assembly.body)
        .expect("write prepared memory body");
    runtime
        .store
        .append_mem(&prepared_mem)
        .expect("append prepared mem");

    let commit = runtime
        .maybe_commit_output("close", Some(memory_assembly))
        .expect("retry close with matching prepared memory")
        .expect("close should commit");
    assert!(matches!(commit, SpineCommitKind::Close { .. }));
    assert_eq!(
        runtime.store.mems().expect("read mems after retry").len(),
        1,
        "retry must reuse matching suffix mem instead of appending duplicate"
    );
    assert_eq!(
        runtime
            .store
            .commit_markers_for_test()
            .expect("read commit markers")
            .len(),
        1,
        "retry should publish the explicit close commit proof"
    );
    assert!(
        runtime
            .pending_commit("close")
            .expect("pending close")
            .is_none(),
        "successful retry must clear pending close"
    );
}

#[test]
fn nested_close_reduces_inner_tree_into_parent_nodes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .observe_raw_items(1)
        .expect("record outer open request");
    runtime
        .observe_context_item(0, 0, &spine_call(SPINE_TOOL_OPEN, "outer"))
        .expect("observe outer open request");
    runtime
        .stage_open("outer".to_string(), "outer".to_string())
        .expect("stage outer open");
    runtime.observe_raw_items(1).expect("record outer output");
    runtime
        .observe_context_item(1, 1, &function_output("outer"))
        .expect("observe outer output");
    runtime
        .maybe_commit_output("outer", None)
        .expect("commit outer");

    runtime
        .observe_raw_items(1)
        .expect("record inner open request");
    runtime
        .observe_context_item(2, 2, &spine_call(SPINE_TOOL_OPEN, "inner"))
        .expect("observe inner open request");
    runtime
        .stage_open("inner".to_string(), "inner".to_string())
        .expect("stage inner open");
    runtime.observe_raw_items(1).expect("record inner output");
    runtime
        .observe_context_item(3, 3, &function_output("inner"))
        .expect("observe inner output");
    runtime
        .maybe_commit_output("inner", None)
        .expect("commit inner");

    runtime.observe_raw_items(1).expect("record inner raw");
    runtime
        .observe_context_item(4, 4, &text_item("inner body"))
        .expect("observe inner raw");
    runtime
        .observe_raw_items(1)
        .expect("record inner close request");
    runtime
        .observe_context_item(5, 5, &spine_call(SPINE_TOOL_CLOSE, "close-inner"))
        .expect("observe inner close request");
    runtime
        .stage_close("close-inner".to_string(), "test node memory".to_string())
        .expect("stage inner close");
    let inner_suffix_start = match runtime
        .pending_commit("close-inner")
        .expect("pending inner close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending inner close, got {other:?}"),
    };
    runtime
        .observe_raw_items(1)
        .expect("record inner close output");
    runtime
        .observe_context_item(6, 6, &function_output("close-inner"))
        .expect("observe inner close output");
    runtime
        .maybe_commit_output(
            "close-inner",
            Some(memory_assembly_with_context_range(
                "1.1.1.1",
                inner_suffix_start..5,
            )),
        )
        .expect("commit inner close");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::Control(ControlSymbol::Open(outer)),
            Symbol::SpineTreeNodes(nodes),
        ] if root.id == NodeId::root_epoch(1).child(1)
            && outer.id == NodeId::root_epoch(1).child(1).child(1)
            && matches!(
                nodes.as_slice(),
                [
                    SpineTreeNode::ToolCallAsLeafNode { segments: outer_open_segments },
                    SpineTreeNode::SpineTree { meta, .. },
                    SpineTreeNode::ToolCallAsLeafNode { segments },
                ]
                    if outer_open_segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
                        && meta.id == NodeId::root_epoch(1).child(1).child(1).child(1)
                        && meta.summary == "inner"
                        && segments == &vec![tool_req(5, 3), tool_resp(6, 4)]
            )
    ));

    runtime
        .observe_raw_items(1)
        .expect("record outer close request");
    runtime
        .observe_context_item(7, 7, &spine_call(SPINE_TOOL_CLOSE, "close-outer"))
        .expect("observe outer close request");
    runtime
        .stage_close("close-outer".to_string(), "test node memory".to_string())
        .expect("stage outer close");
    let outer_suffix_start = match runtime
        .pending_commit("close-outer")
        .expect("pending outer close")
    {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending outer close, got {other:?}"),
    };
    runtime
        .observe_raw_items(1)
        .expect("record outer close output");
    runtime
        .observe_context_item(8, 8, &function_output("close-outer"))
        .expect("observe outer close output");
    runtime
        .maybe_commit_output(
            "close-outer",
            Some(memory_assembly_with_context_range(
                "1.1.1",
                outer_suffix_start..7,
            )),
        )
        .expect("commit outer close");

    let Some(Symbol::SpineTreeNodes(root_nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("outer close should reduce to root Nodes")
    };
    assert!(matches!(
        root_nodes.as_slice(),
        [
            SpineTreeNode::SpineTree {
                meta,
                children,
                trajs_path,
                ..
            },
            SpineTreeNode::ToolCallAsLeafNode { segments },
        ] if meta.id == NodeId::root_epoch(1).child(1).child(1)
            && meta.summary == "outer"
            && segments == &vec![tool_req(7, 1), tool_resp(8, 2)]
            && matches!(
                children.as_slice(),
                [
                    SpineTreeNode::ToolCallAsLeafNode { segments: outer_open_segments },
                    SpineTreeNode::SpineTree { meta: inner, children: inner_children, .. },
                    SpineTreeNode::ToolCallAsLeafNode { segments: inner_close_segments },
                ] if outer_open_segments == &vec![tool_req(0, 0), tool_resp(1, 1)]
                    && inner.summary == "inner"
                    && matches!(
                        inner_children.as_slice(),
                        [
                            SpineTreeNode::ToolCallAsLeafNode { segments },
                            SpineTreeNode::MsgAsLeafNode { .. },
                        ] if segments == &vec![tool_req(2, 2), tool_resp(3, 3)]
                    )
                    && inner_close_segments == &vec![tool_req(5, 3), tool_resp(6, 4)]
            )
            && trajs_path == &PathBuf::from("nodes/1/1/1/Trajs.md")
    ));
    let outer_trajs = std::fs::read_to_string(runtime.store.root.join("nodes/1/1/1/Trajs.md"))
        .expect("outer trajs");
    assert!(outer_trajs.contains("compact_id=mem-1-1-1-1-2-5"));
    assert!(outer_trajs.contains("node_id=1.1.1.1"));
    assert!(outer_trajs.contains("body_path="));
    assert!(outer_trajs.contains("memory_path=nodes/1/1/1/1/Memory.md"));
    assert!(outer_trajs.contains("trajs_path=nodes/1/1/1/1/Trajs.md"));
    assert!(!outer_trajs.contains("body_hash:"));
    assert!(!outer_trajs.contains("body:"));
    assert!(!outer_trajs.contains("Spine Memory 1.1.1.1"));
    assert!(!outer_trajs.contains("inner assistant traj"));
}

#[test]
fn layer_1_2_4_example_trace_replays_shift_reduce() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root work");
    open_task(&mut runtime, &mut raw, "open-1-1", "task 1.1");
    append_msg(&mut runtime, &mut raw, "1.1 work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1.1");
    open_task(&mut runtime, &mut raw, "open-1-2", "task 1.2");
    append_msg(&mut runtime, &mut raw, "1.2 work");
    open_task(&mut runtime, &mut raw, "open-1-2-1", "task 1.2.1");
    append_msg(&mut runtime, &mut raw, "1.2.1 work");
    close_task(&mut runtime, &mut raw, "close-1-2-1", "1.1.2.1");
    open_task(&mut runtime, &mut raw, "open-1-2-2", "task 1.2.2");
    append_msg(&mut runtime, &mut raw, "1.2.2 work");
    close_task(&mut runtime, &mut raw, "close-1-2-2", "1.1.2.2");
    close_task(&mut runtime, &mut raw, "close-1-2", "1.1.2");
    append_msg(&mut runtime, &mut raw, "1.3 work");
    runtime
        .root_compact("root epoch 1 memory".to_string(), &raw)
        .expect("root compact");
    let post_compact_len = runtime
        .materialize_history(&raw)
        .expect("post-compact h(PS)")
        .len();
    append_msg(&mut runtime, &mut raw, "2.1 work");

    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
            Symbol::SpineTreeNodes(nodes),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == post_compact_len
            && matches!(
                nodes.as_slice(),
                [
                    SpineTreeNode::MsgAsLeafNode {
                        msg: SegRef::ResponseItem {
                            raw_ordinal,
                            context_index,
                        },
                        ..
                    }
                ] if *raw_ordinal == u64::try_from(raw.len() - 1).expect("ordinal")
                    && *context_index == post_compact_len
            )
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );

    let tree = replayed.parse_stack().render_tree().expect("render tree");
    assert!(tree.contains("[1] Done"), "{tree}");
    assert!(tree.contains("[2.1] Current"), "{tree}");
    assert!(
        !tree.contains("[1.2.1]") && !tree.contains("[1.2.2]"),
        "closed descendants of a previous root epoch must stay folded: {tree}"
    );

    let materialized = replayed.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 2);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("root epoch 1 memory")
            )
    ));
    assert_eq!(materialized[1], anchored_text_item(7, "2.1 work"));
}

// Fork isolation and replayed materialization.

#[test]
fn fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent() {
    assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent();
}

#[test]
fn fork_child_initial_h_ps_matches_parent() {
    assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent();
}

#[test]
fn fork_child_mutation_does_not_change_parent() {
    assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent();
}

#[test]
fn fork_rewrites_node_dir_to_child_sidecar() {
    assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent();
}

fn assert_fork_clone_rewrites_node_dirs_copies_artifacts_and_isolates_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent_rollout = dir.path().join("parent.jsonl");
    let child_rollout = dir.path().join("child.jsonl");
    let mut raw = Vec::new();
    let mut parent = SpineRuntime::load_or_create(&parent_rollout, 0).expect("create parent");

    append_msg(&mut parent, &mut raw, "parent root before child");
    open_task(&mut parent, &mut raw, "open-child", "child task");
    append_msg(&mut parent, &mut raw, "child work");
    close_task(&mut parent, &mut raw, "close-child", "1.1.1");
    append_msg(&mut parent, &mut raw, "parent after child");

    let parent_materialized = parent.materialize_history(&raw).expect("parent h(PS)");
    let parent_stack_before_child_work = parent.parse_stack().clone();
    let parent_tree_events_before_child_work = event_log_debug(&parent);
    let parent_root = parent.store.root.clone();

    let raw_live = vec![true; raw.len()];
    clone_for_rollout_with_raw_live(&parent_rollout, &child_rollout, &raw_live);
    let child = SpineRuntime::load_for_rollout_items(&child_rollout, &raw, &[])
        .expect("load child")
        .expect("child sidecar exists");
    let child_root = child.store.root.clone();

    assert_ne!(child_root, parent_root);
    assert_eq!(
        child.materialize_history(&raw).expect("child h(PS)"),
        parent_materialized,
        "fork child h(PS) must match parent at fork boundary"
    );

    let Some(Symbol::SpineTreeNodes(nodes)) = child.parse_stack().symbols.last() else {
        panic!("fork child should replay parent root nodes");
    };
    let child_meta_dir = match nodes.as_slice() {
        [
            SpineTreeNode::MsgAsLeafNode { .. },
            SpineTreeNode::SpineTree {
                meta,
                memory_path,
                trajs_path,
                children,
                ..
            },
            SpineTreeNode::ToolCallAsLeafNode { segments },
            SpineTreeNode::MsgAsLeafNode { .. },
        ] if segments == &vec![tool_req(4, 2), tool_resp(5, 3)] => {
            assert_eq!(meta.id, NodeId::root_epoch(1).child(1).child(1));
            assert!(meta.node_dir.starts_with(&child_root));
            assert!(!meta.node_dir.starts_with(&parent_root));
            assert_eq!(memory_path, &PathBuf::from("nodes/1/1/1/Memory.md"));
            assert_eq!(trajs_path, &PathBuf::from("nodes/1/1/1/Trajs.md"));
            assert!(matches!(
                children.as_slice(),
                [
                    SpineTreeNode::ToolCallAsLeafNode { segments },
                    SpineTreeNode::MsgAsLeafNode { .. },
                ] if segments == &vec![tool_req(1, 1), tool_resp(2, 2)]
            ));
            meta.node_dir.clone()
        }
        other => panic!("unexpected fork child nodes: {other:?}"),
    };
    let child_memory_archive =
        std::fs::read_to_string(child_meta_dir.join("Memory.md")).expect("child Memory.md");
    let child_trajs_archive =
        std::fs::read_to_string(child_meta_dir.join("Trajs.md")).expect("child Trajs.md");
    assert!(child_memory_archive.contains("Spine Memory 1.1.1"));
    assert!(child_trajs_archive.contains("raw raw_ordinal=3"));
    assert!(child_trajs_archive.contains("context_index=1"));
    assert!(child_meta_dir.join("Memory.md").exists());
    assert!(child_meta_dir.join("Trajs.md").exists());

    let mut child = child;
    open_task(&mut child, &mut raw, "child-open-only", "child-only task");
    append_msg(&mut child, &mut raw, "child-only work");
    close_task(&mut child, &mut raw, "child-close-only", "1.1.2");

    let reloaded_parent = SpineRuntime::load_for_rollout(&parent_rollout, parent.raw_len)
        .expect("reload parent")
        .expect("parent sidecar exists");
    assert_eq!(
        reloaded_parent.parse_stack(),
        &parent_stack_before_child_work
    );
    assert_eq!(
        event_log_debug(&reloaded_parent),
        parent_tree_events_before_child_work
    );
}

#[test]
fn open_close_replay_materializes_closed_child_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
        )
        .expect("commit close");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("[1.1] Current"));
    assert!(tree.contains("[1.1.1] Done child task"));

    let materialized = replayed
        .materialize_history(&raw)
        .expect("materialize history");
    assert_eq!(materialized.len(), 4);
    assert_eq!(materialized[0], anchored_text_item(1, "before"));
    assert!(matches!(
        &materialized[1],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
        )
    ));
    assert_eq!(materialized[2], spine_call(SPINE_TOOL_CLOSE, "close"));
    assert_eq!(materialized[3], function_output("close"));
}

#[test]
fn tree_renders_from_parse_stack_without_mutating_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");

    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
        )
        .expect("commit close");

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    let before = replayed.parse_stack().clone();
    let tree = replayed.render_tree().expect("render tree");
    assert_eq!(replayed.parse_stack(), &before);
    assert_eq!(
        tree,
        replayed.parse_stack().render_tree().expect("render ps")
    );
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("Spine Task Tree:"), "{tree}");
    assert!(tree.contains("[1.1] Current"), "{tree}");
    assert!(tree.contains("[1.1.1] Done child task"), "{tree}");
    assert!(
        tree.contains("memory=nodes/1/1/1/Memory.md")
            && tree.contains("trajs=nodes/1/1/1/Trajs.md"),
        "{tree}"
    );
}

#[test]
fn materialize_history_renders_from_parse_stack_memory_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        Some(function_output("open")),
        Some(text_item("inside")),
        Some(spine_call(SPINE_TOOL_CLOSE, "close")),
        Some(function_output("close")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime.observe_raw_items(1).expect("observe child item");
    runtime
        .observe_context_item(3, 3, &text_item("inside"))
        .expect("observe child item");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
        )
        .expect("commit close");

    let Some(Symbol::SpineTreeNodes(nodes)) = runtime.parse_stack().symbols.last() else {
        panic!("closed child should reduce into ParseStack nodes")
    };
    let memory = nodes
        .iter()
        .find_map(|node| match node {
            SpineTreeNode::SpineTree { memory, .. } => Some(memory),
            _ => None,
        })
        .expect("closed child memory ref");
    assert_eq!(memory.compact_id, "mem-1-1-1-1-4");
    assert_eq!(memory.source_context_range, 1..4);
    assert_eq!(memory.source_raw_range, 1..4);
    let memory_seg = SegRef::from_memory_ref(memory);
    assert!(matches!(
        &memory_seg,
        SegRef::Memory {
            memory_id,
            body_path,
        } if memory_id == "mem-1-1-1-1-4"
            && body_path.ends_with("memory/mem-1-1-1-1-4.md")
    ));
    let memory_only = ParseStack {
        symbols: vec![Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
            msg: memory_seg,
            from_user: true,
            user_anchor: None,
        }])],
    };
    let rendered_memory =
        render_parse_stack_to_context(&memory_only, &[]).expect("render SegRef::Memory");
    assert!(matches!(
        rendered_memory.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));

    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert_eq!(materialized.len(), 4);
    assert_eq!(materialized[0], anchored_text_item(1, "before"));
    assert!(matches!(
        &materialized[1],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("Spine Memory 1.1.1")
                        && text.contains("real compact body for 1.1.1")
            )
    ));
    assert_eq!(materialized[2], spine_call(SPINE_TOOL_CLOSE, "close"));
    assert_eq!(materialized[3], function_output("close"));
}

// Rollback replay over sparse raw history.

#[test]
fn materialization_skips_rolled_back_raw_items_without_shifting_ordinals() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("kept")),
        None,
        Some(text_item("after rollback")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(3).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept item");
    runtime
        .observe_context_item(2, 2, &text_item("after rollback"))
        .expect("observe surviving item");
    let materialized = runtime.materialize_history(&raw).expect("materialize");

    assert_eq!(
        materialized,
        vec![
            anchored_text_item(1, "kept"),
            anchored_text_item(2, "after rollback")
        ]
    );
}

#[test]
fn rollback_keeps_open_when_request_item_survives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(spine_call(SPINE_TOOL_OPEN, "open")),
        None,
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Open"), "{tree}");
    assert!(tree.contains("- [1.1.1] Current child task"), "{tree}");
    assert_eq!(
        replayed.materialize_history(&raw).expect("materialize"),
        vec![anchored_text_item(1, "before")]
    );
}

#[test]
fn rollback_skips_open_when_request_item_is_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        None,
        Some(function_output("open")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Current"), "{tree}");
    assert_eq!(
        replayed.materialize_history(&raw).expect("materialize"),
        vec![anchored_text_item(1, "before")]
    );
}

#[test]
fn rollback_hole_rejects_suffix_memory_span() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before")),
        Some(text_item("open request")),
        Some(function_output("open")),
        None,
        Some(function_output("close")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 1).expect("create spine");
    runtime
        .observe_context_item(0, 0, &text_item("before"))
        .expect("observe prefix");
    runtime.observe_raw_items(1).expect("record open request");
    runtime
        .observe_context_item(1, 1, &spine_call(SPINE_TOOL_OPEN, "open"))
        .expect("observe open request");
    runtime
        .stage_open("open".to_string(), "child task".to_string())
        .expect("stage open");
    runtime.observe_raw_items(1).expect("record open output");
    runtime
        .observe_context_item(2, 2, &function_output("open"))
        .expect("observe open output");
    runtime
        .maybe_commit_output("open", None)
        .expect("commit open");
    runtime
        .observe_raw_items(1)
        .expect("record rolled-back child raw");
    runtime
        .observe_context_item(3, 3, &text_item("rolled back child"))
        .expect("observe rolled-back child raw");
    runtime.observe_raw_items(1).expect("record close request");
    runtime
        .observe_context_item(4, 4, &spine_call(SPINE_TOOL_CLOSE, "close"))
        .expect("observe close request");
    runtime
        .stage_close("close".to_string(), "test node memory".to_string())
        .expect("stage close");
    let suffix_start = match runtime.pending_commit("close").expect("pending close") {
        Some(SpinePendingCommit::Close { suffix_start, .. }) => suffix_start,
        other => panic!("expected pending close, got {other:?}"),
    };
    runtime.observe_raw_items(1).expect("record close output");
    runtime
        .observe_context_item(5, 5, &function_output("close"))
        .expect("observe close output");
    runtime
        .maybe_commit_output(
            "close",
            Some(memory_assembly_with_ranges("1.1.1", suffix_start..4, 1..4)),
        )
        .expect("commit close");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect_err("suffix memory spanning a rollback hole must fail closed");
    assert!(
        err.to_string()
            .contains("memory mem-1-1-1-1-4 does not cover live raw evidence"),
        "unexpected materialization error: {err}"
    );
}

// Native root compact and root epoch behavior.

#[test]
fn native_compact_shifts_compact_and_new_root_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("before compact")),
        Some(text_item("more context")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(2).expect("record raw");
    runtime
        .observe_context_item(0, 0, &text_item("before compact"))
        .expect("observe first context item");
    runtime
        .observe_context_item(1, 1, &text_item("more context"))
        .expect("observe second context item");

    runtime
        .root_compact("root summary".to_string(), &raw)
        .expect("compact root");

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { summary, .. },
            SpineLedgerEvent::Msg { raw_ordinal: 0, .. },
            SpineLedgerEvent::Msg { raw_ordinal: 1, .. },
            SpineLedgerEvent::RootCompact {
                boundary: 2,
                next_open_index: 1,
                ..
            },
        ] if summary == "root"
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && root_epochs[0].memory.compact_id == "root-1-2"
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == 1
            && next_root.summary == "root"
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );
}

#[test]
fn root_compact_prepare_store_failure_retains_retryable_compact_without_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(
        &mut runtime,
        &mut raw,
        "context before root compact failure",
    );
    append_msg(
        &mut runtime,
        &mut raw,
        "more context before root compact failure",
    );
    let before_events = ledger_event_debug(&runtime);
    let blocked_root = dir.path().join("not-a-dir-root-compact");
    std::fs::write(&blocked_root, "file blocks sidecar dir").expect("write blocker file");
    runtime.store.root = blocked_root;

    runtime
        .prepare_root_compact_with_checkpoint(
            &rollout,
            "root compact memory that will fail before commit".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect_err("root compact prepare must fail while writing sidecar memory");
    assert_pending_compact_retry_state(&runtime, &before_events);
}

#[test]
fn root_depth_open_after_native_compact_can_close_and_open_sibling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "epoch one work");
    runtime
        .root_compact("root summary".to_string(), &raw)
        .expect("compact root");
    let _post_compact_len = runtime
        .materialize_history(&raw)
        .expect("materialize")
        .len();

    append_msg(&mut runtime, &mut raw, "epoch two child work");
    close_task(&mut runtime, &mut raw, "close-2-1", "2.1");

    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 2"), "{tree}");
    assert!(tree.contains("[1] Done"), "{tree}");
    assert!(tree.contains("[2] Current"), "{tree}");
    assert!(tree.contains("[2.1] Done"), "{tree}");

    let post_close_len = runtime
        .materialize_history(&raw)
        .expect("materialize after close")
        .len();
    open_task(&mut runtime, &mut raw, "open-2-2", "task 2.2");
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::SpineTreeNodes(_),
            Symbol::Control(ControlSymbol::Open(open)),
            Symbol::SpineTreeNodes(open_nodes),
        ] if root_epochs.len() == 1
            && open.id == NodeId::root_epoch(2).child(2)
            && open.index == post_close_len
            && open.summary == "task 2.2"
            && matches!(
                open_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(4, 4), tool_resp(5, 5)]
            )
    ));
}

#[test]
fn prepare_root_compact_does_not_install_final_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "before staged root compact");
    append_msg(&mut runtime, &mut raw, "more staged root compact context");
    let before_tree = runtime
        .render_tree()
        .expect("render before prepared root compact");
    let before_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot before prepared root compact");

    let prepared = runtime
        .prepare_root_compact_with_checkpoint(
            &rollout,
            "staged root compact body".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("prepare root compact");

    assert_eq!(
        runtime
            .render_tree()
            .expect("render after prepared root compact"),
        before_tree,
        "prepared root compact must not install the reduced ParseStack before host publication"
    );
    let staged_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after prepared root compact");
    assert_eq!(
        staged_snapshot.active_node_id, before_snapshot.active_node_id,
        "prepared root compact must not advance the live active node"
    );

    runtime.install_prepared_root_compact(prepared);
    let after_snapshot = runtime
        .build_tree_snapshot()
        .expect("snapshot after installing prepared root compact");
    assert_ne!(
        after_snapshot.active_node_id, before_snapshot.active_node_id,
        "installing prepared root compact should advance the live ParseStack"
    );
}

#[test]
fn root_compact_from_root_cursor_after_closing_first_child_opens_next_epoch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root child work");
    close_task(&mut runtime, &mut raw, "close-1-1", "1.1");
    let pre_compact = runtime.materialize_history(&raw).expect("materialize");

    let materialized = runtime
        .root_compact("root epoch summary after closing 1.1".to_string(), &raw)
        .expect("compact root cursor");
    assert_eq!(materialized.len(), 1);

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { child: first_child, .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Close { node: closed_node, .. },
            SpineLedgerEvent::ToolCall { .. },
            SpineLedgerEvent::RootCompact {
                node: compacted_epoch,
                next_open_index: 1,
                ..
            },
        ] if *first_child == NodeId::root_epoch(1).child(1)
            && *closed_node == NodeId::root_epoch(1).child(1)
            && *compacted_epoch == NodeId::root_epoch(1)
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.node_id == NodeId::root_epoch(1)
            && root_epochs[0].memory.source_context_range == (0..pre_compact.len())
            && next_root.id == NodeId::root_epoch(2).child(1)
            && next_root.index == materialized.len()
    ));

    let replayed = SpineRuntime::load_for_rollout(&rollout, runtime.raw_len)
        .expect("reload spine")
        .expect("sidecar exists");
    assert_eq!(
        replayed.parse_stack().symbols,
        runtime.parse_stack().symbols
    );
    let snapshot = replayed.build_tree_snapshot().expect("snapshot");
    assert_snapshot_is_self_contained_forest(&snapshot);
    assert_eq!(snapshot.active_node_id, "2.1");
}

#[test]
fn root_compact_separates_source_context_range_from_next_open_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw = vec![
        Some(text_item("root visible item 1")),
        Some(text_item("root visible item 2")),
        Some(text_item("root visible item 3")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(raw.len()).expect("record raw");
    for (index, item) in raw.iter().enumerate() {
        runtime
            .observe_context_item(
                u64::try_from(index).expect("raw ordinal"),
                index,
                item.as_ref().expect("raw item"),
            )
            .expect("observe context item");
    }

    let before_len = runtime
        .materialize_history(&raw)
        .expect("pre-compact h(PS)")
        .len();
    assert_eq!(before_len, 3);
    let materialized = runtime
        .root_compact("root compact summary".to_string(), &raw)
        .expect("compact root");
    assert_eq!(materialized.len(), 1);

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::RootCompact {
                next_open_index: 1,
                ..
            },
        ]
    ));
    assert!(matches!(
        runtime.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::RootEpoches(root_epochs),
            Symbol::Control(ControlSymbol::Open(next_root)),
        ] if root_epochs.len() == 1
            && root_epochs[0].memory.source_context_range == (0..before_len)
            && next_root.index == materialized.len()
    ));
}

#[test]
fn root_compact_keeps_close_tokens_without_next_open_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root visible work");
    runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata {
                close_input_tokens: Some(229_136),
                close_context_tokens: Some(230_871),
                next_open_input_tokens: None,
                next_open_context_tokens: None,
            },
        )
        .expect("compact root");

    let mems = runtime.store.mems().expect("mem records");
    assert_eq!(mems.len(), 1);
    assert_eq!(mems[0].close_input_tokens, Some(229_136));
    assert_eq!(mems[0].close_context_tokens, Some(230_871));
    assert_eq!(runtime.current_open_provider_input_tokens(), None);
    assert_eq!(runtime.current_open_context_baseline_source(), None);

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::RootCompact {
                next_open_input_tokens: None,
                next_open_context_tokens: None,
                ..
            },
        ]
    ));
}

#[test]
fn root_compact_ignores_next_open_handoff_tokens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root visible work");
    runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata {
                close_input_tokens: Some(111_222),
                close_context_tokens: Some(222_333),
                next_open_input_tokens: Some(12_345),
                next_open_context_tokens: Some(67_890),
            },
        )
        .expect("compact root");

    assert_eq!(runtime.current_open_input_tokens(), None);
    assert_eq!(runtime.current_open_provider_input_tokens(), None);
    assert_eq!(runtime.current_open_context_baseline_source(), None);

    let events = event_log(&runtime);
    assert!(matches!(
        events.as_slice(),
        [
            SpineLedgerEvent::Init { .. },
            SpineLedgerEvent::Open { .. },
            SpineLedgerEvent::Msg { .. },
            SpineLedgerEvent::RootCompact {
                next_open_input_tokens: None,
                next_open_context_tokens: None,
                ..
            },
        ]
    ));

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw, &[])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_input_tokens(), None);
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
    assert_eq!(replayed.current_open_context_baseline_source(), None);
}

#[test]
fn root_compact_checkpoint_validates_against_root_compact_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root visible work");
    let result = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("compact root with checkpoint");

    runtime
        .store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &runtime.raw_live,
            &raw,
            result.raw_boundary,
            &result.materialized,
        )
        .expect("runtime compact checkpoint should bind to RootCompact marker");
}

#[test]
fn root_compact_checkpoint_append_failure_can_retry_without_duplicate_mem() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root visible work");
    std::fs::create_dir_all(runtime.store.compact_checkpoint_path_for_test())
        .expect("block compact checkpoint append with directory");

    let err = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect_err("blocked compact checkpoint append should fail");
    assert!(
        !err.to_string().is_empty(),
        "checkpoint append failure should surface"
    );
    assert!(
        !event_log(&runtime)
            .iter()
            .any(|event| matches!(event, SpineLedgerEvent::RootCompact { .. })),
        "failed checkpoint append must not commit RootCompact marker"
    );
    assert!(
        runtime
            .parse_stack()
            .symbols
            .iter()
            .any(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Compact(..)))),
        "failed root compact must retain the zero-width Compact token for retry"
    );
    let mems_after_failure = runtime.store.mems().expect("read mems after failure");
    assert_eq!(
        mems_after_failure.len(),
        1,
        "failed checkpoint append leaves exactly one prepared root mem"
    );

    std::fs::remove_dir_all(runtime.store.compact_checkpoint_path_for_test())
        .expect("unblock compact checkpoint append");
    let result = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata::default(),
        )
        .expect("retry root compact after transient checkpoint failure");

    let mems_after_retry = runtime.store.mems().expect("read mems after retry");
    assert_eq!(
        mems_after_retry.len(),
        1,
        "retry must reuse matching root compact mem instead of appending duplicate"
    );
    runtime
        .store
        .validate_compact_checkpoint_for_boundary(
            &rollout,
            &runtime.raw_live,
            &raw,
            result.raw_boundary,
            &result.materialized,
        )
        .expect("retry checkpoint should validate against reused mem and RootCompact marker");
    assert!(!matches!(
        runtime.parse_stack().symbols.as_slice(),
        [.., Symbol::Control(ControlSymbol::Compact(..))]
    ));
}

#[test]
fn root_compact_new_root_accepts_post_compact_provider_baseline_capture() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    append_msg(&mut runtime, &mut raw, "root visible work");
    runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root compact summary".to_string(),
            &raw,
            SpineRootCompactTokenMetadata {
                close_input_tokens: Some(229_136),
                close_context_tokens: Some(230_871),
                next_open_input_tokens: None,
                next_open_context_tokens: None,
            },
        )
        .expect("compact root");
    assert_eq!(runtime.current_open_provider_input_tokens(), None);

    runtime
        .capture_current_open_provider_baseline(7_913)
        .expect("capture post-compact provider baseline");

    assert_eq!(runtime.current_open_input_tokens(), Some(7_913));
    assert_eq!(runtime.current_open_provider_input_tokens(), Some(7_913));
    assert_eq!(
        runtime.current_open_context_baseline_source(),
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
    assert_ne!(runtime.current_open_provider_input_tokens(), Some(230_871));
}

#[test]
fn native_compact_failure_leaves_parse_stack_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("record raw");
    runtime
        .observe_context_item(0, 0, &text_item("before failed compact"))
        .expect("observe context item");
    let parse_stack_before = runtime.parse_stack().clone();
    let tree_before = runtime.render_tree().expect("render tree before failure");
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();

    let err = runtime
        .root_compact(
            "   \n\t".to_string(),
            &[Some(text_item("before failed compact"))],
        )
        .expect_err("empty native compact body must fail closed");
    assert!(
        err.to_string()
            .contains("spine root compact memory body must not be empty"),
        "unexpected empty compact error: {err}"
    );

    assert_parse_stack_tree_and_events_unchanged(
        &runtime,
        &parse_stack_before,
        &tree_before,
        &events_before,
    );
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
}

#[test]
fn root_compact_staging_failure_does_not_write_memory_body() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let init_meta = crate::spine::archive::tree_meta(
        &runtime.archive(),
        NodeId::root_epoch(1),
        0,
        "root".to_string(),
    )
    .expect("build init meta");
    let open_meta = crate::spine::archive::tree_meta(
        &runtime.archive(),
        NodeId::root_epoch(1).child(1),
        0,
        "root".to_string(),
    )
    .expect("build open meta");
    let close_memory = crate::spine::archive::memory_ref(
        &runtime.archive(),
        "invalid-close".to_string(),
        NodeId::root_epoch(1).child(1),
        crate::spine::io::sha1_hex("invalid close".as_bytes()),
        0..0,
        0..0,
        1..2,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    );
    runtime.parse_stack.symbols = vec![
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Init(init_meta)),
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Open(open_meta)),
        crate::spine::model::Symbol::Control(crate::spine::model::ControlSymbol::Close(
            close_memory,
        )),
    ];
    let parse_stack_before = runtime.parse_stack().clone();
    let events_before = event_log_debug(&runtime);
    let mem_count_before = runtime
        .store
        .mems()
        .expect("read mems before failure")
        .len();
    let compact_checkpoint_count_before = runtime
        .store
        .compact_checkpoints()
        .expect("read checkpoints before failure")
        .len();

    let err = runtime
        .root_compact_with_checkpoint(
            &rollout,
            "root summary after invalid close".to_string(),
            &[],
            SpineRootCompactTokenMetadata::default(),
        )
        .expect_err("invalid staged parse stack should fail before commit");
    assert!(
        err.to_string()
            .contains("spine.close requires non-empty live suffix"),
        "unexpected staging failure error: {err}"
    );

    assert_eq!(runtime.parse_stack(), &parse_stack_before);
    assert_eq!(event_log_debug(&runtime), events_before);
    assert_eq!(
        runtime.store.mems().expect("read mems after failure").len(),
        mem_count_before
    );
    assert_eq!(
        runtime
            .store
            .compact_checkpoints()
            .expect("read checkpoints after failure")
            .len(),
        compact_checkpoint_count_before
    );
    assert!(
        !runtime.store.root.join("memory/root-1-0.md").exists(),
        "root compact body must not be written before staging succeeds"
    );
}

#[test]
fn root_compact_survives_rollback_without_new_raw_items() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(2).expect("record raw");
    runtime.raw_live = vec![true, false];
    runtime
        .root_compact(
            "root summary after rollback".to_string(),
            &raw_after_rollback,
        )
        .expect("compact root");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[])
        .expect("load spine")
        .expect("sidecar exists");
    let materialized = replayed
        .materialize_history(&raw_after_rollback)
        .expect("materialize");
    assert_eq!(materialized.len(), 1);
    assert!(matches!(
        &materialized[0],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }]
                    if text.contains("root summary after rollback")
            )
    ));
}

// Rollback checkpoints and recovery.

#[test]
fn checkpoint_before_user_msg_records_recoverable_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime
        .checkpoint_before_user_msg(&rollout, 0, &[])
        .expect("write checkpoint");
    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("first user"))
        .expect("shift user");

    let checkpoint = runtime
        .store
        .checkpoint_for_test(0)
        .expect("read checkpoint");
    assert_eq!(checkpoint.version, CHECKPOINT_VERSION);
    assert_eq!(checkpoint.checkpoint_id, "pre-user-00000000000000000000");
    assert_eq!(checkpoint.rollout_path, rollout.display().to_string());
    assert_eq!(checkpoint.raw_ordinal, 0);
    assert_eq!(checkpoint.token_seq, 2);
    assert_eq!(checkpoint.raw_live_hash, hash_raw_live(&[]));
    assert_eq!(checkpoint.context_len, 0);
    assert_eq!(checkpoint.cursor, "1.1");
    assert_eq!(
        checkpoint.parse_stack.symbols,
        vec![
            Symbol::Control(ControlSymbol::Init(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1),
                    0,
                    "root".to_string()
                )
                .expect("root meta")
            )),
            Symbol::Control(ControlSymbol::Open(
                tree_meta(
                    &runtime.archive(),
                    NodeId::root_epoch(1).child(1),
                    0,
                    "root".to_string()
                )
                .expect("root open meta")
            )),
        ]
    );
    assert_eq!(checkpoint.tree_meta.len(), 2);
    assert!(checkpoint.memory_refs.is_empty());
    assert!(checkpoint.trajs_refs.is_empty());
    assert_eq!(
        checkpoint.h_ps_hash,
        hash_response_items(&[]).expect("hash")
    );
}

#[test]
fn initial_checkpoint_records_root_open_without_msg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime
        .checkpoint_initial(&rollout, &[])
        .expect("write initial checkpoint");
    let checkpoint = runtime
        .store
        .initial_checkpoint_for_test()
        .expect("read initial checkpoint");

    assert_eq!(checkpoint.checkpoint_id, "initial");
    assert_eq!(checkpoint.raw_ordinal, 0);
    assert_eq!(checkpoint.context_len, 0);
    assert_eq!(checkpoint.cursor, "1.1");
    assert!(checkpoint.memory_refs.is_empty());
    assert!(checkpoint.trajs_refs.is_empty());
    assert!(matches!(
        checkpoint.parse_stack.symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root))
        ] if root.id == NodeId::root_epoch(1).child(1)
            && root.summary == "root"
    ));
}

#[test]
fn rollback_checkpoint_without_provider_baseline_has_no_node_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint before provider baseline");
    runtime
        .capture_current_open_provider_baseline(8_000)
        .expect("capture provider baseline after checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let checkpoint = runtime
        .store
        .checkpoint_for_test(1)
        .expect("read checkpoint");
    assert_eq!(checkpoint.pressure_seq_watermark, None);

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), None);
}

#[test]
fn rollback_checkpoint_replays_checkpoint_visible_provider_baseline() {
    assert_rollback_checkpoint_replays_checkpoint_visible_provider_baseline();
}

#[test]
fn rollback_restores_checkpoint_visible_provider_baseline() {
    assert_rollback_checkpoint_replays_checkpoint_visible_provider_baseline();
}

fn assert_rollback_checkpoint_replays_checkpoint_visible_provider_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .capture_current_open_provider_baseline(8_000)
        .expect("capture pre-checkpoint provider baseline");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint after provider baseline");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let checkpoint = runtime
        .store
        .checkpoint_for_test(1)
        .expect("read checkpoint");
    assert_eq!(checkpoint.pressure_seq_watermark, None);

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.current_open_provider_input_tokens(), Some(8_000));
}

#[test]
fn rollback_uses_pre_user_checkpoint_to_restore_parse_stack() {
    assert_rollback_uses_pre_user_checkpoint_to_restore_parse_stack();
}

#[test]
fn rollback_restores_parse_stack_before_target_user_msg() {
    assert_rollback_uses_pre_user_checkpoint_to_restore_parse_stack();
}

fn assert_rollback_uses_pre_user_checkpoint_to_restore_parse_stack() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");

    assert_eq!(
        replayed.parse_stack().symbols,
        vec![
            Symbol::Control(ControlSymbol::Init(
                tree_meta(
                    &replayed.archive(),
                    NodeId::root_epoch(1),
                    0,
                    "root".to_string()
                )
                .expect("root meta")
            )),
            Symbol::Control(ControlSymbol::Open(
                tree_meta(
                    &replayed.archive(),
                    NodeId::root_epoch(1).child(1),
                    0,
                    "root".to_string()
                )
                .expect("root open meta")
            )),
            Symbol::SpineTreeNodes(vec![SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                from_user: true,
                user_anchor: Some(1),
            }]),
        ]
    );
    assert_eq!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize"),
        vec![anchored_text_item(1, "kept")]
    );
}

#[test]
fn rollback_checkpoint_replays_new_live_append_after_cut() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(text_item("after rollback")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime.observe_raw_items(1).expect("observe new raw");
    runtime
        .observe_context_item(2, 1, &text_item("after rollback"))
        .expect("observe new user");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");

    assert_eq!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize"),
        vec![
            anchored_text_item(1, "kept"),
            anchored_text_item(3, "after rollback")
        ]
    );
    let Some(Symbol::SpineTreeNodes(nodes)) = replayed.parse_stack().symbols.last() else {
        panic!("expected root nodes after replay")
    };
    assert!(matches!(
        nodes.as_slice(),
        [
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 0,
                    context_index: 0,
                },
                ..
            },
            SpineTreeNode::MsgAsLeafNode {
                msg: SegRef::ResponseItem {
                    raw_ordinal: 2,
                    context_index: 1,
                },
                ..
            },
        ]
    ));
}

#[test]
fn rollback_checkpoint_rebuilds_cache_from_full_sidecar_before_new_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw_after_rollback = vec![Some(text_item("kept")), None];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    let full_sidecar_next_seq = runtime.ledger.next_event_seq;

    let mut replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    assert_eq!(replayed.ledger.next_event_seq, full_sidecar_next_seq);
    assert_eq!(
        replayed
            .materialize_history(&raw_after_rollback)
            .expect("materialize before append"),
        vec![anchored_text_item(1, "kept")]
    );

    raw_after_rollback.push(Some(text_item("after rollback")));
    replayed.observe_raw_items(1).expect("observe new raw");
    replayed
        .observe_context_item(2, 1, &text_item("after rollback"))
        .expect("append new raw after rollback replay");

    assert_eq!(replayed.ledger.next_event_seq, full_sidecar_next_seq + 1);
    let events = logged_events(&replayed);
    assert!(matches!(
        events.last(),
        Some(LoggedSpineLedgerEvent {
            seq,
            event: SpineLedgerEvent::Msg { raw_ordinal: 2, .. },
        }) if *seq == full_sidecar_next_seq
    ));
}

#[test]
fn rollback_checkpoint_new_open_reuses_restored_sibling_id() {
    assert_rollback_checkpoint_new_open_reuses_restored_sibling_id();
}

#[test]
fn rollback_allocates_correct_sibling_after_restore() {
    assert_rollback_checkpoint_new_open_reuses_restored_sibling_id();
}

fn assert_rollback_checkpoint_new_open_reuses_restored_sibling_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(spine_call(SPINE_TOOL_OPEN, "new-open")),
        Some(function_output("new-open")),
    ];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(1).expect("observe kept raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    let raw_before_rollback = vec![Some(text_item("kept"))];
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw_before_rollback)
        .expect("write checkpoint");
    runtime
        .observe_raw_items(1)
        .expect("observe rolled-back raw");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime
        .observe_raw_items(1)
        .expect("observe new open request");
    runtime
        .observe_context_item(2, 1, &spine_call(SPINE_TOOL_OPEN, "new-open"))
        .expect("observe new open request");
    runtime
        .stage_open("new-open".to_string(), "restored sibling".to_string())
        .expect("stage new open");
    runtime
        .observe_raw_items(1)
        .expect("observe new open output");
    runtime
        .observe_context_item(3, 2, &function_output("new-open"))
        .expect("observe new open output");
    runtime
        .maybe_commit_output("new-open", None)
        .expect("commit new open");

    let replayed = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect("load spine")
        .expect("sidecar exists");
    let tree = replayed.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.1"), "{tree}");
    assert!(tree.contains("- [1.1] Open"), "{tree}");
    assert!(
        tree.contains("- [1.1.1] Current restored sibling"),
        "{tree}"
    );
    assert!(matches!(
        replayed.parse_stack().symbols.as_slice(),
        [
            Symbol::Control(ControlSymbol::Init(_)),
            Symbol::Control(ControlSymbol::Open(root)),
            Symbol::SpineTreeNodes(nodes),
            Symbol::Control(ControlSymbol::Open(child)),
            Symbol::SpineTreeNodes(child_nodes),
        ] if root.id == NodeId::root_epoch(1).child(1)
            && matches!(
                nodes.as_slice(),
                [SpineTreeNode::MsgAsLeafNode {
                    msg: SegRef::ResponseItem {
                        raw_ordinal: 0,
                        context_index: 0,
                    },
                    ..
                }]
            )
            && child.id == NodeId::root_epoch(1).child(1).child(1)
            && child.index == 1
            && child.summary == "restored sibling"
            && matches!(
                child_nodes.as_slice(),
                [SpineTreeNode::ToolCallAsLeafNode { segments }]
                    if segments == &vec![tool_req(2, 1), tool_resp(3, 2)]
            )
    ));
}

#[test]
fn rollback_without_pre_user_checkpoint_fails_closed() {
    assert_rollback_without_pre_user_checkpoint_fails_closed();
}

#[test]
fn rollback_does_not_parse_rendered_history() {
    assert_rollback_does_not_parse_rendered_history();
}

fn assert_rollback_without_pre_user_checkpoint_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let raw_after_rollback = vec![Some(text_item("kept")), None, Some(text_item("new turn"))];

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    runtime.observe_raw_items(3).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &text_item("kept"))
        .expect("observe kept");
    runtime
        .observe_context_item(1, 1, &text_item("rolled back"))
        .expect("observe rolled-back user");
    runtime
        .observe_context_item(2, 1, &text_item("new turn"))
        .expect("observe new user");

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("rollback without checkpoint must fail closed");
    assert!(
        err.to_string()
            .contains("missing spine rollback checkpoint before raw ordinal 1"),
        "unexpected error: {err}"
    );
}

fn assert_rollback_does_not_parse_rendered_history() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut raw = Vec::new();

    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    append_msg(&mut runtime, &mut raw, "kept");
    runtime
        .checkpoint_before_user_msg(&rollout, 1, &raw)
        .expect("write rollback checkpoint");
    append_msg(&mut runtime, &mut raw, "rolled back");
    open_task(&mut runtime, &mut raw, "rendered-open", "rendered child");
    append_msg(&mut runtime, &mut raw, "rendered child work");
    close_task(&mut runtime, &mut raw, "rendered-close", "1.1.1");

    let rendered_history = runtime
        .materialize_history(&raw)
        .expect("materialize plausible rendered h(PS)");
    let rendered_memory = rendered_history
        .iter()
        .find(|item| {
            matches!(
                item,
                ResponseItem::Message { content, .. }
                    if matches!(
                        content.as_slice(),
                        [ContentItem::InputText { text }]
                            if text.contains("<spine_memory>")
                                && text.contains("Spine Memory 1.1.1")
                    )
            )
        })
        .cloned()
        .expect("rendered h(PS) should include plausible closed-child memory");
    let rendered_tree = runtime.render_tree().expect("render plausible tree");
    assert!(rendered_tree.contains("[1.1.1] Done rendered child"));

    std::fs::remove_file(runtime.store.checkpoint_path(1)).expect("remove rollback checkpoint");
    let raw_after_rollback = vec![
        Some(text_item("kept")),
        None,
        Some(rendered_memory),
        Some(text_item(&format!("Spine Task Tree:\n{rendered_tree}"))),
    ];

    let err = SpineRuntime::load_for_rollout_items(&rollout, &raw_after_rollback, &[1])
        .expect_err("rollback must fail closed instead of parsing rendered text");
    assert!(
        err.to_string()
            .contains("missing spine rollback checkpoint before raw ordinal 1"),
        "unexpected error: {err}"
    );
}

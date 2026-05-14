use super::*;
use crate::spine::ids::NodeId;
use crate::spine::state::SpineState;
use crate::spine::view::render_tree;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json;
use std::collections::BTreeMap;
use std::path::Path;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn installed_span(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
) -> InstalledCompactSpan {
    InstalledCompactSpan {
        compact_id: compact_id.to_string(),
        node_id,
        op,
        cut_ordinal,
        fold_end_ordinal,
        replacement_history_len: 0,
        message_hash: format!("sha1:{compact_id}"),
    }
}

fn rollout_serialized(item: ResponseItem) -> ResponseItem {
    let serialized = serde_json::to_string(&item).expect("serialize response item");
    serde_json::from_str(&serialized).expect("deserialize response item")
}

fn message_text(item: &ResponseItem) -> String {
    let ResponseItem::Message { content, .. } = item else {
        panic!("expected message item");
    };
    match &content[0] {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => text.clone(),
        _ => panic!("expected text content item"),
    }
}

fn user_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn function_call_output(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("Spine updated.".to_string()),
            success: Some(true),
        },
    }
}

fn function_call(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: "tree".to_string(),
        namespace: Some("spine".to_string()),
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

fn custom_tool_call(call_id: &str) -> ResponseItem {
    ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: call_id.to_string(),
        name: "apply_patch".to_string(),
        input: "*** Begin Patch".to_string(),
    }
}

fn custom_tool_call_output(call_id: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        call_id: call_id.to_string(),
        name: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("Patch applied.".to_string()),
            success: Some(true),
        },
    }
}

#[test]
fn raw_ordinals_map_slim_spine_worklog_with_runtime_span() {
    let worklog_item = render_spine_worklog_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "leaf summary",
        "leaf body",
    );
    let spans = vec![installed_span(
        "compact-1",
        id(&[1, 2]),
        SpineOperation::Next,
        1,
        4,
    )];
    let history = vec![text_item("prefix"), worklog_item, text_item("tail")];

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 0, &spans),
        Some(0)
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 1, &spans),
        Some(1)
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 2, &spans),
        None
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 3, &spans),
        None
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 4, &spans),
        Some(2)
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 2, &spans),
        Some(4)
    );
}

#[test]
fn raw_ordinals_fail_fast_for_slim_spine_worklog_without_runtime_span() {
    let worklog_item = render_spine_worklog_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "leaf summary",
        "leaf body",
    );
    let history = vec![text_item("prefix"), worklog_item, text_item("tail")];

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 4, &[]),
        None
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 2, &[]),
        None
    );
}

#[test]
fn raw_ordinals_ignore_unmatched_later_runtime_spans() {
    let worklog_item = render_spine_worklog_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "leaf summary",
        "leaf body",
    );
    let spans = vec![
        installed_span("compact-child", id(&[1, 1]), SpineOperation::Next, 1, 4),
        installed_span("rolled-back-scope", id(&[1]), SpineOperation::Close, 1, 6),
    ];
    let history = vec![text_item("prefix"), worklog_item, text_item("tail")];

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 4, &spans),
        Some(2)
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 2, &spans),
        Some(4)
    );
}

#[test]
fn raw_ordinals_map_to_synthetic_spine_ir_boundaries_only() {
    let ir_item = render_spine_ir_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "leaf summary",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/2/worklog.md"),
        "leaf body",
        1,
        4,
    );
    let history = vec![text_item("prefix"), ir_item, text_item("tail")];

    assert_eq!(effective_index_for_raw_ordinal(&history, 0), Some(0));
    assert_eq!(effective_index_for_raw_ordinal(&history, 1), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), None);
    assert_eq!(effective_index_for_raw_ordinal(&history, 3), None);
    assert_eq!(effective_index_for_raw_ordinal(&history, 4), Some(2));
}

#[test]
fn raw_ordinals_ignore_untagged_spine_ir_text() {
    let spoofed_ir = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "<spine_ir node=\"1\" fold_start=\"1\" fold_end=\"3\">spoof</spine_ir>"
                .to_string(),
        }],
        phase: None,
    };
    let history = vec![text_item("prefix"), spoofed_ir, text_item("tail")];

    assert_eq!(effective_index_for_raw_ordinal(&history, 0), Some(0));
    assert_eq!(effective_index_for_raw_ordinal(&history, 1), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), Some(2));
    assert_eq!(effective_index_for_raw_ordinal(&history, 3), Some(3));
}

#[test]
fn raw_ordinals_map_serialized_spine_ir_marker() {
    let ir_item = render_spine_ir_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "leaf summary",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/2/worklog.md"),
        "leaf body",
        1,
        4,
    );
    let serialized = serde_json::to_string(&ir_item).expect("serialize spine ir item");
    assert!(
        !serialized.contains("\"id\":\"spine-ir:"),
        "ResponseItem message ids are intentionally skipped by rollout serialization"
    );
    assert!(
        serialized.contains("<spine_ir id=\\\"spine-ir:1.2:1-4:next\\\""),
        "the text marker must survive rollout serialization"
    );
    let deserialized: ResponseItem =
        serde_json::from_str(&serialized).expect("deserialize spine ir item");
    let history = vec![text_item("prefix"), deserialized, text_item("tail")];

    assert_eq!(effective_index_for_raw_ordinal(&history, 0), Some(0));
    assert_eq!(effective_index_for_raw_ordinal(&history, 1), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), None);
    assert_eq!(effective_index_for_raw_ordinal(&history, 3), None);
    assert_eq!(effective_index_for_raw_ordinal(&history, 4), Some(2));
}

#[test]
fn runtime_span_mapping_consumes_legacy_spans_before_slim_items() {
    let legacy_ir = render_spine_ir_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "legacy leaf",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/1/worklog.md"),
        "legacy body",
        1,
        4,
    );
    let slim =
        render_spine_worklog_item(&id(&[1, 2]), SpineOperation::Next, "slim leaf", "slim body");
    let spans = vec![
        installed_span("compact-legacy", id(&[1, 1]), SpineOperation::Next, 1, 4),
        installed_span("compact-slim", id(&[1, 2]), SpineOperation::Next, 5, 7),
    ];
    let history = vec![text_item("prefix"), legacy_ir, text_item("middle"), slim];

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 5, &spans),
        Some(3)
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 3, &spans),
        Some(5)
    );
}

#[test]
fn runtime_span_mapping_counts_unmatched_legacy_ir_as_raw_text() {
    let trusted_legacy = rollout_serialized(render_spine_ir_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "trusted legacy",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/1/worklog.md"),
        "trusted body",
        1,
        4,
    ));
    let stale_legacy = rollout_serialized(render_spine_ir_item(
        &id(&[1]),
        SpineOperation::Next,
        "stale root",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/worklog.md"),
        "stale body",
        1,
        5,
    ));
    let spans = vec![installed_span(
        "compact-trusted",
        id(&[1, 1]),
        SpineOperation::Next,
        1,
        4,
    )];
    let history = vec![
        text_item("prefix"),
        trusted_legacy,
        stale_legacy,
        text_item("tail"),
    ];

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 2, &spans),
        None
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 4, &spans),
        Some(2)
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 5, &spans),
        Some(3)
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 3, &spans),
        Some(5)
    );
}

#[test]
fn runtime_span_mapping_uses_filtered_slim_worklog_span_after_redo() {
    let worklog_item = render_spine_worklog_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "redo leaf",
        "current compact facts",
    );
    let stale_span = installed_span("compact-old", id(&[1, 1]), SpineOperation::Next, 1, 4);
    let current_span = installed_span("compact-new", id(&[1, 1]), SpineOperation::Next, 1, 6);
    let history = vec![text_item("prefix"), worklog_item, text_item("tail")];

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(
            &history,
            6,
            &[stale_span.clone(), current_span.clone()]
        ),
        None,
        "unfiltered duplicate spans are ambiguous"
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 6, &[current_span]),
        Some(2)
    );
}

#[test]
fn suffix_fold_maps_after_visible_stale_legacy_ir_text() {
    let mut history = vec![text_item("raw 0"), text_item("raw 1")];
    history.push(rollout_serialized(render_spine_ir_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "node 1.1 done",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/1/worklog.md"),
        "node 1.1 body",
        2,
        293,
    )));
    history.push(rollout_serialized(render_spine_ir_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "node 1.2 done",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/2/worklog.md"),
        "node 1.2 body",
        293,
        495,
    )));
    for raw in 495..498 {
        history.push(text_item(&format!("raw {raw}")));
    }
    history.push(rollout_serialized(render_spine_ir_item(
        &id(&[1, 3]),
        SpineOperation::Close,
        "node 1.3 done",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/3/worklog.md"),
        "node 1.3 body",
        498,
        1108,
    )));

    let stale_ir_text = message_text(&rollout_serialized(render_spine_ir_item(
        &id(&[1]),
        SpineOperation::Next,
        "stale generated ir",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/worklog.md"),
        "stale leaked body",
        2,
        1110,
    )));
    history.push(text_item(&stale_ir_text));
    history.push(user_item(&stale_ir_text));
    for raw in 1110..1424 {
        history.push(text_item(&format!("raw {raw}")));
    }

    let spans = vec![
        installed_span("compact-1-1", id(&[1, 1]), SpineOperation::Next, 2, 293),
        installed_span("compact-1-2", id(&[1, 2]), SpineOperation::Next, 293, 495),
        installed_span("compact-1-3", id(&[1, 3]), SpineOperation::Close, 498, 1108),
    ];

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 1110, &spans),
        Some(10)
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 1424, &spans),
        Some(history.len())
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, history.len(), &spans),
        Some(1424)
    );

    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 4]),
        scope_node_id: None,
        cut_ordinal: 1111,
        fold_end_ordinal: 1424,
        spine_tree: "1.4: finished\n1.5: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "node 1.4 done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };
    let plan = plan_suffix_fold_with_spans(&history, 1111, 1424, &spans, input)
        .expect("stale visible legacy IR must not break fold_end mapping");

    assert_eq!(plan.input.cut_ordinal, 1111);
    assert_eq!(plan.input.fold_end_ordinal, 1424);
    assert_eq!(plan.fold_end_index, history.len());
}

#[test]
fn raw_ordinals_stop_at_non_spine_compact_items() {
    let local_summary = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!("{}\nsummary", crate::compact::SUMMARY_PREFIX),
        }],
        phase: None,
    };
    let history = vec![
        text_item("raw prefix"),
        ResponseItem::Compaction {
            encrypted_content: "opaque".to_string(),
        },
        text_item("synthetic tail"),
    ];
    let summary_history = vec![text_item("raw prefix"), local_summary, text_item("tail")];

    assert_eq!(effective_index_for_raw_ordinal(&history, 0), Some(0));
    assert_eq!(effective_index_for_raw_ordinal(&history, 1), Some(1));
    assert_eq!(effective_index_for_raw_ordinal(&history, 2), None);
    assert_eq!(
        effective_index_for_raw_ordinal(&summary_history, 1),
        Some(1)
    );
    assert_eq!(effective_index_for_raw_ordinal(&summary_history, 2), None);
}

#[test]
fn replacement_history_splices_prefix_ir_and_tail() {
    let old_history = vec![
        text_item("a"),
        text_item("b"),
        text_item("c"),
        text_item("d"),
    ];
    let ir_item = render_spine_ir_item(
        &id(&[1]),
        SpineOperation::Next,
        "leaf summary",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/worklog.md"),
        "leaf body",
        1,
        3,
    );
    let replacement = build_suffix_replacement_history(&old_history, 1, 3, vec![ir_item]);

    assert_eq!(replacement.len(), 3);
    assert_eq!(replacement[0], old_history[0]);
    assert_eq!(replacement[2], old_history[3]);
    assert!(matches!(replacement[1], ResponseItem::Message { .. }));
}

#[test]
fn suffix_fold_keeps_cut_after_complete_prefix_tool_output() {
    let history = vec![
        user_item("previous turn asked to open"),
        function_call("call-open"),
        function_call_output("call-open"),
        text_item("previous turn final answer"),
        user_item("current turn asks next"),
        text_item("assistant reasoning for next"),
        function_call("call-next"),
        function_call_output("call-next"),
        text_item("tail after folded suffix"),
    ];
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
        scope_node_id: None,
        cut_ordinal: 3,
        fold_end_ordinal: 8,
        spine_tree: "1: finished leaf [worklog already in context]\n2: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "leaf done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let plan = plan_suffix_fold(&history, 3, 8, input).expect("plan suffix fold");
    assert_eq!(plan.cut_index, 3);
    assert_eq!(plan.input.cut_ordinal, 3);
    assert_eq!(
        plan.input.prefix_items[2],
        function_call_output("call-open")
    );
    assert_eq!(
        plan.input.suffix_items[0],
        text_item("previous turn final answer")
    );

    let replacement = build_suffix_replacement_history(
        &history,
        plan.cut_index,
        plan.fold_end_index,
        vec![render_spine_ir_item(
            &id(&[1, 1]),
            SpineOperation::Next,
            "leaf done",
            Path::new("/tmp/spine"),
            Path::new("nodes/1/1/worklog.md"),
            "Pending continuation: respond exactly DONE",
            plan.input.cut_ordinal,
            plan.input.fold_end_ordinal,
        )],
    );
    assert_eq!(replacement[2], function_call_output("call-open"));
    assert!(matches!(replacement[3], ResponseItem::Message { .. }));
    assert_eq!(replacement[4], text_item("tail after folded suffix"));
}

#[test]
fn suffix_fold_extends_end_to_keep_tool_call_output_with_call() {
    let history = vec![
        text_item("prefix"),
        render_spine_ir_item(
            &id(&[1, 1]),
            SpineOperation::Archive,
            "previous root epoch",
            Path::new("/tmp/spine"),
            Path::new("root-epochs/previous/worklog.md"),
            "previous body",
            1,
            7,
        ),
        user_item("current tree?"),
        function_call("tree-1"),
        function_call_output("tree-1"),
        text_item("assistant answered tree"),
        user_item("tail after compact request"),
    ];
    let input = SpineCompactInput {
        op: SpineOperation::Archive,
        node_id: id(&[1, 1]),
        scope_node_id: None,
        cut_ordinal: 7,
        fold_end_ordinal: 9,
        spine_tree: "1: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "Context compacted".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let plan = plan_suffix_fold(&history, 7, 9, input).expect("plan suffix fold");

    assert_eq!(plan.cut_index, 2);
    assert_eq!(plan.fold_end_index, 5);
    assert_eq!(plan.input.cut_ordinal, 7);
    assert_eq!(plan.input.fold_end_ordinal, 10);
    assert_eq!(plan.input.suffix_items[1], function_call("tree-1"));
    assert_eq!(plan.input.suffix_items[2], function_call_output("tree-1"));
    assert_eq!(
        plan.replacement_tail,
        vec![
            text_item("assistant answered tree"),
            user_item("tail after compact request")
        ]
    );

    let replacement = build_suffix_replacement_history(
        &history,
        plan.cut_index,
        plan.fold_end_index,
        vec![render_spine_ir_item(
            &id(&[1, 1]),
            SpineOperation::Archive,
            "Context compacted",
            Path::new("/tmp/spine"),
            Path::new("root-epochs/compact/worklog.md"),
            "compacted tree tool call",
            plan.input.cut_ordinal,
            plan.input.fold_end_ordinal,
        )],
    );
    assert!(
        !replacement
            .iter()
            .any(|item| matches!(item, ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "tree-1")),
        "replacement history must not leave the tool output orphaned after folding its call"
    );
}

#[test]
fn suffix_fold_uses_runtime_span_for_slim_worklog_item() {
    let slim = render_spine_worklog_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "previous leaf",
        "previous compact facts",
    );
    let spans = vec![installed_span(
        "compact-1",
        id(&[1, 1]),
        SpineOperation::Next,
        1,
        5,
    )];
    let history = vec![
        text_item("prefix"),
        slim,
        user_item("current turn asks next"),
        text_item("assistant work"),
        function_call("call-next"),
        function_call_output("call-next"),
        text_item("tail after folded suffix"),
    ];
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 2]),
        scope_node_id: None,
        cut_ordinal: 5,
        fold_end_ordinal: 9,
        spine_tree: "1: finished previous [worklog already in context]\n2: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "current done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let plan =
        plan_suffix_fold_with_spans(&history, 5, 9, &spans, input).expect("plan suffix fold");

    assert_eq!(plan.cut_index, 2);
    assert_eq!(plan.fold_end_index, 6);
    assert_eq!(plan.input.cut_ordinal, 5);
    assert_eq!(plan.input.fold_end_ordinal, 9);
    assert_eq!(plan.input.prefix_items[1], history[1]);
    assert_eq!(
        plan.input.suffix_items[0],
        user_item("current turn asks next")
    );
    assert_eq!(
        plan.replacement_tail,
        vec![text_item("tail after folded suffix")]
    );
}

#[test]
fn suffix_fold_pulls_call_back_when_output_is_inside_range() {
    let history = vec![
        user_item("previous turn"),
        function_call("shell-1"),
        function_call_output("shell-1"),
        text_item("assistant final"),
    ];
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
        scope_node_id: None,
        cut_ordinal: 2,
        fold_end_ordinal: 3,
        spine_tree: "1: finished leaf [worklog already in context]\n2: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "leaf done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let plan = plan_suffix_fold(&history, 2, 3, input).expect("plan suffix fold");

    assert_eq!(plan.cut_index, 1);
    assert_eq!(plan.fold_end_index, 3);
    assert_eq!(plan.input.cut_ordinal, 1);
    assert_eq!(plan.input.fold_end_ordinal, 3);
    assert_eq!(plan.input.suffix_items[0], function_call("shell-1"));
    assert_eq!(plan.input.suffix_items[1], function_call_output("shell-1"));
}

#[test]
fn suffix_fold_pulls_custom_tool_call_back_when_output_is_inside_range() {
    let history = vec![
        user_item("previous turn"),
        custom_tool_call("patch-1"),
        custom_tool_call_output("patch-1"),
        text_item("assistant final"),
    ];
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
        scope_node_id: None,
        cut_ordinal: 2,
        fold_end_ordinal: 3,
        spine_tree: "1: finished leaf [worklog already in context]\n2: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "leaf done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let plan = plan_suffix_fold(&history, 2, 3, input).expect("plan suffix fold");

    assert_eq!(plan.cut_index, 1);
    assert_eq!(plan.fold_end_index, 3);
    assert_eq!(plan.input.cut_ordinal, 1);
    assert_eq!(plan.input.fold_end_ordinal, 3);
    assert_eq!(plan.input.suffix_items[0], custom_tool_call("patch-1"));
    assert_eq!(
        plan.input.suffix_items[1],
        custom_tool_call_output("patch-1")
    );
}

#[test]
fn render_ir_item_embeds_summary_path_and_fold_bounds() {
    let item = render_spine_ir_item(
        &id(&[1, 2]),
        SpineOperation::Close,
        "scope summary",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/2/worklog.md"),
        "scope body",
        8,
        17,
    );
    let text = match &item {
        ResponseItem::Message { content, .. } => match &content[0] {
            ContentItem::OutputText { text } => text.clone(),
            _ => panic!("unexpected content item"),
        },
        _ => panic!("unexpected item type"),
    };

    assert!(text.contains("node=\"1.2\""));
    assert!(text.contains("id=\"spine-ir:1.2:8-17:close\""));
    assert!(text.contains("op=\"close\""));
    assert!(text.contains("fold_start=\"8\""));
    assert!(text.contains("fold_end=\"17\""));
    assert!(text.contains("Base: /tmp/spine"));
    assert!(text.contains("Worklog path: nodes/1/2/worklog.md"));
    assert!(!text.contains("Continue the active user turn"));
    assert!(!text.contains("do not repeat older tool calls"));
    assert!(text.contains("scope body"));
    let ResponseItem::Message { id, .. } = item else {
        panic!("unexpected item type");
    };
    assert_eq!(id.as_deref(), Some("spine-ir:1.2:8-17:close"));
}

#[test]
fn render_worklog_item_omits_runtime_metadata() {
    let item = render_spine_worklog_item(
        &id(&[1, 2]),
        SpineOperation::Close,
        "scope summary",
        "## Compact\n\nscope facts",
    );
    let text = match &item {
        ResponseItem::Message { content, .. } => match &content[0] {
            ContentItem::OutputText { text } => text.clone(),
            _ => panic!("unexpected content item"),
        },
        _ => panic!("unexpected item type"),
    };

    assert!(text.starts_with("<spine_worklog node=\"1.2\" op=\"close\">"));
    assert!(text.contains("Summary: scope summary"));
    assert!(text.contains("scope facts"));
    assert!(text.ends_with("</spine_worklog>"));
    assert!(!text.contains("fold_start"));
    assert!(!text.contains("fold_end"));
    assert!(!text.contains("spine-ir:"));
    assert!(!text.contains("Base:"));
    assert!(!text.contains("Worklog path:"));
    assert!(!text.contains("Node trajs:"));
    assert!(!text.contains("Raw mirror:"));
    assert!(!text.contains("Rollout:"));
    assert!(!text.contains("## Auto Compact"));
    let ResponseItem::Message { id, .. } = item else {
        panic!("unexpected item type");
    };
    assert_eq!(id, None);
}

#[test]
fn codex_builtin_prompt_uses_fork_full_history_shape() {
    let mut state = SpineState::new();
    state.next("leaf done").expect("finish leaf");
    let spine_tree = render_tree(&state, state.cursor());
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
        scope_node_id: None,
        cut_ordinal: 1,
        fold_end_ordinal: 3,
        spine_tree,
        prefix_items: vec![text_item("prefix must stay local")],
        suffix_items: vec![text_item("suffix goes to compactor")],
        transition_summary: "leaf done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let prompt = build_codex_builtin_prompt_input(&input);
    let rendered = format!("{prompt:?}");

    assert_eq!(prompt.len(), 3);
    assert!(rendered.contains("suffix goes to compactor"));
    assert!(rendered.contains("prefix must stay local"));
    assert!(!rendered.contains("quoted_suffix_response_items_json"));
    assert!(!rendered.contains("Target suffix item count"));
    assert!(rendered.contains("<spine_tree>"));
    assert!(rendered.contains("1.1: finished leaf done [worklog already in context]"));
    assert!(rendered.contains("1.2: Current"));
    assert!(rendered.contains("<spine_compact_worklog>"));
    assert!(rendered.contains("</spine_compact_worklog>"));
    assert_eq!(prompt[0], input.prefix_items[0]);
    assert_eq!(prompt[1], input.suffix_items[0]);
    let ResponseItem::Message { content, .. } = &prompt[2] else {
        panic!("expected final compact instruction message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected compact instruction text");
    };
    assert!(!text.contains(crate::compact::SUMMARIZATION_PROMPT));
    assert!(text.starts_with("Compact only target Spine node `1.1` into factual worklog IR."));
    assert!(text.contains("Keep durable facts needed by later nodes"));
    assert!(text.contains("validation status, blockers, unresolved questions"));
    assert!(text.contains("Target tree node: 1.1"));
    assert!(text.contains("Internal node id: 1.1"));
    assert!(text.contains("Target operation: next"));
    assert!(text.contains("Spine Tree summary label: leaf done"));
    assert!(!text.contains("What remains to be done"));
    assert!(!text.contains("clear next steps"));
    assert!(!text.contains("next concrete step"));
    assert!(!text.contains("imperative continuation text"));
    assert!(!text.contains("Pending continuation"));
    assert!(!text.contains("<spine_compact_instruction>"));

    let output = render_auto_compact_worklog(&input, "## Compact\n\nsuffix facts");
    assert!(output.contains("Base: /tmp/spine"));
    assert!(output.contains("Node trajs: nodes/1/1/trajs.jsonl"));
    assert!(output.contains("Raw mirror: /tmp/raw.jsonl"));
    assert!(!output.contains("Compact instruction:"));
}

#[test]
fn codex_builtin_prompt_includes_compact_instruction_when_present() {
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
        scope_node_id: None,
        cut_ordinal: 1,
        fold_end_ordinal: 3,
        spine_tree: "1: finished leaf done [worklog already in context]".to_string(),
        prefix_items: vec![text_item("prefix")],
        suffix_items: vec![text_item("suffix")],
        transition_summary: "leaf done".to_string(),
        compact_instruction: Some("Keep failed command and verification status.".to_string()),
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let prompt = build_codex_builtin_prompt_input(&input);
    let ResponseItem::Message { content, .. } = &prompt[2] else {
        panic!("expected final compact instruction message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected compact instruction text");
    };

    assert!(text.contains("Additional compaction guidance:"));
    assert!(text.contains("Keep failed command and verification status."));
    assert!(!text.contains("<spine_compact_instruction>"));

    let output = render_auto_compact_worklog(&input, "## Compact\n\nsuffix facts");
    assert!(output.contains("Base: /tmp/spine"));
    assert!(!output.contains("Compact instruction:"));
}

#[test]
fn codex_builtin_prompt_reuses_main_request_envelope_without_final_schema() {
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
        scope_node_id: None,
        cut_ordinal: 1,
        fold_end_ordinal: 3,
        spine_tree: "1: finished leaf done [worklog already in context]".to_string(),
        prefix_items: vec![text_item("prefix")],
        suffix_items: vec![text_item("suffix")],
        transition_summary: "leaf done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };
    let tool = ToolSpec::Function(ResponsesApiTool {
        name: "probe".to_string(),
        description: "Probe tool".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::new(),
            /*required*/ None,
            /*additional_properties*/ None,
        ),
        output_schema: None,
    });
    let prompt_envelope = crate::Prompt {
        input: vec![text_item("main request input is replaced")],
        tools: vec![tool.clone()],
        parallel_tool_calls: true,
        base_instructions: BaseInstructions {
            text: "main instructions".to_string(),
        },
        personality: None,
        output_schema: Some(serde_json::json!({"type": "object"})),
        output_schema_strict: false,
    };

    let compact_prompt = build_codex_builtin_prompt(&input, &prompt_envelope);

    assert_eq!(compact_prompt.tools, vec![tool]);
    assert!(compact_prompt.parallel_tool_calls);
    assert_eq!(
        compact_prompt.base_instructions.text,
        prompt_envelope.base_instructions.text
    );
    assert_eq!(compact_prompt.output_schema, None);
    assert!(
        compact_prompt.output_schema_strict,
        "compact response is parsed from a strict XML-like block, not the user final output schema"
    );
    assert_eq!(compact_prompt.input[0], input.prefix_items[0]);
    assert_eq!(compact_prompt.input[1], input.suffix_items[0]);
}

#[test]
fn spine_compact_worklog_extraction_requires_exact_outer_block() {
    assert_eq!(
        extract_spine_compact_worklog(
            "<spine_compact_worklog>\n## Done\n\nfacts\n</spine_compact_worklog>"
        )
        .expect("extract compact worklog"),
        "## Done\n\nfacts"
    );
    assert!(
        extract_spine_compact_worklog("prefix\n<spine_compact_worklog>x</spine_compact_worklog>")
            .is_err()
    );
    assert!(
        extract_spine_compact_worklog("<spine_compact_worklog>x</spine_compact_worklog>\nsuffix")
            .is_err()
    );
    assert!(
        extract_spine_compact_worklog("<spine_compact_worklog> </spine_compact_worklog>").is_err()
    );
}

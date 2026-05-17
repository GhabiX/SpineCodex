use super::*;
use crate::spine::ids::NodeId;
use crate::spine::state::SpineState;
use crate::spine::view::render_tree;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::LocalShellExecAction;
use codex_protocol::models::LocalShellStatus;
use codex_protocol::models::MessagePhase;
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

fn local_shell_call(call_id: Option<&str>) -> ResponseItem {
    ResponseItem::LocalShellCall {
        id: None,
        call_id: call_id.map(str::to_string),
        status: LocalShellStatus::Completed,
        action: LocalShellAction::Exec(LocalShellExecAction {
            command: Vec::new(),
            timeout_ms: None,
            working_directory: None,
            env: None,
            user: None,
        }),
    }
}

fn tool_search_call(call_id: Option<&str>) -> ResponseItem {
    ResponseItem::ToolSearchCall {
        id: None,
        call_id: call_id.map(str::to_string),
        status: None,
        execution: "client".to_string(),
        arguments: serde_json::json!({}),
    }
}

fn tool_search_output(call_id: Option<&str>, execution: &str) -> ResponseItem {
    ResponseItem::ToolSearchOutput {
        call_id: call_id.map(str::to_string),
        status: "completed".to_string(),
        execution: execution.to_string(),
        tools: Vec::new(),
    }
}

#[test]
fn tool_pairing_classifier_covers_response_item_call_shapes() {
    assert_eq!(
        tool_pairing(&function_call("fn-call")),
        ToolPairing::Call("fn-call".to_string())
    );
    assert_eq!(
        tool_pairing(&function_call_output("fn-call")),
        ToolPairing::Output("fn-call".to_string())
    );
    assert_eq!(
        tool_pairing(&custom_tool_call("custom")),
        ToolPairing::Call("custom".to_string())
    );
    assert_eq!(
        tool_pairing(&custom_tool_call_output("custom")),
        ToolPairing::Output("custom".to_string())
    );
    assert_eq!(
        tool_pairing(&local_shell_call(Some("shell"))),
        ToolPairing::Call("shell".to_string())
    );
    assert_eq!(tool_pairing(&local_shell_call(None)), ToolPairing::None);
    assert_eq!(
        tool_pairing(&tool_search_call(Some("search"))),
        ToolPairing::Call("search".to_string())
    );
    assert_eq!(tool_pairing(&tool_search_call(None)), ToolPairing::None);
    assert_eq!(
        tool_pairing(&tool_search_output(Some("search"), "client")),
        ToolPairing::Output("search".to_string())
    );
    assert_eq!(
        tool_pairing(&tool_search_output(Some("server"), "server")),
        ToolPairing::None,
        "server-side search output has no local call item to keep paired"
    );
    assert_eq!(
        tool_pairing(&tool_search_output(None, "client")),
        ToolPairing::None
    );
    assert_eq!(
        tool_pairing(&text_item("ordinary message")),
        ToolPairing::None
    );
}

#[test]
fn effective_mapping_satisfies_formal_item_semantics() {
    let spans = vec![
        installed_span("compact-a", id(&[1]), SpineOperation::Next, 1, 4),
        installed_span("compact-b", id(&[2]), SpineOperation::Next, 5, 8),
    ];
    let history = vec![
        text_item("raw 0"),
        render_spine_memory_item(&id(&[1]), SpineOperation::Next, "a", "a facts"),
        render_spine_handoff_item(&id(&[1]), &id(&[2])),
        text_item("raw 4"),
        render_spine_memory_item(&id(&[2]), SpineOperation::Next, "b", "b facts"),
        render_spine_initial_context_item(vec![ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "prompt hydration".to_string(),
            }],
            phase: None,
        }])
        .expect("wrap initial context"),
        text_item("raw 8"),
    ];

    for raw in [0, 1, 4, 5, 8, 9] {
        let index = effective_index_for_raw_ordinal_with_spans(&history, raw, &spans)
            .unwrap_or_else(|| panic!("raw boundary {raw} should be mappable"));
        assert_eq!(
            raw_ordinal_for_effective_index_with_spans(&history, index, &spans),
            Some(raw),
            "g(f({raw})) must preserve future live boundaries"
        );
    }

    for raw in [2, 3, 6, 7] {
        assert_eq!(
            effective_index_for_raw_ordinal_with_spans(&history, raw, &spans),
            None,
            "span interior raw boundary {raw} must not be mappable"
        );
    }
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 2, &spans),
        Some(4),
        "handoff is Zero-width after first span"
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 6, &spans),
        Some(8),
        "initial context wrapper is Zero-width after second span"
    );
}

#[test]
fn raw_ordinals_map_slim_spine_memory_with_runtime_span() {
    let memory_item = render_spine_memory_item(
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
    let history = vec![text_item("prefix"), memory_item, text_item("tail")];

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
fn raw_ordinals_map_serialized_slim_spine_memory_with_runtime_span() {
    let memory_item = rollout_serialized(render_spine_memory_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "leaf summary",
        "leaf body",
    ));
    let spans = vec![installed_span(
        "compact-1",
        id(&[1, 2]),
        SpineOperation::Next,
        1,
        4,
    )];
    let history = vec![text_item("prefix"), memory_item, text_item("tail")];

    assert!(
        is_spine_ir_item(&history[1]),
        "serialized slim memories need a durable runtime marker because message ids are not serialized"
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
fn raw_ordinals_treat_plain_final_answer_markdown_memory_as_raw1() {
    let previous_memory = render_spine_memory_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "previous leaf",
        "previous facts",
    );
    let plain_final_answer = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "## Spine Memory\n\nNode: 1.2\nOperation: next\nSummary: visible answer\n\nfacts"
                .to_string(),
        }],
        phase: Some(MessagePhase::FinalAnswer),
    };
    let spans = vec![installed_span(
        "compact-1-1",
        id(&[1, 1]),
        SpineOperation::Next,
        1,
        4,
    )];
    let history = vec![
        text_item("prefix"),
        previous_memory,
        render_spine_handoff_item(&id(&[1, 1]), &id(&[1, 2])),
        plain_final_answer,
        text_item("tail"),
    ];

    assert!(
        !is_spine_ir_item(&history[3]),
        "plain final answers must not become synthetic memory items by markdown shape alone"
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 4, &spans),
        Some(3),
        "the plain final answer starts at the post-handoff raw boundary"
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 5, &spans),
        Some(4),
        "the tail boundary proves the markdown final answer consumed Raw1"
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 4, &spans),
        Some(5)
    );
}

#[test]
fn raw_ordinals_treat_unmarked_markdown_memory_without_span_as_raw1() {
    let history = vec![
        text_item("prefix"),
        text_item("## Spine Memory\n\nNode: 1.2\nOperation: next\nSummary: visible\n\nfacts"),
        text_item("tail"),
    ];

    assert!(
        !is_spine_ir_item(&history[1]),
        "bare markdown is not a durable synthetic marker"
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 1, &[]),
        Some(1)
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 2, &[]),
        Some(2),
        "the item must consume Raw1 instead of failing span lookup"
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 2, &[]),
        Some(2)
    );
}

#[test]
fn raw_ordinals_treat_spine_handoff_as_zero_width() {
    let memory_item = render_spine_memory_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "previous leaf",
        "previous facts",
    );
    let handoff_item = render_spine_handoff_item(&id(&[1, 1]), &id(&[1, 2]));
    let spans = vec![installed_span(
        "compact-1-1",
        id(&[1, 1]),
        SpineOperation::Next,
        1,
        4,
    )];
    let history = vec![
        text_item("prefix"),
        memory_item,
        handoff_item,
        text_item("tail"),
    ];

    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 2, &spans),
        Some(4),
        "handoff shares the boundary after the folded memory span"
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 3, &spans),
        Some(4),
        "tail must not be shifted by the zero-width handoff"
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, history.len(), &spans),
        Some(5)
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 4, &spans),
        Some(3),
        "raw boundary should map to the next real item, not the handoff marker"
    );
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 5, &spans),
        Some(history.len())
    );
}

#[test]
fn raw_ordinals_treat_spine_initial_context_wrapper_as_zero_width() {
    let memory_item = render_spine_memory_item(
        &id(&[1]),
        SpineOperation::Archive,
        "root epoch",
        "root facts",
    );
    let initial_context = vec![ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fresh permissions".to_string(),
        }],
        phase: None,
    }];
    let wrapped_context =
        render_spine_initial_context_item(initial_context.clone()).expect("wrap initial context");
    let spans = vec![installed_span(
        "compact-root",
        id(&[1]),
        SpineOperation::Archive,
        2,
        8,
    )];
    let mut prompt_history = vec![
        text_item("prelude 0"),
        text_item("prelude 1"),
        memory_item.clone(),
        wrapped_context,
        user_item("next epoch first live item"),
    ];
    let history = prompt_history.clone();

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 8, &spans),
        Some(4),
        "next root epoch start should map past reinjected context"
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 4, &spans),
        Some(8),
        "next live item must keep the root archive fold_end ordinal"
    );

    expand_spine_initial_context_items(&mut prompt_history);
    assert_eq!(
        prompt_history,
        vec![
            text_item("prelude 0"),
            text_item("prelude 1"),
            memory_item,
            initial_context[0].clone(),
            user_item("next epoch first live item"),
        ],
        "model prompt should receive the original initial context item"
    );
}

#[test]
fn suffix_fold_after_root_archive_reinjected_context_starts_at_next_live_item() {
    let spans = vec![installed_span(
        "compact-root",
        id(&[1]),
        SpineOperation::Archive,
        2,
        8,
    )];
    let wrapped_context = render_spine_initial_context_item(vec![ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fresh permissions".to_string(),
        }],
        phase: None,
    }])
    .expect("wrap initial context");
    let history = vec![
        text_item("prelude 0"),
        text_item("prelude 1"),
        render_spine_memory_item(
            &id(&[1]),
            SpineOperation::Archive,
            "root epoch",
            "root facts",
        ),
        wrapped_context,
        text_item("next epoch live 0"),
        text_item("next epoch live 1"),
        text_item("next epoch live 2"),
        text_item("next epoch live 3"),
        text_item("future epoch first live item"),
    ];
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[2, 1]),
        cut_ordinal: 8,
        fold_end_ordinal: 12,
        spine_tree: "2.1: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "next epoch done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let plan =
        plan_suffix_fold_with_spans(&history, 8, 12, &spans, input).expect("plan suffix fold");

    assert_eq!(plan.cut_index, 4);
    assert_eq!(plan.fold_end_index, 8);
    assert_eq!(
        plan.input.suffix_items,
        vec![
            text_item("next epoch live 0"),
            text_item("next epoch live 1"),
            text_item("next epoch live 2"),
            text_item("next epoch live 3"),
        ]
    );
    assert_eq!(
        plan.replacement_tail,
        vec![text_item("future epoch first live item")]
    );
}

#[test]
fn suffix_fold_does_not_extend_past_handoff_shifted_boundary() {
    let mut history = vec![
        text_item("raw 0"),
        text_item("raw 1"),
        render_spine_memory_item(
            &id(&[1, 1]),
            SpineOperation::Next,
            "node 1.1 done",
            "node 1.1 facts",
        ),
        render_spine_handoff_item(&id(&[1, 1]), &id(&[1, 2])),
    ];
    for raw in 106..215 {
        history.push(text_item(&format!("raw {raw}")));
    }
    history.push(function_call("call-next"));
    history.push(function_call_output("call-next"));

    let spans = vec![installed_span(
        "compact-1-1",
        id(&[1, 1]),
        SpineOperation::Next,
        2,
        106,
    )];
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 2]),
        cut_ordinal: 106,
        fold_end_ordinal: 217,
        spine_tree: "1.1: finished\n1.2: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "node 1.2 done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let plan =
        plan_suffix_fold_with_spans(&history, 106, 217, &spans, input).expect("plan suffix fold");

    assert_eq!(plan.input.cut_ordinal, 106);
    assert_eq!(plan.input.fold_end_ordinal, 217);
    assert_eq!(plan.fold_end_index, history.len());
    assert_eq!(
        plan.input.suffix_items.last(),
        Some(&function_call_output("call-next"))
    );
    assert!(
        plan.replacement_tail.is_empty(),
        "fold end should already include the transition output, so closure must not extend past it"
    );
}

#[test]
fn future_live_start_remains_mappable_after_handoff_compact() {
    let history = vec![
        text_item("raw 0"),
        text_item("raw 1"),
        render_spine_memory_item(
            &id(&[1, 1]),
            SpineOperation::Next,
            "node 1.1 done",
            "node 1.1 facts",
        ),
        render_spine_handoff_item(&id(&[1, 1]), &id(&[1, 2])),
        render_spine_memory_item(
            &id(&[1, 2]),
            SpineOperation::Next,
            "node 1.2 done",
            "node 1.2 facts",
        ),
        render_spine_handoff_item(&id(&[1, 2]), &id(&[1, 3])),
    ];
    let spans = vec![
        installed_span("compact-1-1", id(&[1, 1]), SpineOperation::Next, 2, 106),
        installed_span("compact-1-2", id(&[1, 2]), SpineOperation::Next, 106, 217),
    ];

    let live_start_index = effective_index_for_raw_ordinal_with_spans(&history, 217, &spans)
        .expect("future live start must remain mappable");
    assert_eq!(live_start_index, history.len());
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, live_start_index, &spans),
        Some(217)
    );
}

#[test]
fn raw_ordinals_fail_fast_for_slim_spine_memory_without_runtime_span() {
    let memory_item = render_spine_memory_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "leaf summary",
        "leaf body",
    );
    let history = vec![text_item("prefix"), memory_item, text_item("tail")];

    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&history, 4, &[]),
        None
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&history, 2, &[]),
        None
    );

    let serialized_history = vec![
        text_item("prefix"),
        rollout_serialized(render_spine_memory_item(
            &id(&[1, 2]),
            SpineOperation::Next,
            "leaf summary",
            "leaf body",
        )),
        text_item("tail"),
    ];
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(&serialized_history, 4, &[]),
        None
    );
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(&serialized_history, 2, &[]),
        None
    );
}

#[test]
fn raw_ordinals_ignore_unmatched_later_runtime_spans() {
    let memory_item = render_spine_memory_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "leaf summary",
        "leaf body",
    );
    let spans = vec![
        installed_span("compact-child", id(&[1, 1]), SpineOperation::Next, 1, 4),
        installed_span("rolled-back-scope", id(&[1]), SpineOperation::Close, 1, 6),
    ];
    let history = vec![text_item("prefix"), memory_item, text_item("tail")];

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
        Path::new("nodes/1/2/memory.md"),
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
        Path::new("nodes/1/2/memory.md"),
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
        Path::new("nodes/1/1/memory.md"),
        "legacy body",
        1,
        4,
    );
    let slim =
        render_spine_memory_item(&id(&[1, 2]), SpineOperation::Next, "slim leaf", "slim body");
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
        Path::new("nodes/1/1/memory.md"),
        "trusted body",
        1,
        4,
    ));
    let stale_legacy = rollout_serialized(render_spine_ir_item(
        &id(&[1]),
        SpineOperation::Next,
        "stale root",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/memory.md"),
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
fn runtime_span_mapping_uses_filtered_slim_memory_span_after_redo() {
    let memory_item = render_spine_memory_item(
        &id(&[1, 1]),
        SpineOperation::Next,
        "redo leaf",
        "current compact facts",
    );
    let stale_span = installed_span("compact-old", id(&[1, 1]), SpineOperation::Next, 1, 4);
    let current_span = installed_span("compact-new", id(&[1, 1]), SpineOperation::Next, 1, 6);
    let history = vec![text_item("prefix"), memory_item, text_item("tail")];

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
        Path::new("nodes/1/1/memory.md"),
        "node 1.1 body",
        2,
        293,
    )));
    history.push(rollout_serialized(render_spine_ir_item(
        &id(&[1, 2]),
        SpineOperation::Next,
        "node 1.2 done",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/2/memory.md"),
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
        Path::new("nodes/1/3/memory.md"),
        "node 1.3 body",
        498,
        1108,
    )));

    let stale_ir_text = message_text(&rollout_serialized(render_spine_ir_item(
        &id(&[1]),
        SpineOperation::Next,
        "stale generated ir",
        Path::new("/tmp/spine"),
        Path::new("nodes/1/memory.md"),
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
        Path::new("nodes/1/memory.md"),
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
fn root_archive_replacement_folds_prior_spine_memory_into_archive_span() {
    let prior_memory =
        render_spine_memory_item(&id(&[1]), SpineOperation::Archive, "first epoch", "facts");
    let root_memory =
        render_spine_memory_item(&id(&[2]), SpineOperation::Archive, "second epoch", "facts");
    let wrapped_initial_context =
        render_spine_initial_context_item(vec![text_item("fresh initial context")])
            .expect("wrap initial context");
    let fixed_prelude = text_item("fixed prelude must remain");
    let prefix_history = vec![
        fixed_prelude.clone(),
        prior_memory.clone(),
        text_item("native compact should not keep ordinary assistant prefix"),
        user_item("recent user message kept by native compact"),
        user_item(&format!(
            "{}\nnative compact summary",
            crate::compact::SUMMARY_PREFIX
        )),
    ];
    let live_tail = vec![user_item("live tail after fold")];

    let replacement = build_root_archive_replacement_history(
        &prefix_history,
        vec![wrapped_initial_context.clone()],
        vec![root_memory],
        &live_tail,
        &[installed_span(
            "compact-1",
            id(&[1]),
            SpineOperation::Archive,
            1,
            3,
        )],
    )
    .expect("build root archive replacement");
    let rendered = serde_json::to_string(&replacement.replacement_history)
        .expect("serialize replacement history");

    assert_eq!(replacement.archive_cut_ordinal, 1);
    assert_eq!(replacement.archive_cut_index, 1);
    assert_eq!(replacement.replacement_history.len(), 4);
    assert_eq!(replacement.replacement_history[0], fixed_prelude);
    assert_eq!(replacement.replacement_history[1], wrapped_initial_context);
    assert!(rendered.contains("Node: 2"));
    assert!(!rendered.contains("Node: 1"));
    assert!(rendered.contains("live tail after fold"));
    assert!(rendered.contains("spine_initial_context"));
    assert!(!rendered.contains("native compact summary"));
    assert!(!rendered.contains("recent user message kept by native compact"));
    assert!(!rendered.contains("ordinary assistant prefix"));
}

#[test]
fn root_archive_replacement_must_not_emit_discontinuous_memory_spans() {
    let prefix_history = vec![
        text_item("raw 0"),
        text_item("raw 1"),
        render_spine_memory_item(
            &id(&[1, 1, 1]),
            SpineOperation::Next,
            "first",
            "first facts",
        ),
        text_item("raw gap 10"),
        render_spine_memory_item(
            &id(&[1, 1, 2]),
            SpineOperation::Next,
            "second",
            "second facts",
        ),
        text_item("raw gap 25"),
    ];
    let root_memory =
        render_spine_memory_item(&id(&[1]), SpineOperation::Archive, "root", "root facts");
    let replacement = build_root_archive_replacement_history(
        &prefix_history,
        Vec::new(),
        vec![root_memory],
        &[text_item("future live raw 50")],
        &[
            installed_span("compact-1", id(&[1, 1, 1]), SpineOperation::Next, 2, 10),
            installed_span("compact-2", id(&[1, 1, 2]), SpineOperation::Next, 11, 25),
        ],
    )
    .expect("build root archive replacement");

    assert_eq!(replacement.archive_cut_ordinal, 2);
    assert_eq!(replacement.archive_cut_index, 2);
    let rendered = serde_json::to_string(&replacement.replacement_history)
        .expect("serialize replacement history");
    assert!(rendered.contains("Node: 1"));
    assert!(!rendered.contains("Node: 1.1.1"));
    assert!(!rendered.contains("Node: 1.1.2"));

    let spans_after_install = vec![installed_span(
        "compact-root",
        id(&[1]),
        SpineOperation::Archive,
        2,
        50,
    )];
    validate_spine_replacement_history_admissible(
        &replacement.replacement_history,
        &spans_after_install,
        &[2, 50],
    )
    .expect("root archive replacement should be mappable");
    assert_eq!(
        effective_index_for_raw_ordinal_with_spans(
            &replacement.replacement_history,
            50,
            &spans_after_install,
        ),
        Some(replacement.replacement_history.len() - 1)
    );
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
        cut_ordinal: 3,
        fold_end_ordinal: 8,
        spine_tree: "1: finished leaf [memory already in context]\n2: Current".to_string(),
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
            Path::new("nodes/1/1/memory.md"),
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
            Path::new("root-epochs/previous/memory.md"),
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
            Path::new("root-epochs/compact/memory.md"),
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
fn suffix_fold_uses_runtime_span_for_slim_memory_item() {
    let slim = render_spine_memory_item(
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
        cut_ordinal: 5,
        fold_end_ordinal: 9,
        spine_tree: "1: finished previous [memory already in context]\n2: Current".to_string(),
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
fn close_parent_suffix_fold_can_cover_installed_child_memory_span() {
    let child_memory = render_spine_memory_item(
        &id(&[1, 1, 1, 2]),
        SpineOperation::Close,
        "second child done",
        "child compact facts",
    );
    let child_span = installed_span(
        "compact-child",
        id(&[1, 1, 1, 2]),
        SpineOperation::Close,
        6,
        8,
    );
    let history = vec![
        text_item("raw prelude 0"),
        text_item("raw prelude 1"),
        text_item("raw parent start 2"),
        text_item("raw parent detail 3"),
        text_item("raw first child detail 4"),
        text_item("raw first child detail 5"),
        child_memory.clone(),
        render_spine_handoff_item(&id(&[1, 1, 1, 2]), &id(&[1, 1, 2])),
        text_item("future live raw 8"),
    ];
    let input = SpineCompactInput {
        op: SpineOperation::Close,
        node_id: id(&[1, 1, 1]),
        cut_ordinal: 2,
        fold_end_ordinal: 8,
        spine_tree: "1.1.1: closed scope [memory already in context]\n1.1.2: Current".to_string(),
        prefix_items: Vec::new(),
        suffix_items: Vec::new(),
        transition_summary: "scope done".to_string(),
        compact_instruction: None,
        rollout_path: Path::new("/tmp/rollout.jsonl").to_path_buf(),
        raw_mirror_path: Path::new("/tmp/raw.jsonl").to_path_buf(),
        sidecar_root: Path::new("/tmp/spine").to_path_buf(),
    };

    let plan =
        plan_suffix_fold_with_spans(&history, 2, 8, std::slice::from_ref(&child_span), input)
            .expect("parent close can cover installed child memory");

    assert_eq!(plan.cut_index, 2);
    assert_eq!(plan.fold_end_index, 8);
    assert_eq!(plan.input.cut_ordinal, 2);
    assert_eq!(plan.input.fold_end_ordinal, 8);
    assert_eq!(
        plan.input.prefix_items,
        vec![text_item("raw prelude 0"), text_item("raw prelude 1")]
    );
    assert_eq!(plan.input.suffix_items[4], child_memory);
    assert_eq!(plan.replacement_tail, vec![text_item("future live raw 8")]);

    let parent_memory = render_spine_memory_item(
        &id(&[1, 1, 1]),
        SpineOperation::Close,
        "scope done",
        "parent compact facts",
    );
    let replacement = build_suffix_replacement_history(
        &history,
        plan.cut_index,
        plan.fold_end_index,
        vec![parent_memory],
    );
    assert_eq!(replacement.len(), 4);
    assert_eq!(replacement[0], text_item("raw prelude 0"));
    assert_eq!(replacement[1], text_item("raw prelude 1"));
    assert_eq!(replacement[3], text_item("future live raw 8"));
    assert!(
        !replacement.contains(&child_memory),
        "parent close IR supersedes the child IR in active replacement history after child memory is durable"
    );
    let parent_span = installed_span(
        "compact-parent",
        id(&[1, 1, 1]),
        SpineOperation::Close,
        plan.input.cut_ordinal,
        plan.input.fold_end_ordinal,
    );
    let live_start_index = effective_index_for_raw_ordinal_with_spans(
        &replacement,
        8,
        std::slice::from_ref(&parent_span),
    )
    .expect("future live start must remain mappable after parent close compact");
    assert_eq!(
        raw_ordinal_for_effective_index_with_spans(
            &replacement,
            live_start_index,
            std::slice::from_ref(&parent_span),
        ),
        Some(8)
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
        cut_ordinal: 2,
        fold_end_ordinal: 3,
        spine_tree: "1: finished leaf [memory already in context]\n2: Current".to_string(),
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
        cut_ordinal: 2,
        fold_end_ordinal: 3,
        spine_tree: "1: finished leaf [memory already in context]\n2: Current".to_string(),
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
        Path::new("nodes/1/2/memory.md"),
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
    assert!(text.contains("Memory path: nodes/1/2/memory.md"));
    assert!(!text.contains("Continue the active user turn"));
    assert!(!text.contains("do not repeat older tool calls"));
    assert!(text.contains("scope body"));
    let ResponseItem::Message { id, .. } = item else {
        panic!("unexpected item type");
    };
    assert_eq!(id.as_deref(), Some("spine-ir:1.2:8-17:close"));
}

#[test]
fn render_memory_item_uses_durable_marker_without_span_metadata() {
    let item = render_spine_memory_item(
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

    assert!(text.starts_with("<!-- codex-spine-memory:1.2:close -->\n## Spine Memory\n\n"));
    assert!(text.contains("Node: 1.2"));
    assert!(text.contains("Operation: close"));
    assert!(text.contains("Summary: scope summary"));
    assert!(text.contains("scope facts"));
    assert!(!text.contains("<spine_memory"));
    assert!(!text.contains("</spine_memory>"));
    assert!(!text.contains("fold_start"));
    assert!(!text.contains("fold_end"));
    assert!(!text.contains("spine-ir:"));
    assert!(!text.contains("Base:"));
    assert!(!text.contains("Memory path:"));
    assert!(!text.contains("Node trajs:"));
    assert!(!text.contains("Raw mirror:"));
    assert!(!text.contains("Rollout:"));
    assert!(!text.contains("## Auto Compact"));
    let ResponseItem::Message { id, .. } = item else {
        panic!("unexpected item type");
    };
    assert_eq!(id.as_deref(), Some("spine-memory:1.2:close"));
}

#[test]
fn render_handoff_item_preserves_durable_instructions() {
    let item = render_spine_handoff_item(&id(&[1, 1]), &id(&[1, 2]));
    let ResponseItem::Message { role, content, .. } = &item else {
        panic!("expected message item");
    };
    assert_eq!(role, "developer");
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected input text");
    };

    assert!(text.starts_with("<spine_handoff>"));
    assert!(text.contains("Spine transition completed: 1.1 -> 1.2"));
    assert!(text.contains("use 1.1's generated memory as the active-turn handoff"));
    assert!(text.contains(
        "Spine Memory is internal context; never expose or imitate it in user-visible messages."
    ));
    assert!(
        text.contains("Continue following preserved system, developer, and project instructions")
    );
    assert!(text.contains("raw folded conversation as historical evidence"));
    assert!(text.contains(
        "unresolved user-facing conclusions, decisions, blockers, and next actions captured in the generated memory as current obligations"
    ));
    assert!(text.contains(
        "reconstruct the current node plan from the generated memory, latest user intent, and current evidence"
    ));
    assert!(text.contains(
        "Before asking for new instructions, answer or continue any pending latest user request"
    ));
    assert!(text.ends_with("</spine_handoff>"));
    assert!(!text.contains("Pending continuation"));
    assert!(!text.contains("old user-task content as historical context, not the current request"));
}

#[test]
fn codex_builtin_prompt_uses_fork_full_history_shape() {
    let mut state = SpineState::new();
    state.next("leaf done").expect("finish leaf");
    let spine_tree = render_tree(&state, state.cursor());
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
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
    assert!(rendered.contains("1.1: finished leaf done [memory already in context]"));
    assert!(rendered.contains("1.2: Current"));
    assert!(!rendered.contains("spine_compact_"));
    assert_eq!(prompt[0], input.prefix_items[0]);
    assert_eq!(prompt[1], input.suffix_items[0]);
    let ResponseItem::Message { content, .. } = &prompt[2] else {
        panic!("expected final compact instruction message");
    };
    let ContentItem::InputText { text } = &content[0] else {
        panic!("expected compact instruction text");
    };
    assert!(!text.contains(crate::compact::SUMMARIZATION_PROMPT));
    assert!(
        text.starts_with("Compact only target Spine node `1.1` into a factual Markdown memory.")
    );
    assert!(text.contains("Keep durable facts needed by later nodes"));
    assert!(text.contains("validation status, blockers, unresolved questions"));
    assert!(text.contains("Target tree node: 1.1"));
    assert!(text.contains("Internal node id: 1.1"));
    assert!(text.contains("Target operation: next"));
    assert!(text.contains("Spine Tree summary label: leaf done"));
    assert!(text.contains("Return exactly the compacted suffix as Markdown."));
    assert!(text.contains("Do not wrap it in XML/HTML tags or code fences."));
    assert!(text.contains("any text outside the compacted Markdown body."));
    assert!(!text.contains("<spine_memory"));
    assert!(!text.contains("What remains to be done"));
    assert!(!text.contains("clear next steps"));
    assert!(!text.contains("next concrete step"));
    assert!(!text.contains("imperative continuation text"));
    assert!(!text.contains("Pending continuation"));
    assert!(!text.contains("<spine_compact_instruction>"));

    let output = render_auto_compact_memory(&input, "## Compact\n\nsuffix facts");
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
        cut_ordinal: 1,
        fold_end_ordinal: 3,
        spine_tree: "1: finished leaf done [memory already in context]".to_string(),
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

    let output = render_auto_compact_memory(&input, "## Compact\n\nsuffix facts");
    assert!(output.contains("Base: /tmp/spine"));
    assert!(!output.contains("Compact instruction:"));
}

#[test]
fn codex_builtin_prompt_reuses_main_request_envelope_without_final_schema() {
    let input = SpineCompactInput {
        op: SpineOperation::Next,
        node_id: id(&[1, 1]),
        cut_ordinal: 1,
        fold_end_ordinal: 3,
        spine_tree: "1: finished leaf done [memory already in context]".to_string(),
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
        "compact response is a plain Markdown memory, not the user final output schema"
    );
    assert_eq!(compact_prompt.input[0], input.prefix_items[0]);
    assert_eq!(compact_prompt.input[1], input.suffix_items[0]);
}

#[test]
fn spine_compact_markdown_extraction_accepts_plain_markdown() {
    assert_eq!(
        extract_spine_compact_markdown("\n## Done\n\nfacts\n").expect("extract compact memory"),
        "## Done\n\nfacts"
    );
    assert!(extract_spine_compact_markdown(" \n\t ").is_err());
}

#[test]
fn spine_compact_markdown_extraction_rejects_xml_wrappers() {
    assert!(
        extract_spine_compact_markdown(
            "<spine_memory node=\"1\" op=\"next\">\n## Done\n\nfacts\n</spine_memory>"
        )
        .is_err()
    );
    assert!(extract_spine_compact_markdown("<memory>\n## Done\n\nfacts\n</memory>").is_err());
    assert!(
        extract_spine_compact_markdown("<spine_ir id=\"x\">\n<memory>facts</memory>\n</spine_ir>")
            .is_err()
    );
}

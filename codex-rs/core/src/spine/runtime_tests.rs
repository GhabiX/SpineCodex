use super::*;
use crate::spine::ids::NodeId;
use crate::spine::state::NodeRecord;
use crate::spine::state::NodeStatus;
use crate::spine::store::SpineOperation;
use crate::spine::store::SpineSidecarStore;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn temp_runtime() -> (TempDir, SpineRuntime) {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-2026-05-10T16-00-00-thread.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path).expect("store path");
    let runtime = SpineRuntime::create(store).expect("create runtime");
    (temp, runtime)
}

fn read_json_lines(path: impl AsRef<Path>) -> Vec<Value> {
    let contents = std::fs::read_to_string(path).expect("read jsonl");
    contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse json line"))
        .collect()
}

fn spine_call(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: crate::spine::SPINE_TOOL_OPEN.to_string(),
        namespace: Some(crate::spine::SPINE_NAMESPACE.to_string()),
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

fn spine_close_call(call_id: &str, summary: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: crate::spine::SPINE_TOOL_CLOSE.to_string(),
        namespace: Some(crate::spine::SPINE_NAMESPACE.to_string()),
        arguments: serde_json::json!({ "summary": summary }).to_string(),
        call_id: call_id.to_string(),
    }
}

fn namespaced_spine_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(crate::spine::SPINE_NAMESPACE.to_string()),
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
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

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn response_item(text: &str) -> ResponseItem {
    assistant_message(text)
}

fn initial_tree_event(raw_start_ordinal: u64) -> Value {
    json!({
        "type": "spine_initialized",
        "seq": 1,
        "initial_raw_start_ordinal": raw_start_ordinal,
    })
}

#[test]
fn size_hint_thresholds_start_at_50k_then_step_by_30k() {
    assert_eq!(size_hint_threshold(49_999), None);
    assert_eq!(size_hint_threshold(50_000), Some(50_000));
    assert_eq!(size_hint_threshold(79_999), Some(50_000));
    assert_eq!(size_hint_threshold(80_000), Some(80_000));
    assert_eq!(size_hint_threshold(109_999), Some(80_000));
    assert_eq!(size_hint_threshold(110_000), Some(110_000));
    assert_eq!(size_hint_threshold(139_999), Some(110_000));
    assert_eq!(size_hint_threshold(140_000), Some(140_000));
}

#[test]
fn maybe_emit_size_hint_records_each_threshold_once_per_node() {
    let (_temp, mut runtime) = temp_runtime();
    let payload = "x".repeat(220_000);
    runtime
        .store()
        .append_raw_mirror_items(&[codex_protocol::protocol::RolloutItem::ResponseItem(
            response_item(&payload),
        )])
        .expect("append raw mirror");
    runtime
        .after_response_items_recorded("turn-1", &[response_item(&payload)], 0, 1)
        .expect("record raw item");

    let first = runtime
        .maybe_emit_size_hint("runtime_observation")
        .expect("emit first hint")
        .expect("hint should appear");
    assert_eq!(first.node_id, id(&[1, 1]));
    assert!(first.estimated_tokens >= 50_000);
    assert_eq!(first.threshold_tokens, 50_000);

    assert_eq!(
        runtime
            .maybe_emit_size_hint("runtime_observation")
            .expect("second call should not fail"),
        None
    );

    let larger_payload = "y".repeat(180_000);
    runtime
        .store()
        .append_raw_mirror_items(&[codex_protocol::protocol::RolloutItem::ResponseItem(
            response_item(&larger_payload),
        )])
        .expect("append larger raw mirror");
    runtime
        .after_response_items_recorded("turn-2", &[response_item(&larger_payload)], 1, 2)
        .expect("record larger raw item");

    let second = runtime
        .maybe_emit_size_hint("runtime_observation")
        .expect("emit second threshold")
        .expect("second threshold should appear");
    assert_eq!(second.node_id, id(&[1, 1]));
    assert_eq!(second.threshold_tokens, 80_000);
}

#[test]
fn load_or_create_initializes_then_replays_existing_sidecar() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-2026-05-10T16-00-00-thread.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path).expect("store path");
    let mut runtime =
        SpineRuntime::load_or_create(store.clone(), 0).expect("create missing sidecar");
    runtime
        .after_response_items_recorded(
            "turn-1",
            &[assistant_message("one"), assistant_message("two")],
            0,
            2,
        )
        .expect("record raw items");

    let loaded = SpineRuntime::load_or_create(store, 2).expect("load existing sidecar");

    assert_eq!(loaded.cursor(), &id(&[1, 1]));
    assert_eq!(loaded.current_ordinal(), 2);
    assert_eq!(
        read_json_lines(loaded.store().tree_path()),
        vec![initial_tree_event(0)]
    );
}

#[test]
fn records_raw_item_ranges_for_current_cursor() {
    let (_temp, mut runtime) = temp_runtime();

    let first = runtime
        .after_response_items_recorded(
            "turn-1",
            &[
                assistant_message("one"),
                assistant_message("two"),
                assistant_message("three"),
            ],
            0,
            3,
        )
        .expect("record raw items")
        .pop()
        .expect("non-empty range");

    assert_eq!(
        first,
        RawOrdinalRange {
            node_id: id(&[1, 1]),
            start: 0,
            end: 3,
        }
    );
    assert_eq!(runtime.current_ordinal(), 3);
    assert_eq!(
        read_json_lines(runtime.store().trajs_index_path()),
        vec![json!({
            "type": "raw_items_recorded",
            "seq": 1,
            "node_id": "1.1",
            "turn_id": "turn-1",
            "start": 0,
            "end": 3,
        })]
    );
}

#[test]
fn stage_does_not_advance_cursor_or_write_transition() {
    let (_temp, mut runtime) = temp_runtime();

    let staged = runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage transition")
        .clone();

    assert_eq!(staged.from_node, id(&[1, 1]));
    assert_eq!(staged.to_node, id(&[1, 1, 1]));
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
    assert_eq!(runtime.state().visible_spine(), vec![id(&[1]), id(&[1, 1])]);
    assert!(!runtime.store().memory_path(&id(&[1, 1])).exists());
    assert_eq!(
        read_json_lines(runtime.store().tree_path()),
        vec![initial_tree_event(0)]
    );
}

#[test]
fn commit_moves_cursor_after_function_call_output_boundary() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage transition");

    let ranges = runtime
        .after_response_items_recorded(
            "turn-1",
            &[spine_call("call-1"), function_call_output("call-1")],
            0,
            2,
        )
        .expect("record response items");

    assert_eq!(
        ranges,
        vec![RawOrdinalRange {
            node_id: id(&[1, 1]),
            start: 0,
            end: 2,
        }]
    );
    assert_eq!(
        runtime.cursor(),
        &id(&[1, 1, 1]),
        "cursor moves after the FunctionCallOutput is recorded"
    );
    assert!(runtime.staged_transition().is_none());
    let committed = runtime
        .take_last_committed_transition()
        .expect("transition should be tracked");
    assert_eq!(committed.op, SpineOperation::Open);
    assert_eq!(committed.call_start_ordinal, 0);
    assert_eq!(committed.boundary_end, 2);
    assert_eq!(
        read_json_lines(runtime.store().tree_path()),
        vec![
            initial_tree_event(0),
            json!({
                "type": "transition_applied",
                "seq": 2,
                "op": "open",
                "from_node": "1.1",
                "to_node": "1.1.1",
                "summary": null,
                "raw_start_ordinal": 2,
                "source_turn_id": "turn-1",
            }),
        ]
    );
    assert_eq!(
        read_json_lines(runtime.store().trajs_index_path()),
        vec![
            json!({
                "type": "raw_items_recorded",
                "seq": 1,
                "node_id": "1.1",
                "turn_id": "turn-1",
                "start": 0,
                "end": 2,
            }),
            json!({
                "type": "transition_committed",
                "seq": 2,
                "call_id": "call-1",
                "op": "open",
                "from_node": "1.1",
                "to_node": "1.1.1",
                "call_start_ordinal": 0,
                "boundary_end": 2,
            }),
        ]
    );
}

#[test]
fn stage_after_recorded_call_preserves_function_call_start() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .after_response_items_recorded(
            "turn-1",
            &[assistant_message("before"), spine_call("call-1")],
            0,
            2,
        )
        .expect("record model output before tool dispatch");
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage transition after function call was recorded");
    runtime
        .after_response_items_recorded("turn-1", &[function_call_output("call-1")], 2, 3)
        .expect("record tool output");

    let committed = runtime
        .take_last_committed_transition()
        .expect("transition should be tracked");

    assert_eq!(committed.call_start_ordinal, 1);
    assert_eq!(committed.boundary_end, 3);
    assert_eq!(
        read_json_lines(runtime.store().trajs_index_path()),
        vec![
            json!({
                "type": "raw_items_recorded",
                "seq": 1,
                "node_id": "1.1",
                "turn_id": "turn-1",
                "start": 0,
                "end": 2,
            }),
            json!({
                "type": "raw_items_recorded",
                "seq": 2,
                "node_id": "1.1",
                "turn_id": "turn-1",
                "start": 2,
                "end": 3,
            }),
            json!({
                "type": "transition_committed",
                "seq": 3,
                "call_id": "call-1",
                "op": "open",
                "from_node": "1.1",
                "to_node": "1.1.1",
                "call_start_ordinal": 1,
                "boundary_end": 3,
            }),
        ]
    );
}

#[test]
fn namespaced_transition_call_preserves_function_call_start() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .after_response_items_recorded(
            "turn-1",
            &[
                assistant_message("before"),
                namespaced_spine_call("open", "call-1"),
            ],
            0,
            2,
        )
        .expect("record namespaced model output before tool dispatch");
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage transition after function call was recorded");
    runtime
        .after_response_items_recorded("turn-1", &[function_call_output("call-1")], 2, 3)
        .expect("record tool output");

    let committed = runtime
        .take_last_committed_transition()
        .expect("transition should be tracked");

    assert_eq!(committed.call_start_ordinal, 1);
    assert_eq!(committed.boundary_end, 3);
}

#[test]
fn close_compact_boundary_uses_closed_leaf_raw_start() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "open-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage open");
    runtime
        .after_response_items_recorded(
            "turn-1",
            &[spine_call("open-1"), function_call_output("open-1")],
            0,
            2,
        )
        .expect("commit open");
    runtime.take_last_committed_transition();
    runtime
        .after_response_items_recorded("turn-2", &[assistant_message("leaf work")], 2, 3)
        .expect("record leaf work");
    runtime
        .stage_transition(
            "close-1",
            "turn-2",
            SpineOperation::Close,
            "leaf done",
            Some("preserve test output".to_string()),
        )
        .expect("stage close");
    runtime
        .after_response_items_recorded(
            "turn-2",
            &[
                spine_close_call("close-1", "leaf done"),
                function_call_output("close-1"),
            ],
            3,
            5,
        )
        .expect("commit close");

    let committed = runtime
        .take_last_committed_transition()
        .expect("close transition");
    let boundaries = runtime
        .plan_compaction_after_transition(&committed)
        .expect("compact boundary");
    assert_eq!(boundaries.len(), 1);
    let boundary = &boundaries[0];

    assert_eq!(boundary.op, SpineOperation::Close);
    assert_eq!(boundary.node_id, id(&[1, 1, 1]));
    assert_eq!(boundary.cut_ordinal, 2);
    assert_eq!(boundary.fold_end_ordinal, 5);
    assert_eq!(
        boundary.compact_instruction.as_deref(),
        Some("preserve test output")
    );
}

#[test]
fn non_spine_compact_stop_transition_stage_fails_after_non_spine_compacted_history() {
    let (_temp, mut runtime) = temp_runtime();
    runtime.mark_non_spine_compacted_history();

    for op in [
        SpineOperation::Open,
        SpineOperation::Close,
        SpineOperation::Close,
    ] {
        let error = runtime
            .stage_transition(
                "spine-1", "turn-1", op, "summary", /*compact_instruction*/ None,
            )
            .expect_err("non-spine compacted history should fail fast");
        let SpineRuntimeError::ArchivedReadOnly { reason } = error else {
            panic!("expected ArchivedReadOnly");
        };
        assert_eq!(reason, NON_SPINE_COMPACT_STOP_REASON);
    }
}

#[test]
fn close_compact_fails_after_non_spine_compacted_history() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "close-1",
            "turn-1",
            SpineOperation::Close,
            "root done",
            /*compact_instruction*/ None,
        )
        .expect("stage close");
    runtime
        .after_response_items_recorded(
            "turn-1",
            &[
                spine_close_call("close-1", "root done"),
                function_call_output("close-1"),
            ],
            0,
            2,
        )
        .expect("commit close");
    runtime.mark_non_spine_compacted_history();

    let committed = runtime
        .take_last_committed_transition()
        .expect("close transition");
    let error = runtime
        .plan_compaction_after_transition(&committed)
        .expect_err("non-spine compacted history should fail fast");

    assert!(matches!(error, SpineRuntimeError::ArchivedReadOnly { .. }));
}

#[test]
fn close_root_child_stages_return_to_root_epoch() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .after_response_items_recorded("turn-1", &[assistant_message("root child work")], 0, 1)
        .expect("record child work");

    let staged = runtime
        .stage_transition(
            "close-1",
            "turn-1",
            SpineOperation::Close,
            "scope done",
            /*compact_instruction*/ None,
        )
        .expect("close root child should return to root epoch");

    assert_eq!(staged.from_node, id(&[1, 1]));
    assert_eq!(staged.to_node, id(&[1]));
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
    assert!(runtime.staged_transition().is_some());
}

#[test]
fn close_context_outline_lists_scope_and_direct_children_only() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "open-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage open");
    runtime
        .after_response_items_recorded(
            "turn-1",
            &[spine_call("open-1"), function_call_output("open-1")],
            0,
            2,
        )
        .expect("commit open");
    runtime.take_last_committed_transition();
    runtime
        .stage_transition(
            "open-2",
            "turn-2",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage nested open");
    runtime
        .after_response_items_recorded(
            "turn-2",
            &[spine_call("open-2"), function_call_output("open-2")],
            2,
            4,
        )
        .expect("commit nested open");
    runtime.take_last_committed_transition();
    runtime
        .stage_transition(
            "close-1",
            "turn-3",
            SpineOperation::Close,
            "first child done",
            /*compact_instruction*/ None,
        )
        .expect("stage close");
    runtime
        .after_response_items_recorded(
            "turn-3",
            &[
                spine_close_call("close-1", "first child done"),
                function_call_output("close-1"),
            ],
            4,
            6,
        )
        .expect("commit close");
    runtime.take_last_committed_transition();
    runtime
        .stage_transition(
            "open-second-child",
            "turn-4",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage second child open");
    runtime
        .after_response_items_recorded(
            "turn-4",
            &[
                spine_call("open-second-child"),
                function_call_output("open-second-child"),
            ],
            6,
            8,
        )
        .expect("commit second child open");
    runtime.take_last_committed_transition();
    runtime
        .stage_transition(
            "close-1",
            "turn-5",
            SpineOperation::Close,
            "second child done",
            Some("keep subtree decisions".to_string()),
        )
        .expect("stage close");
    runtime
        .after_response_items_recorded(
            "turn-5",
            &[spine_call("close-1"), function_call_output("close-1")],
            8,
            10,
        )
        .expect("commit close");
    let committed = runtime
        .take_last_committed_transition()
        .expect("close transition");
    let boundaries = runtime
        .plan_compaction_after_transition(&committed)
        .expect("compact boundary");
    assert_eq!(boundaries.len(), 1);
    let child_boundary = &boundaries[0];

    assert_eq!(child_boundary.op, SpineOperation::Close);
    assert_eq!(child_boundary.node_id, id(&[1, 1, 1, 2]));
    assert_eq!(child_boundary.cut_ordinal, 8);
    assert_eq!(child_boundary.fold_end_ordinal, 10);
    assert_eq!(child_boundary.transition_summary, "second child done");
    assert_eq!(
        child_boundary.compact_instruction.as_deref(),
        Some("keep subtree decisions")
    );

    runtime
        .stage_transition(
            "close-scope",
            "turn-6",
            SpineOperation::Close,
            "scope done",
            Some("keep subtree decisions".to_string()),
        )
        .expect("stage scope close");
    runtime
        .after_response_items_recorded(
            "turn-6",
            &[
                spine_call("close-scope"),
                function_call_output("close-scope"),
            ],
            10,
            12,
        )
        .expect("commit scope close");
    let committed = runtime
        .take_last_committed_transition()
        .expect("scope close transition");
    let boundaries = runtime
        .plan_compaction_after_transition(&committed)
        .expect("scope compact boundary");
    assert_eq!(boundaries.len(), 1);
    let scope_boundary = &boundaries[0];
    assert_eq!(scope_boundary.op, SpineOperation::Close);
    assert_eq!(scope_boundary.node_id, id(&[1, 1, 1]));
    assert_eq!(scope_boundary.cut_ordinal, 10);
    assert_eq!(scope_boundary.fold_end_ordinal, 12);
    assert_eq!(scope_boundary.transition_summary, "scope done");
    assert_eq!(
        scope_boundary.compact_instruction.as_deref(),
        Some("keep subtree decisions")
    );

    let outline = runtime
        .render_context_compacted_outline(&id(&[1, 1, 1]))
        .expect("render outline");
    let base = runtime.store().root().display().to_string();

    assert!(outline.contains("## Context Compacted"));
    assert!(outline.contains(&format!("Base: {base}")));
    assert!(outline.contains("[1.1.1] scope done"));
    assert!(outline.contains("|-- [1.1.1.1] first child done"));
    assert!(outline.contains("|-- [1.1.1.2] second child done"));
    assert!(
        outline.find("|-- [1.1.1.1]").expect("first child row")
            < outline.find("|-- [1.1.1.2]").expect("second child row")
    );
    assert!(!outline.contains("memory.md"));

    let model_outline = runtime
        .render_model_context_compacted_outline(&id(&[1, 1, 1]))
        .expect("render model outline");
    assert!(model_outline.contains("## Context Compacted"));
    assert!(model_outline.contains("[1.1.1] scope done"));
    assert!(model_outline.contains("|-- [1.1.1.1] first child done"));
    assert!(model_outline.contains("|-- [1.1.1.2] second child done"));
    assert!(!model_outline.contains("Base:"));
    assert!(!model_outline.contains("memory.md"));
}

#[test]
fn close_context_outline_keeps_numeric_child_order() {
    let state = SpineState::from_records(
        id(&[1, 1]),
        vec![
            NodeRecord {
                node_id: id(&[1, 1]),
                parent_id: Some(id(&[1])),
                raw_start_ordinal: Some(0),
                status: NodeStatus::Live,
                summary: Some("scope".to_string()),
            },
            NodeRecord {
                node_id: id(&[1]),
                parent_id: None,
                raw_start_ordinal: Some(0),
                status: NodeStatus::Suspended,
                summary: Some("root".to_string()),
            },
            NodeRecord {
                node_id: id(&[1, 1, 2]),
                parent_id: Some(id(&[1, 1])),
                raw_start_ordinal: Some(1),
                status: NodeStatus::Closed,
                summary: Some("child two".to_string()),
            },
            NodeRecord {
                node_id: id(&[1, 1, 10]),
                parent_id: Some(id(&[1, 1])),
                raw_start_ordinal: Some(2),
                status: NodeStatus::Closed,
                summary: Some("child ten".to_string()),
            },
        ],
    )
    .expect("construct state");
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-2026-05-10T16-00-00-thread.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path).expect("store path");
    let runtime = SpineRuntime::from_parts(store, state, 3);

    let outline = runtime
        .render_context_compacted_outline(&id(&[1, 1]))
        .expect("render outline");

    assert!(outline.contains("|-- [1.1.2] child two"));
    assert!(outline.contains("|-- [1.1.10] child ten"));
    assert!(
        outline.find("|-- [1.1.2]").expect("child two row")
            < outline.find("|-- [1.1.10]").expect("child ten row")
    );
}

#[test]
fn raw_items_after_commit_are_owned_by_new_cursor() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .after_response_items_recorded("model-call", &[spine_call("call-1")], 0, 1)
        .expect("record model call");
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage transition");
    runtime
        .after_response_items_recorded("spine-output", &[function_call_output("call-1")], 1, 2)
        .expect("record output");
    runtime
        .commit_staged_transition("call-1", 2)
        .expect_err("transition was already committed by the output hook");

    let next = runtime
        .after_response_items_recorded(
            "after-spine",
            &[
                assistant_message("child one"),
                assistant_message("child two"),
            ],
            2,
            4,
        )
        .expect("record new node items")
        .pop()
        .expect("non-empty range");

    assert_eq!(
        next,
        RawOrdinalRange {
            node_id: id(&[1, 1, 1]),
            start: 2,
            end: 4,
        }
    );
    assert_eq!(runtime.current_ordinal(), 4);
}

#[test]
fn items_after_staged_output_in_same_batch_are_owned_by_new_cursor() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage transition");

    let ranges = runtime
        .after_response_items_recorded(
            "turn-1",
            &[
                spine_call("call-1"),
                function_call_output("call-1"),
                assistant_message("now working in child"),
            ],
            0,
            3,
        )
        .expect("record response items");

    assert_eq!(
        ranges,
        vec![
            RawOrdinalRange {
                node_id: id(&[1, 1]),
                start: 0,
                end: 2,
            },
            RawOrdinalRange {
                node_id: id(&[1, 1, 1]),
                start: 2,
                end: 3,
            },
        ]
    );
    assert_eq!(runtime.cursor(), &id(&[1, 1, 1]));
    assert_eq!(runtime.current_ordinal(), 3);
    assert_eq!(runtime.raw_start_ordinal(&id(&[1, 1, 1])), Some(2));
    assert_eq!(
        read_json_lines(runtime.store().trajs_index_path()),
        vec![
            json!({
                "type": "raw_items_recorded",
                "seq": 1,
                "node_id": "1.1",
                "turn_id": "turn-1",
                "start": 0,
                "end": 2,
            }),
            json!({
                "type": "transition_committed",
                "seq": 2,
                "call_id": "call-1",
                "op": "open",
                "from_node": "1.1",
                "to_node": "1.1.1",
                "call_start_ordinal": 0,
                "boundary_end": 2,
            }),
            json!({
                "type": "raw_items_recorded",
                "seq": 3,
                "node_id": "1.1.1",
                "turn_id": "turn-1",
                "start": 2,
                "end": 3,
            }),
        ]
    );
}

#[test]
fn rejects_second_staged_transition() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage first transition");

    let error = runtime
        .stage_transition(
            "call-2",
            "turn-1",
            SpineOperation::Close,
            "another",
            /*compact_instruction*/ None,
        )
        .expect_err("second staged transition should fail");

    assert!(matches!(
        error,
        SpineRuntimeError::TransitionAlreadyStaged { call_id } if call_id == "call-1"
    ));
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
}

#[test]
fn commit_requires_matching_call_id() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage transition");

    let error = runtime
        .commit_staged_transition("call-2", 0)
        .expect_err("wrong call id should fail");

    assert!(matches!(
        error,
        SpineRuntimeError::StagedCallIdMismatch { expected, actual }
            if expected == "call-1" && actual == "call-2"
    ));
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
    assert!(runtime.staged_transition().is_some());
}

#[test]
fn commit_requires_recorded_function_call_start() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Close,
            "root done",
            /*compact_instruction*/ None,
        )
        .expect("stage transition without recorded call");

    let error = runtime
        .commit_staged_transition("call-1", 0)
        .expect_err("missing call start should fail fast");

    assert!(matches!(
        error,
        SpineRuntimeError::MissingCallStartOrdinal { call_id } if call_id == "call-1"
    ));
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
    assert!(runtime.staged_transition().is_some());
}

#[test]
fn commit_failure_leaves_cursor_and_tree_unchanged() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "__spine_fail_transition_commit__",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage transition");

    let error = runtime
        .after_response_items_recorded(
            "turn-1",
            &[
                spine_call("__spine_fail_transition_commit__"),
                function_call_output("__spine_fail_transition_commit__"),
            ],
            0,
            2,
        )
        .expect_err("injected commit failure should abort transition");

    assert!(matches!(
        error,
        SpineRuntimeError::Store(crate::spine::store::SpineStoreError::InvalidLedger(message))
            if message == "injected transition commit failure"
    ));
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
    assert_eq!(runtime.current_ordinal(), 2);
    assert!(runtime.staged_transition().is_some());
    assert!(!runtime.store().memory_path(&id(&[1])).exists());
    assert_eq!(
        read_json_lines(runtime.store().tree_path()),
        vec![initial_tree_event(0)]
    );
    assert_eq!(
        read_json_lines(runtime.store().trajs_index_path()),
        vec![json!({
            "type": "raw_items_recorded",
            "seq": 1,
            "node_id": "1.1",
            "turn_id": "turn-1",
            "start": 0,
            "end": 2,
        })]
    );
}

#[test]
fn stage_uses_state_validation_without_mutating_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-2026-05-10T16-00-00-thread.jsonl");
    let store = SpineSidecarStore::create_for_rollout(&rollout_path).expect("store path");
    let state = SpineState::from_records(
        id(&[1]),
        vec![NodeRecord {
            node_id: id(&[1]),
            parent_id: None,
            raw_start_ordinal: Some(0),
            status: NodeStatus::Live,
            summary: None,
        }],
    )
    .expect("construct root cursor state");
    let mut runtime = SpineRuntime::from_parts(store, state, 0);

    let error = runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Close,
            "root done",
            /*compact_instruction*/ None,
        )
        .expect_err("close on root should fail");

    assert!(matches!(
        error,
        SpineRuntimeError::State(SpineStateError::CannotCloseRoot)
    ));
    assert_eq!(runtime.cursor(), &id(&[1]));
    assert!(runtime.staged_transition().is_none());
}

#[test]
fn root_epoch_archive_plans_internal_archive_boundary() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .after_prelude_items_recorded("turn-prelude", &[assistant_message("prelude")], 0, 1)
        .expect("record prelude");
    runtime
        .stage_transition(
            "open-1",
            "turn-1",
            SpineOperation::Open,
            None,
            /*compact_instruction*/ None,
        )
        .expect("stage open");
    runtime
        .after_response_items_recorded(
            "turn-1",
            &[spine_call("open-1"), function_call_output("open-1")],
            1,
            3,
        )
        .expect("commit open");
    runtime
        .after_response_items_recorded("turn-2", &[assistant_message("child work")], 3, 4)
        .expect("record child work");

    let boundary = runtime
        .plan_root_epoch_archive()
        .expect("plan root archive");

    assert_eq!(boundary.op, SpineOperation::Archive);
    assert_eq!(boundary.node_id, id(&[1]));
    assert_eq!(boundary.cut_ordinal, 1);
    assert_eq!(boundary.fold_end_ordinal, 4);
    assert_eq!(boundary.transition_summary, "Context compacted");

    runtime
        .record_root_epoch_archive(
            boundary.transition_summary,
            boundary.fold_end_ordinal,
            "compact-1",
            "turn-compact",
        )
        .expect("record archive");

    assert_eq!(runtime.cursor(), &id(&[2, 1]));
    assert_eq!(
        runtime
            .state()
            .node(&id(&[1, 1]))
            .map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        runtime
            .state()
            .node(&id(&[1, 1, 1]))
            .map(|node| node.status.clone()),
        Some(NodeStatus::Closed)
    );
    assert_eq!(
        runtime
            .state()
            .nodes()
            .values()
            .filter(|node| node.status == NodeStatus::Live)
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>(),
        vec![id(&[2, 1])]
    );
}

#[test]
fn root_epoch_archive_plans_materialized_epoch_when_cursor_is_hidden_root() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .after_prelude_items_recorded("turn-prelude", &[assistant_message("prelude")], 0, 1)
        .expect("record prelude");
    runtime
        .after_response_items_recorded("turn-1", &[assistant_message("root work")], 1, 2)
        .expect("record root work");

    let boundary = runtime
        .plan_root_epoch_archive()
        .expect("plan root archive");

    assert_eq!(boundary.op, SpineOperation::Archive);
    assert_eq!(boundary.node_id, id(&[1]));
    assert_eq!(boundary.cut_ordinal, 1);
    assert_eq!(boundary.fold_end_ordinal, 2);

    runtime
        .record_root_epoch_archive(
            boundary.transition_summary,
            boundary.fold_end_ordinal,
            "compact-root",
            "turn-compact",
        )
        .expect("record archive");

    assert_eq!(runtime.cursor(), &id(&[2, 1]));
    assert_eq!(
        runtime
            .state()
            .node(&id(&[2, 1]))
            .and_then(|node| node.parent_id.clone()),
        Some(id(&[2]))
    );
}

#[test]
fn zero_raw_items_are_noop() {
    let (_temp, mut runtime) = temp_runtime();

    let ranges = runtime
        .after_response_items_recorded("empty", &[], 0, 0)
        .expect("zero item record should be a no-op");

    assert_eq!(ranges, Vec::<RawOrdinalRange>::new());
    assert_eq!(runtime.current_ordinal(), 0);
    assert_eq!(
        std::fs::read_to_string(runtime.store().trajs_index_path()).expect("read trajs index"),
        ""
    );
}

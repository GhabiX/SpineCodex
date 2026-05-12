use super::*;
use crate::spine::ids::NodeId;
use crate::spine::store::SpineOperation;
use crate::spine::store::SpineSidecarStore;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::plan_tool::UpdatePlanArgs;
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
    let store = SpineSidecarStore::for_rollout(&rollout_path).expect("store path");
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

fn read_json(path: impl AsRef<Path>) -> Value {
    let contents = std::fs::read_to_string(path).expect("read json");
    serde_json::from_str(&contents).expect("parse json")
}

fn plan_args(step: &str, status: StepStatus) -> UpdatePlanArgs {
    plan_args_many(&[(step, status)])
}

fn plan_args_many(items: &[(&str, StepStatus)]) -> UpdatePlanArgs {
    UpdatePlanArgs {
        explanation: Some("PlanBridge test".to_string()),
        plan: items
            .iter()
            .map(|(step, status)| PlanItemArg {
                step: (*step).to_string(),
                status: status.clone(),
            })
            .collect(),
    }
}

fn spine_call(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: "spine".to_string(),
        namespace: None,
        arguments: r#"{"op":"open","summary":"root scope"}"#.to_string(),
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

#[test]
fn record_plan_update_writes_active_node_snapshot_without_moving_cursor() {
    let (_temp, mut runtime) = temp_runtime();
    let initial_state = runtime.state().clone();

    let snapshot = runtime
        .record_plan_update("turn-1", plan_args("Inspect root", StepStatus::InProgress))
        .expect("record plan update");

    assert_eq!(runtime.state(), &initial_state);
    assert_eq!(snapshot.node_id, "1");
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.source_turn_id, "turn-1");
    assert_eq!(snapshot.event_seq, 2);
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].stable_task_id, "step-1");
    assert_eq!(snapshot.items[0].step, "Inspect root");
    assert_eq!(snapshot.items[0].status, "in_progress");

    let plan = read_json(runtime.store().plan_path(&id(&[1])));
    assert_eq!(plan["node_id"], "1");
    assert_eq!(plan["revision"], 1);
    assert_eq!(plan["event_seq"], 2);
    assert_eq!(plan["source_turn_id"], "turn-1");
    assert_eq!(plan["items"][0]["stable_task_id"], "step-1");
    assert_eq!(plan["items"][0]["status"], "in_progress");
    let tree = read_json_lines(runtime.store().tree_path());
    assert_eq!(tree[1]["type"], "task_plan_updated");
    assert_eq!(tree[1]["seq"], 2);
    assert_eq!(tree[1]["node_id"], "1");
    assert_eq!(tree[1]["revision"], 1);
    assert_eq!(tree[1]["items"][0]["stable_task_id"], "step-1");
    assert_eq!(tree[1]["items"][0]["step"], "Inspect root");

    let second = runtime
        .record_plan_update("turn-2", plan_args("Inspect root", StepStatus::Completed))
        .expect("record second plan update");
    assert_eq!(second.revision, 2);
    assert_eq!(second.event_seq, 3);
    assert_eq!(second.items[0].stable_task_id, "step-1");
    assert_eq!(runtime.state(), &initial_state);
}

#[test]
fn record_plan_update_reuses_task_ids_after_insert_and_reorder() {
    let (_temp, mut runtime) = temp_runtime();

    let first = runtime
        .record_plan_update(
            "turn-1",
            plan_args_many(&[
                ("Inspect root", StepStatus::Pending),
                ("Verify root", StepStatus::InProgress),
            ]),
        )
        .expect("record first plan update");
    assert_eq!(first.items[0].stable_task_id, "step-1");
    assert_eq!(first.items[1].stable_task_id, "step-2");

    let second = runtime
        .record_plan_update(
            "turn-2",
            plan_args_many(&[
                ("Verify root", StepStatus::InProgress),
                ("Document root", StepStatus::Pending),
                ("Inspect root", StepStatus::Completed),
            ]),
        )
        .expect("record second plan update");

    assert_eq!(second.revision, 2);
    assert_eq!(second.event_seq, 3);
    assert_eq!(second.items[0].stable_task_id, "step-2");
    assert_eq!(second.items[1].stable_task_id, "step-3");
    assert_eq!(second.items[2].stable_task_id, "step-1");
    assert_eq!(runtime.cursor(), &id(&[1]));
}

#[test]
fn build_tree_snapshot_includes_node_local_plans() {
    let (_temp, mut runtime) = temp_runtime();

    runtime
        .record_plan_update("turn-1", plan_args("Inspect root", StepStatus::InProgress))
        .expect("record root plan");
    let snapshot = runtime.build_tree_snapshot().expect("build snapshot");

    assert_eq!(snapshot.snapshot_seq, 2);
    assert_eq!(snapshot.active_node_id, "1");
    assert_eq!(snapshot.nodes.len(), 1);
    let root = &snapshot.nodes[0];
    assert_eq!(root.node_id, "1");
    assert_eq!(root.parent_id, None);
    assert_eq!(root.summary, None);
    assert_eq!(root.status, SpineTreeNodeStatus::Live);
    let plan = root.plan.as_ref().expect("root plan");
    assert_eq!(plan.revision, 1);
    assert_eq!(plan.items[0].stable_task_id, "step-1");
    assert_eq!(plan.items[0].step, "Inspect root");
    assert_eq!(plan.items[0].status, SpineTreePlanItemStatus::InProgress);
}

#[test]
fn build_tree_snapshot_includes_only_current_node_plan() {
    let (_temp, mut runtime) = temp_runtime();

    runtime
        .record_plan_update(
            "turn-root",
            plan_args("Inspect root", StepStatus::InProgress),
        )
        .expect("record root plan");
    runtime
        .stage_transition(
            "open-1",
            "turn-open",
            SpineOperation::Open,
            "root scope",
            /*compact_instruction*/ None,
        )
        .expect("stage open");
    runtime
        .after_response_items_recorded(
            "turn-open",
            &[spine_call("open-1"), function_call_output("open-1")],
            0,
            2,
        )
        .expect("commit open");
    runtime.take_last_committed_transition();

    runtime
        .record_plan_update(
            "turn-child",
            plan_args("Inspect child", StepStatus::InProgress),
        )
        .expect("record child plan");
    runtime
        .stage_transition(
            "next-1",
            "turn-next",
            SpineOperation::Next,
            "child done",
            /*compact_instruction*/ None,
        )
        .expect("stage next");
    runtime
        .after_response_items_recorded(
            "turn-next",
            &[spine_call("next-1"), function_call_output("next-1")],
            2,
            4,
        )
        .expect("commit next");
    runtime.take_last_committed_transition();

    runtime
        .record_plan_update(
            "turn-sibling",
            plan_args("Inspect sibling", StepStatus::InProgress),
        )
        .expect("record sibling plan");
    let snapshot = runtime.build_tree_snapshot().expect("build snapshot");

    assert_eq!(snapshot.active_node_id, "1.2");
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1")
        .expect("root node");
    let child = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1")
        .expect("child node");
    let sibling = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.2")
        .expect("sibling node");

    assert_eq!(root.status, SpineTreeNodeStatus::Opened);
    assert_eq!(root.plan, None);
    assert_eq!(child.status, SpineTreeNodeStatus::Finished);
    assert_eq!(child.plan, None);
    assert_eq!(sibling.status, SpineTreeNodeStatus::Live);
    let plan = sibling.plan.as_ref().expect("current sibling plan");
    assert_eq!(plan.revision, 1);
    assert_eq!(plan.items[0].step, "Inspect sibling");
    assert_eq!(plan.items[0].status, SpineTreePlanItemStatus::InProgress);
}

#[test]
fn load_or_create_initializes_then_replays_existing_sidecar() {
    let temp = tempfile::tempdir().expect("tempdir");
    let rollout_path = temp.path().join("rollout-2026-05-10T16-00-00-thread.jsonl");
    let store = SpineSidecarStore::for_rollout(&rollout_path).expect("store path");
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

    assert_eq!(loaded.cursor(), &id(&[1]));
    assert_eq!(loaded.current_ordinal(), 2);
    assert_eq!(
        read_json_lines(loaded.store().tree_path()),
        vec![json!({
            "type": "node_created",
            "seq": 1,
            "node_id": "1",
            "parent_id": null,
            "raw_start_ordinal": 0,
        })]
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
            node_id: id(&[1]),
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
            "node_id": "1",
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
            "root scope",
            /*compact_instruction*/ None,
        )
        .expect("stage transition")
        .clone();

    assert_eq!(staged.from_node, id(&[1]));
    assert_eq!(staged.to_node, id(&[1, 1]));
    assert_eq!(runtime.cursor(), &id(&[1]));
    assert_eq!(runtime.state().visible_spine(), vec![id(&[1])]);
    assert!(!runtime.store().worklog_path(&id(&[1])).exists());
    assert_eq!(
        read_json_lines(runtime.store().tree_path()),
        vec![json!({
            "type": "node_created",
            "seq": 1,
            "node_id": "1",
            "parent_id": null,
            "raw_start_ordinal": 0,
        })]
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
            "root scope",
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
            node_id: id(&[1]),
            start: 0,
            end: 2,
        }]
    );
    assert_eq!(
        runtime.cursor(),
        &id(&[1, 1]),
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
            json!({
                "type": "node_created",
                "seq": 1,
                "node_id": "1",
                "parent_id": null,
                "raw_start_ordinal": 0,
            }),
            json!({
                "type": "transition_applied",
                "seq": 2,
                "op": "open",
                "from_node": "1",
                "to_node": "1.1",
                "to_parent_id": "1",
                "summary": "root scope",
                "raw_start_ordinal": 2,
            }),
        ]
    );
    assert_eq!(
        read_json_lines(runtime.store().trajs_index_path()),
        vec![
            json!({
                "type": "raw_items_recorded",
                "seq": 1,
                "node_id": "1",
                "turn_id": "turn-1",
                "start": 0,
                "end": 2,
            }),
            json!({
                "type": "transition_committed",
                "seq": 2,
                "call_id": "call-1",
                "op": "open",
                "from_node": "1",
                "to_node": "1.1",
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
            "root scope",
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
                "node_id": "1",
                "turn_id": "turn-1",
                "start": 0,
                "end": 2,
            }),
            json!({
                "type": "raw_items_recorded",
                "seq": 2,
                "node_id": "1",
                "turn_id": "turn-1",
                "start": 2,
                "end": 3,
            }),
            json!({
                "type": "transition_committed",
                "seq": 3,
                "call_id": "call-1",
                "op": "open",
                "from_node": "1",
                "to_node": "1.1",
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
            "root scope",
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
fn next_compact_boundary_uses_finished_leaf_raw_start() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "open-1",
            "turn-1",
            SpineOperation::Open,
            "root scope",
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
            "next-1",
            "turn-2",
            SpineOperation::Next,
            "leaf done",
            Some("preserve test output".to_string()),
        )
        .expect("stage next");
    runtime
        .after_response_items_recorded(
            "turn-2",
            &[spine_call("next-1"), function_call_output("next-1")],
            3,
            5,
        )
        .expect("commit next");

    let committed = runtime
        .take_last_committed_transition()
        .expect("next transition");
    let boundary = runtime
        .plan_compaction_after_transition(&committed)
        .expect("compact boundary")
        .expect("next should compact");

    assert_eq!(boundary.op, SpineOperation::Next);
    assert_eq!(boundary.node_id, id(&[1, 1]));
    assert_eq!(boundary.cut_ordinal, 2);
    assert_eq!(boundary.fold_end_ordinal, 5);
    assert_eq!(
        boundary.compact_instruction.as_deref(),
        Some("preserve test output")
    );
}

#[test]
fn transition_stage_fails_after_non_spine_compacted_history() {
    let (_temp, mut runtime) = temp_runtime();
    runtime.mark_non_spine_compacted_history();

    for op in [
        SpineOperation::Open,
        SpineOperation::Next,
        SpineOperation::Close,
    ] {
        let error = runtime
            .stage_transition(
                "spine-1", "turn-1", op, "summary", /*compact_instruction*/ None,
            )
            .expect_err("non-spine compacted history should fail fast");
        assert!(matches!(error, SpineRuntimeError::ArchivedReadOnly { .. }));
    }
}

#[test]
fn next_compact_fails_after_non_spine_compacted_history() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "next-1",
            "turn-1",
            SpineOperation::Next,
            "root done",
            /*compact_instruction*/ None,
        )
        .expect("stage next");
    runtime
        .after_response_items_recorded(
            "turn-1",
            &[spine_call("next-1"), function_call_output("next-1")],
            0,
            2,
        )
        .expect("commit next");
    runtime.mark_non_spine_compacted_history();

    let committed = runtime
        .take_last_committed_transition()
        .expect("next transition");
    let error = runtime
        .plan_compaction_after_transition(&committed)
        .expect_err("non-spine compacted history should fail fast");

    assert!(matches!(error, SpineRuntimeError::ArchivedReadOnly { .. }));
}

#[test]
fn close_that_would_close_root_scope_is_rejected() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "open-1",
            "turn-1",
            SpineOperation::Open,
            "root scope",
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
        .after_response_items_recorded("turn-2", &[assistant_message("child work")], 2, 3)
        .expect("record child work");

    let error = runtime
        .stage_transition(
            "close-1",
            "turn-2",
            SpineOperation::Close,
            "scope done",
            /*compact_instruction*/ None,
        )
        .expect_err("close should reject root scope");

    assert!(matches!(
        error,
        SpineRuntimeError::State(SpineStateError::CannotCloseRoot)
    ));
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
    assert!(runtime.staged_transition().is_none());
}

#[test]
fn close_context_outline_lists_scope_and_direct_children_only() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "open-1",
            "turn-1",
            SpineOperation::Open,
            "root scope",
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
            "child scope",
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
            "next-1",
            "turn-3",
            SpineOperation::Next,
            "first child done",
            /*compact_instruction*/ None,
        )
        .expect("stage next");
    runtime
        .after_response_items_recorded(
            "turn-3",
            &[spine_call("next-1"), function_call_output("next-1")],
            4,
            6,
        )
        .expect("commit next");
    runtime.take_last_committed_transition();
    runtime
        .stage_transition(
            "close-1",
            "turn-4",
            SpineOperation::Close,
            "second child done",
            Some("keep subtree decisions".to_string()),
        )
        .expect("stage close");
    runtime
        .after_response_items_recorded(
            "turn-4",
            &[spine_call("close-1"), function_call_output("close-1")],
            6,
            8,
        )
        .expect("commit close");
    let committed = runtime
        .take_last_committed_transition()
        .expect("close transition");
    let boundary = runtime
        .plan_compaction_after_transition(&committed)
        .expect("compact boundary")
        .expect("close should compact");

    assert_eq!(boundary.op, SpineOperation::Close);
    assert_eq!(boundary.node_id, id(&[1, 1]));
    assert_eq!(boundary.transition_summary, "child scope");
    assert_eq!(
        boundary.compact_instruction.as_deref(),
        Some("keep subtree decisions")
    );

    let outline = runtime
        .render_context_compacted_outline(&id(&[1, 1]))
        .expect("render outline");

    assert!(outline.contains("## Context Compacted"));
    assert!(outline.contains("[1.1] child scope (nodes/1/1/worklog.md)"));
    assert!(outline.contains("|-- [1.1.1] first child done (nodes/1/1/1/worklog.md)"));
    assert!(outline.contains("|-- [1.1.2] second child done (nodes/1/1/2/worklog.md)"));
    assert!(
        outline.find("|-- [1.1.1]").expect("first child row")
            < outline.find("|-- [1.1.2]").expect("second child row")
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
            "root scope",
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
            node_id: id(&[1, 1]),
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
            "root scope",
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
                node_id: id(&[1]),
                start: 0,
                end: 2,
            },
            RawOrdinalRange {
                node_id: id(&[1, 1]),
                start: 2,
                end: 3,
            },
        ]
    );
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
    assert_eq!(runtime.current_ordinal(), 3);
    assert_eq!(runtime.raw_start_ordinal(&id(&[1, 1])), Some(2));
    assert_eq!(
        read_json_lines(runtime.store().trajs_index_path()),
        vec![
            json!({
                "type": "raw_items_recorded",
                "seq": 1,
                "node_id": "1",
                "turn_id": "turn-1",
                "start": 0,
                "end": 2,
            }),
            json!({
                "type": "transition_committed",
                "seq": 2,
                "call_id": "call-1",
                "op": "open",
                "from_node": "1",
                "to_node": "1.1",
                "call_start_ordinal": 0,
                "boundary_end": 2,
            }),
            json!({
                "type": "raw_items_recorded",
                "seq": 3,
                "node_id": "1.1",
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
            "root scope",
            /*compact_instruction*/ None,
        )
        .expect("stage first transition");

    let error = runtime
        .stage_transition(
            "call-2",
            "turn-1",
            SpineOperation::Next,
            "another",
            /*compact_instruction*/ None,
        )
        .expect_err("second staged transition should fail");

    assert!(matches!(
        error,
        SpineRuntimeError::TransitionAlreadyStaged { call_id } if call_id == "call-1"
    ));
    assert_eq!(runtime.cursor(), &id(&[1]));
}

#[test]
fn commit_requires_matching_call_id() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Open,
            "root scope",
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
    assert_eq!(runtime.cursor(), &id(&[1]));
    assert!(runtime.staged_transition().is_some());
}

#[test]
fn commit_requires_recorded_function_call_start() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "call-1",
            "turn-1",
            SpineOperation::Next,
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
    assert_eq!(runtime.cursor(), &id(&[1]));
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
            "root scope",
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
    assert_eq!(runtime.cursor(), &id(&[1]));
    assert_eq!(runtime.current_ordinal(), 2);
    assert!(runtime.staged_transition().is_some());
    assert!(!runtime.store().worklog_path(&id(&[1])).exists());
    assert_eq!(
        read_json_lines(runtime.store().tree_path()),
        vec![json!({
            "type": "node_created",
            "seq": 1,
            "node_id": "1",
            "parent_id": null,
            "raw_start_ordinal": 0,
        })]
    );
    assert_eq!(
        read_json_lines(runtime.store().trajs_index_path()),
        vec![json!({
            "type": "raw_items_recorded",
            "seq": 1,
            "node_id": "1",
            "turn_id": "turn-1",
            "start": 0,
            "end": 2,
        })]
    );
}

#[test]
fn stage_uses_state_validation_without_mutating_runtime() {
    let (_temp, mut runtime) = temp_runtime();

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

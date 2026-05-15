use super::*;
use crate::spine::ids::NodeId;
use crate::spine::store::SpineOperation;
use crate::spine::store::SpineSidecarStore;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::SpinePlanTreeArg;
use codex_protocol::plan_tool::SpinePlanTreeCheckpointArg;
use codex_protocol::plan_tool::SpinePlanTreeScopeArg;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::plan_tool::UpdatePlanArgs;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashSet;
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
        spine_plantree: None,
        clear_spine_plantree: false,
    }
}

fn plan_args_with_plantree(
    anchor: Option<&str>,
    children: Vec<(Option<&str>, &str, Vec<&str>)>,
) -> UpdatePlanArgs {
    UpdatePlanArgs {
        explanation: Some("PlanBridge PlanTree test".to_string()),
        plan: vec![PlanItemArg {
            step: "Plan scope tree".to_string(),
            status: StepStatus::InProgress,
        }],
        spine_plantree: Some(SpinePlanTreeArg {
            anchor: anchor.map(str::to_string),
            root: SpinePlanTreeScopeArg {
                node: anchor.map(str::to_string),
                summary: "Editable task scope".to_string(),
                status: Some(StepStatus::InProgress),
                checkpoints: Vec::new(),
                children: children
                    .into_iter()
                    .map(|(node, summary, checkpoints)| SpinePlanTreeScopeArg {
                        node: node.map(str::to_string),
                        summary: summary.to_string(),
                        status: Some(StepStatus::Pending),
                        checkpoints: checkpoints
                            .into_iter()
                            .map(|task| SpinePlanTreeCheckpointArg {
                                task: task.to_string(),
                                status: StepStatus::Pending,
                            })
                            .collect(),
                        children: Vec::new(),
                    })
                    .collect(),
            },
        }),
        clear_spine_plantree: false,
    }
}

fn spine_call(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: "spine".to_string(),
        namespace: None,
        arguments: r#"{"op":"open"}"#.to_string(),
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
        "state": {
            "cursor": "1.1",
            "nodes": [
                {
                    "node_id": "1",
                    "parent_id": null,
                    "raw_start_ordinal": 0,
                    "status": "opened",
                    "summary": null,
                    "worklog_path": "nodes/1/worklog.md",
                    "plan_path": "nodes/1/plan.json",
                },
                {
                    "node_id": "1.1",
                    "parent_id": "1",
                    "raw_start_ordinal": raw_start_ordinal,
                    "status": "live",
                    "summary": null,
                    "worklog_path": "nodes/1/1/worklog.md",
                    "plan_path": "nodes/1/1/plan.json",
                },
            ],
        },
    })
}

#[test]
fn record_plan_update_writes_active_node_snapshot_without_moving_cursor() {
    let (_temp, mut runtime) = temp_runtime();
    let initial_state = runtime.state().clone();

    let snapshot = runtime
        .record_plan_update("turn-1", plan_args("Inspect root", StepStatus::InProgress))
        .expect("record plan update");

    assert_eq!(runtime.state(), &initial_state);
    assert_eq!(snapshot.node_id, "1.1");
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.source_turn_id, "turn-1");
    assert_eq!(snapshot.event_seq, 2);
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].stable_task_id, "step-1");
    assert_eq!(snapshot.items[0].step, "Inspect root");
    assert_eq!(snapshot.items[0].status, "in_progress");

    let plan = read_json(runtime.store().plan_path(&id(&[1, 1])));
    assert_eq!(plan["node_id"], "1.1");
    assert_eq!(plan["revision"], 1);
    assert_eq!(plan["event_seq"], 2);
    assert_eq!(plan["source_turn_id"], "turn-1");
    assert_eq!(plan["items"][0]["stable_task_id"], "step-1");
    assert_eq!(plan["items"][0]["status"], "in_progress");
    let tree = read_json_lines(runtime.store().tree_path());
    assert_eq!(tree[1]["type"], "task_plan_updated");
    assert_eq!(tree[1]["seq"], 2);
    assert_eq!(tree[1]["node_id"], "1.1");
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
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
}

#[test]
fn build_tree_snapshot_includes_node_local_plans() {
    let (_temp, mut runtime) = temp_runtime();

    runtime
        .record_plan_update("turn-1", plan_args("Inspect root", StepStatus::InProgress))
        .expect("record root plan");
    let snapshot = runtime.build_tree_snapshot().expect("build snapshot");

    assert_eq!(snapshot.snapshot_seq, 2);
    assert_eq!(snapshot.active_node_id, "1.1");
    assert_eq!(snapshot.nodes.len(), 2);
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1")
        .expect("root node");
    assert_eq!(root.node_id, "1");
    assert_eq!(root.parent_id, None);
    assert_eq!(root.summary, None);
    assert_eq!(root.status, SpineTreeNodeStatus::Opened);
    assert!(root.plan.is_none());
    let leaf = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1")
        .expect("initial leaf node");
    assert_eq!(leaf.parent_id.as_deref(), Some("1"));
    assert_eq!(leaf.status, SpineTreeNodeStatus::Live);
    let plan = leaf.plan.as_ref().expect("leaf plan");
    assert_eq!(plan.revision, 1);
    assert_eq!(plan.items[0].stable_task_id, "step-1");
    assert_eq!(plan.items[0].step, "Inspect root");
    assert_eq!(plan.items[0].status, SpineTreePlanItemStatus::InProgress);
}

#[test]
fn projection_reset_filters_plan_from_non_surviving_turn() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .record_plan_update(
            "rolled-back-turn",
            plan_args("Rolled back plan", StepStatus::InProgress),
        )
        .expect("record rolled back plan");

    let projected_state = runtime.state().clone();
    runtime
        .record_projection_reset(
            projected_state,
            0,
            HashSet::from(["surviving-turn".to_string()]),
            HashSet::new(),
            "test_projection",
            None,
        )
        .expect("record projection reset");
    let snapshot = runtime.build_tree_snapshot().expect("build tree snapshot");
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1")
        .expect("root node");

    assert!(root.plan.is_none());
    assert!(runtime.store().plan_path(&id(&[1, 1])).exists());
}

#[test]
fn record_plan_update_writes_plantree_without_moving_cursor() {
    let (_temp, mut runtime) = temp_runtime();
    let initial_state = runtime.state().clone();

    let snapshot = runtime
        .record_plan_update(
            "turn-alloc",
            plan_args_with_plantree(
                None,
                vec![
                    (
                        None,
                        "Reproduce failure",
                        vec!["run focused repro", "capture failing assertion"],
                    ),
                    (
                        None,
                        "Patch and verify",
                        vec!["apply minimal fix", "run regression test"],
                    ),
                ],
            ),
        )
        .expect("record PlanTree");

    assert_eq!(runtime.state(), &initial_state);
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
    assert_eq!(snapshot.node_id, "1.1");
    assert_eq!(snapshot.event_seq, 2);

    let plan = read_json(runtime.store().plan_path(&id(&[1, 1])));
    let spine_plantree = &plan["spine_plantree"];
    assert_eq!(spine_plantree["anchor_node_id"], "1.1");
    assert_eq!(spine_plantree["root"]["existing_node_id"], "1.1");
    assert_eq!(spine_plantree["root"]["summary"], "Editable task scope");
    assert_eq!(
        spine_plantree["root"]["children"][0]["existing_node_id"],
        Value::Null
    );
    assert_eq!(
        spine_plantree["root"]["children"][0]["summary"],
        "Reproduce failure"
    );
    assert_eq!(
        spine_plantree["root"]["children"][0]["checkpoints"],
        json!([
            {"task": "run focused repro", "status": "pending"},
            {"task": "capture failing assertion", "status": "pending"}
        ])
    );

    let tree = read_json_lines(runtime.store().tree_path());
    assert_eq!(tree.len(), 2);
    assert_eq!(tree[1]["type"], "task_plan_updated");
    assert_eq!(tree[1]["seq"], 2);
    assert_eq!(tree[1]["spine_plantree"]["anchor_node_id"], "1.1");

    let tree_snapshot = runtime.build_tree_snapshot().expect("build tree snapshot");
    let root = tree_snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1")
        .expect("root node");
    assert!(root.plan.is_none());
    let leaf = tree_snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1")
        .expect("initial leaf node");
    let plan = leaf.plan.as_ref().expect("leaf plan");
    let spine_plantree = plan.spine_plantree.as_ref().expect("root PlanTree");
    assert_eq!(spine_plantree.anchor_node_id, "1.1");
    assert_eq!(spine_plantree.root.existing_node_id.as_deref(), Some("1.1"));
    assert_eq!(spine_plantree.root.children.len(), 2);
    assert_eq!(spine_plantree.root.children[0].existing_node_id, None);
    assert_eq!(spine_plantree.root.children[0].summary, "Reproduce failure");
    assert_eq!(
        spine_plantree.root.children[0]
            .checkpoints
            .iter()
            .map(|checkpoint| checkpoint.task.as_str())
            .collect::<Vec<_>>(),
        vec!["run focused repro", "capture failing assertion"]
    );
}

#[test]
fn record_plan_update_preserves_plantree_when_omitted() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .record_plan_update(
            "turn-plantree",
            plan_args_with_plantree(
                None,
                vec![(None, "Verify scope", vec!["run focused tests"])],
            ),
        )
        .expect("record PlanTree");

    let second = runtime
        .record_plan_update(
            "turn-progress",
            plan_args("Run focused tests", StepStatus::InProgress),
        )
        .expect("record progress-only plan");

    let spine_plantree = second
        .spine_plantree
        .as_ref()
        .expect("omitted PlanTree should inherit previous snapshot");
    assert_eq!(spine_plantree.anchor_node_id, "1.1");
    assert_eq!(spine_plantree.root.existing_node_id.as_deref(), Some("1.1"));
    assert_eq!(spine_plantree.root.children[0].summary, "Verify scope");

    let plan = read_json(runtime.store().plan_path(&id(&[1, 1])));
    assert_eq!(plan["spine_plantree"]["root"]["existing_node_id"], "1.1");
    assert_eq!(
        plan["spine_plantree"]["root"]["children"][0]["summary"],
        "Verify scope"
    );
}

#[test]
fn record_plan_update_can_clear_plantree_explicitly() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .record_plan_update(
            "turn-plantree",
            plan_args_with_plantree(
                None,
                vec![(None, "Verify scope", vec!["run focused tests"])],
            ),
        )
        .expect("record PlanTree");

    let cleared = runtime
        .record_plan_update(
            "turn-clear",
            UpdatePlanArgs {
                explanation: Some("clear obsolete PlanTree".to_string()),
                plan: vec![PlanItemArg {
                    step: "Continue without a planned subtree".to_string(),
                    status: StepStatus::InProgress,
                }],
                spine_plantree: None,
                clear_spine_plantree: true,
            },
        )
        .expect("clear PlanTree");

    assert!(cleared.spine_plantree.is_none());
    let plan = read_json(runtime.store().plan_path(&id(&[1, 1])));
    assert!(plan.get("spine_plantree").is_none());
}

#[test]
fn plantree_defaults_to_open_parent_scope_when_cursor_is_child() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "open-1",
            "turn-open",
            SpineOperation::Open,
            None,
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
            "turn-child-plan",
            plan_args_with_plantree(None, vec![(None, "Child next scope", vec!["finish child"])]),
        )
        .expect("record PlanTree at open parent");

    let plan = read_json(runtime.store().plan_path(&id(&[1, 1, 1])));
    assert_eq!(plan["spine_plantree"]["anchor_node_id"], "1.1");
    assert_eq!(plan["spine_plantree"]["root"]["existing_node_id"], "1.1");

    let snapshot = runtime.build_tree_snapshot().expect("build tree snapshot");
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1")
        .expect("root node");
    let child = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1.1")
        .expect("child node");
    assert!(root.plan.is_none());
    assert!(child.plan.is_some());
    let spine_plantree = child
        .plan
        .as_ref()
        .and_then(|plan| plan.spine_plantree.as_ref())
        .expect("child PlanTree");
    assert_eq!(spine_plantree.anchor_node_id, "1.1");
    assert_eq!(spine_plantree.root.existing_node_id.as_deref(), Some("1.1"));
}

#[test]
fn plantree_rejects_finished_scope_nodes() {
    let (_temp, mut runtime) = temp_runtime();
    runtime
        .stage_transition(
            "open-1",
            "turn-open",
            SpineOperation::Open,
            None,
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
        .stage_transition(
            "next-1",
            "turn-next",
            SpineOperation::Next,
            "child finished",
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

    let initial_state = runtime.state().clone();
    let initial_tree = read_json_lines(runtime.store().tree_path());
    let error = runtime
        .record_plan_update(
            "turn-invalid",
            plan_args_with_plantree(
                Some("1.1"),
                vec![(
                    Some("1.1.1"),
                    "Rewrite finished child",
                    vec!["should be rejected"],
                )],
            ),
        )
        .expect_err("finished nodes must be read-only for PlanTree");

    assert!(matches!(
        error,
        SpineRuntimeError::InvalidPlanTree { message }
            if message.contains("plantree scope [1.1.1] is read-only")
    ));
    assert_eq!(runtime.state(), &initial_state);
    assert_eq!(read_json_lines(runtime.store().tree_path()), initial_tree);
    assert!(!runtime.store().plan_path(&id(&[1, 1, 2])).exists());
}

#[test]
fn plantree_rejects_duplicate_existing_scope_nodes() {
    let (_temp, mut runtime) = temp_runtime();
    let initial_state = runtime.state().clone();
    let initial_tree = read_json_lines(runtime.store().tree_path());

    let error = runtime
        .record_plan_update(
            "turn-invalid",
            plan_args_with_plantree(
                None,
                vec![(
                    Some("1.1"),
                    "Duplicate editable scope",
                    vec!["ambiguous child"],
                )],
            ),
        )
        .expect_err("duplicate existing scope nodes must be rejected");

    assert!(matches!(
        error,
        SpineRuntimeError::InvalidPlanTree { message }
            if message.contains("plantree scope [1.1] is duplicated")
    ));
    assert_eq!(runtime.state(), &initial_state);
    assert_eq!(read_json_lines(runtime.store().tree_path()), initial_tree);
    assert!(!runtime.store().plan_path(&id(&[1, 1])).exists());
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
            None,
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

    assert_eq!(snapshot.active_node_id, "1.1.2");
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1")
        .expect("root node");
    let scope = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1")
        .expect("scope node");
    let child = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1.1")
        .expect("child node");
    let sibling = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1.2")
        .expect("sibling node");

    assert_eq!(root.status, SpineTreeNodeStatus::Opened);
    assert_eq!(root.plan, None);
    assert_eq!(scope.status, SpineTreeNodeStatus::Opened);
    assert_eq!(scope.plan, None);
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
    assert!(!runtime.store().worklog_path(&id(&[1, 1])).exists());
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
                "to_parent_id": "1.1",
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
fn next_compact_boundary_uses_finished_leaf_raw_start() {
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
    assert_eq!(boundary.node_id, id(&[1, 1, 1]));
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
        .after_response_items_recorded("turn-1", &[assistant_message("root child work")], 0, 1)
        .expect("record child work");

    let error = runtime
        .stage_transition(
            "close-1",
            "turn-1",
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
    assert_eq!(boundary.node_id, id(&[1, 1, 1]));
    assert_eq!(boundary.transition_summary, "second child done");
    assert_eq!(
        boundary.compact_instruction.as_deref(),
        Some("keep subtree decisions")
    );

    let outline = runtime
        .render_context_compacted_outline(&id(&[1, 1, 1]))
        .expect("render outline");
    let base = runtime.store().root().display().to_string();

    assert!(outline.contains("## Context Compacted"));
    assert!(outline.contains(&format!("Base: {base}")));
    assert!(outline.contains("[1.1.1] second child done (nodes/1/1/1/worklog.md)"));
    assert!(outline.contains("|-- [1.1.1.1] first child done (nodes/1/1/1/1/worklog.md)"));
    assert!(outline.contains("|-- [1.1.1.2] finished (nodes/1/1/1/2/worklog.md)"));
    assert!(
        outline.find("|-- [1.1.1.1]").expect("first child row")
            < outline.find("|-- [1.1.1.2]").expect("second child row")
    );

    let model_outline = runtime
        .render_model_context_compacted_outline(&id(&[1, 1, 1]))
        .expect("render model outline");
    assert!(model_outline.contains("## Context Compacted"));
    assert!(model_outline.contains("[1.1.1] second child done"));
    assert!(model_outline.contains("|-- [1.1.1.1] first child done"));
    assert!(model_outline.contains("|-- [1.1.1.2] finished"));
    assert!(!model_outline.contains("Base:"));
    assert!(!model_outline.contains("worklog.md"));
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
            SpineOperation::Next,
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
    assert!(!runtime.store().worklog_path(&id(&[1])).exists());
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
    assert_eq!(runtime.cursor(), &id(&[1, 1]));
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

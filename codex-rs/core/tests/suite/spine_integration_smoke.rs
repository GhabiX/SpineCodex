#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used)]

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use serde_json::Value;
use serde_json::json;

const OPEN_CALL_ID: &str = "spine-open-call";
const CHILD_SHELL_CALL_ID: &str = "child-shell-call";
const PLAN_CALL_ID: &str = "plan-call";
const NEXT_CALL_ID: &str = "spine-next-call";
const SIBLING_SHELL_CALL_ID: &str = "sibling-shell-call";

const OPEN_SUMMARY: &str = "open child scope";
const OPEN_WORKLOG: &str = "Root handoff for child shell smoke.";
const NEXT_SUMMARY: &str = "finish child scope";
const NEXT_WORKLOG: &str = "Child handoff for sibling shell smoke.";
const EXPECTED_SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
You have a task tree tool named spine.
Use the active task tree to split complex work into focused right-spine nodes.
Keep simple tasks in one node.
Call spine open when starting a focused subproblem.
Call spine next when handing off from one sibling task to the next.
Call spine close when finishing a child scope and returning to the parent sibling.
Every spine call must include a concise summary and a durable worklog containing goal, findings, decisions, verification, and risks.
Use update_plan only as the TODO list for the current active node; do not treat update_plan as the task tree driver.
There is no read_spine tool; inspect task-tree files, worklogs, and historical rollout trajs with bash when needed.
In Plan mode, do not call mutating spine operations.
</spine_view>"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_transitions_commit_before_following_tools_in_same_response() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-open"),
                ev_function_call(
                    OPEN_CALL_ID,
                    "spine",
                    &spine_args("open", OPEN_SUMMARY, OPEN_WORKLOG),
                ),
                ev_function_call(
                    CHILD_SHELL_CALL_ID,
                    "shell_command",
                    &shell_args("printf 'child-spine\\n'"),
                ),
                ev_completed("resp-open"),
            ]),
            sse(vec![
                ev_response_created("resp-plan"),
                ev_function_call(PLAN_CALL_ID, "update_plan", &plan_args()),
                ev_completed("resp-plan"),
            ]),
            sse(vec![
                ev_response_created("resp-next"),
                ev_function_call(
                    NEXT_CALL_ID,
                    "spine",
                    &spine_args("next", NEXT_SUMMARY, NEXT_WORKLOG),
                ),
                ev_function_call(
                    SIBLING_SHELL_CALL_ID,
                    "shell_command",
                    &shell_args("printf 'sibling-spine\\n'"),
                ),
                ev_completed("resp-next"),
            ]),
            sse(vec![
                ev_response_created("resp-done"),
                ev_assistant_message("msg-done", "done"),
                ev_completed("resp-done"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config
            .features
            .enable(Feature::SpineTaskTree)
            .expect("enable spine task tree");
    });
    let test = builder.build(&server).await?;
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("session should expose rollout path");

    let plan_turn_id = submit_turn_and_assert_plan_update(&test).await?;

    let requests = responses.requests();
    let base_instructions = model_base_instructions(&test).await;
    let expected_instructions =
        format!("{base_instructions}\n\n{EXPECTED_SPINE_VIEW_INSTRUCTIONS}");
    assert_eq!(
        requests
            .first()
            .expect("expected first model request")
            .instructions_text()
            .as_bytes(),
        expected_instructions.as_bytes(),
        "feature-on request should append exact spine steering instructions"
    );
    assert_function_output_contains(&requests, CHILD_SHELL_CALL_ID, "child-spine");
    assert_function_output_contains(&requests, SIBLING_SHELL_CALL_ID, "sibling-spine");
    assert_function_output_contains(&requests, PLAN_CALL_ID, "Plan updated");

    let sidecar_dir = sidecar_dir_for_rollout_path(&rollout_path);
    let tree_path = sidecar_dir.join("tree.jsonl");
    let index_path = sidecar_dir.join("trajs.index.jsonl");
    let tree_text = std::fs::read_to_string(&tree_path)
        .with_context(|| format!("read {}", tree_path.display()))?;
    let index_text = std::fs::read_to_string(&index_path)
        .with_context(|| format!("read {}", index_path.display()))?;
    let tree = parse_json_lines(&tree_text)?;
    let index = parse_json_lines(&index_text)?;

    assert_root_created(&tree);
    assert_transition(&tree, "open", "1", "1.1", OPEN_SUMMARY);
    assert_plan_updated(&tree, "1.1", 1, &plan_turn_id);
    assert_transition(&tree, "next", "1.1", "1.2", NEXT_SUMMARY);
    assert_transition_committed(&index, OPEN_CALL_ID, "1", "1.1");
    assert_transition_committed(&index, NEXT_CALL_ID, "1.1", "1.2");
    assert_raw_range_for_node_after_transition(&index, OPEN_CALL_ID, "1.1");
    assert_raw_range_for_node_after_transition(&index, NEXT_CALL_ID, "1.2");

    assert_eq!(
        std::fs::read_to_string(sidecar_dir.join("nodes/1/worklog.md"))?,
        OPEN_WORKLOG
    );
    assert_eq!(
        std::fs::read_to_string(sidecar_dir.join("nodes/1/1/worklog.md"))?,
        NEXT_WORKLOG
    );
    let plan_snapshot = read_json(sidecar_dir.join("nodes/1/1/plan.json"))?;
    assert_eq!(plan_snapshot["node_id"], "1.1");
    assert_eq!(plan_snapshot["revision"], 1);
    assert_eq!(plan_snapshot["source_turn_id"], plan_turn_id);
    assert_eq!(plan_snapshot["event_seq"], 3);
    assert_eq!(plan_snapshot["explanation"], "plan still works");
    assert_eq!(plan_snapshot["items"][0]["stable_task_id"], "step-1");
    assert_eq!(plan_snapshot["items"][0]["step"], "Exercise child node");
    assert_eq!(plan_snapshot["items"][0]["status"], "completed");
    assert_eq!(plan_snapshot["items"][1]["stable_task_id"], "step-2");
    assert_eq!(plan_snapshot["items"][1]["step"], "Exercise sibling node");
    assert_eq!(plan_snapshot["items"][1]["status"], "in_progress");
    assert!(
        !index_text.contains("child-spine") && !index_text.contains("sibling-spine"),
        "sidecar index must not duplicate raw shell output: {index_text}"
    );

    let rollout_text = std::fs::read_to_string(&rollout_path)
        .with_context(|| format!("read {}", rollout_path.display()))?;
    assert!(
        rollout_text.contains(CHILD_SHELL_CALL_ID) && rollout_text.contains("child-spine"),
        "rollout should remain the raw traj source for child shell output"
    );
    assert!(
        rollout_text.contains(SIBLING_SHELL_CALL_ID) && rollout_text.contains("sibling-spine"),
        "rollout should remain the raw traj source for sibling shell output"
    );
    assert_rollout_has_no_compaction_items(&rollout_text)?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_feature_off_exposes_no_task_tree_tools_or_sidecar() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_response_created("resp-feature-off"),
            ev_assistant_message("msg-feature-off", "done"),
            ev_completed("resp-feature-off"),
        ])],
    )
    .await;

    let mut builder = test_codex().with_model("gpt-5.4");
    let test = builder.build(&server).await?;
    test.submit_turn("feature off smoke").await?;

    let request = responses.single_request();
    let instructions = request.instructions_text();
    let base_instructions = model_base_instructions(&test).await;
    assert_eq!(
        instructions.as_bytes(),
        base_instructions.as_bytes(),
        "feature-off request instructions should remain byte-identical"
    );

    let tool_names = tool_names(&request);
    for forbidden in ["spine", "read_spine", "spine_state", "spine_trajs"] {
        assert!(
            !tool_names.iter().any(|name| name == forbidden),
            "feature-off request unexpectedly exposed {forbidden}: {tool_names:?}"
        );
    }

    let rollout_path = test
        .session_configured
        .rollout_path
        .as_ref()
        .expect("session should expose rollout path");
    let sidecar_dir = sidecar_dir_for_rollout_path(rollout_path);
    assert!(
        !sidecar_dir.exists(),
        "feature-off session should not create spine sidecar at {}",
        sidecar_dir.display()
    );

    Ok(())
}

async fn model_base_instructions(test: &core_test_support::test_codex::TestCodex) -> String {
    test.thread_manager
        .get_models_manager()
        .get_model_info(
            test.session_configured.model.as_str(),
            &test.config.to_models_manager_config(),
        )
        .await
        .get_model_instructions(test.config.personality)
}

async fn submit_turn_and_assert_plan_update(
    test: &core_test_support::test_codex::TestCodex,
) -> anyhow::Result<String> {
    let cwd_path = test.cwd.path().to_path_buf();
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, cwd_path.as_path());
    let session_model = test.session_configured.model.clone();

    test.codex
        .submit(Op::UserTurn {
            environments: None,
            items: vec![UserInput::Text {
                text: "exercise spine integration smoke".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            cwd: cwd_path,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: None,
            sandbox_policy,
            permission_profile,
            model: session_model,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;

    let turn_id = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnStarted(event) => Some(event.turn_id.clone()),
        _ => None,
    })
    .await;

    let mut saw_plan_update = false;
    wait_for_event(&test.codex, |event| match event {
        EventMsg::PlanUpdate(update) => {
            saw_plan_update = true;
            assert_eq!(update.explanation.as_deref(), Some("plan still works"));
            assert_eq!(update.plan.len(), 2);
            assert_eq!(update.plan[0].step, "Exercise child node");
            assert!(matches!(update.plan[0].status, StepStatus::Completed));
            assert_eq!(update.plan[1].step, "Exercise sibling node");
            assert!(matches!(update.plan[1].status, StepStatus::InProgress));
            false
        }
        EventMsg::TurnComplete(event) => event.turn_id == turn_id,
        _ => false,
    })
    .await;
    assert!(saw_plan_update, "expected normal PlanUpdate event");

    Ok(turn_id)
}

fn shell_args(command: &str) -> String {
    json!({
        "command": command,
        "timeout_ms": 2_000,
    })
    .to_string()
}

fn spine_args(op: &str, summary: &str, worklog: &str) -> String {
    json!({
        "op": op,
        "summary": summary,
        "worklog": worklog,
    })
    .to_string()
}

fn plan_args() -> String {
    json!({
        "explanation": "plan still works",
        "plan": [
            {"step": "Exercise child node", "status": "completed"},
            {"step": "Exercise sibling node", "status": "in_progress"}
        ],
    })
    .to_string()
}

fn sidecar_dir_for_rollout_path(rollout_path: &Path) -> PathBuf {
    let parent = rollout_path
        .parent()
        .expect("rollout path should have parent");
    let stem = rollout_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("rollout path should have UTF-8 stem");
    parent.join(format!("spine-{stem}"))
}

fn parse_json_lines(contents: &str) -> anyhow::Result<Vec<Value>> {
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("parse jsonl line"))
        .collect()
}

fn read_json(path: impl AsRef<Path>) -> anyhow::Result<Value> {
    let path = path.as_ref();
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("parse {}", path.display()))
}

fn tool_names(req: &ResponsesRequest) -> Vec<String> {
    req.body_json()
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("name")
                        .or_else(|| tool.get("type"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn assert_function_output_contains(requests: &[ResponsesRequest], call_id: &str, expected: &str) {
    let output = requests
        .iter()
        .find_map(|request| request.function_call_output_text(call_id))
        .unwrap_or_else(|| panic!("function_call_output missing for {call_id}"));
    assert!(
        output.contains(expected),
        "expected output for {call_id} to contain {expected:?}, got {output:?}"
    );
}

fn assert_root_created(tree: &[Value]) {
    assert!(
        tree.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("node_created")
                && event.get("node_id").and_then(Value::as_str) == Some("1")
                && event.get("parent_id").is_some_and(Value::is_null)
                && event.get("raw_start_ordinal").and_then(Value::as_u64) == Some(0)
        }),
        "tree should contain deterministic root creation event: {tree:?}"
    );
}

fn assert_transition(tree: &[Value], op: &str, from_node: &str, to_node: &str, summary: &str) {
    let event = tree
        .iter()
        .find(|event| {
            event.get("type").and_then(Value::as_str) == Some("transition_applied")
                && event.get("op").and_then(Value::as_str) == Some(op)
                && event.get("from_node").and_then(Value::as_str) == Some(from_node)
                && event.get("to_node").and_then(Value::as_str) == Some(to_node)
        })
        .unwrap_or_else(|| panic!("missing {op} transition {from_node} -> {to_node}: {tree:?}"));

    assert_eq!(event.get("summary").and_then(Value::as_str), Some(summary));
    assert!(
        event
            .get("worklog_hash")
            .and_then(Value::as_str)
            .is_some_and(|hash| hash.starts_with("sha1:")),
        "transition should contain a worklog hash: {event:?}"
    );
}

fn assert_plan_updated(tree: &[Value], node_id: &str, revision: u64, source_turn_id: &str) {
    let event = tree
        .iter()
        .find(|event| {
            event.get("type").and_then(Value::as_str) == Some("task_plan_updated")
                && event.get("node_id").and_then(Value::as_str) == Some(node_id)
                && event.get("revision").and_then(Value::as_u64) == Some(revision)
        })
        .unwrap_or_else(|| panic!("missing plan update for {node_id} rev {revision}: {tree:?}"));

    assert_eq!(
        event.get("source_turn_id").and_then(Value::as_str),
        Some(source_turn_id)
    );
    assert_eq!(
        event.get("explanation").and_then(Value::as_str),
        Some("plan still works")
    );
    assert_eq!(
        event.get("items").and_then(Value::as_array).map(Vec::len),
        Some(2)
    );
}

fn assert_transition_committed(index: &[Value], call_id: &str, from_node: &str, to_node: &str) {
    assert!(
        index.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("transition_committed")
                && event.get("call_id").and_then(Value::as_str) == Some(call_id)
                && event.get("from_node").and_then(Value::as_str) == Some(from_node)
                && event.get("to_node").and_then(Value::as_str) == Some(to_node)
                && event.get("boundary_end").and_then(Value::as_u64).is_some()
        }),
        "index should contain transition commit {call_id} {from_node} -> {to_node}: {index:?}"
    );
}

fn assert_raw_range_for_node_after_transition(index: &[Value], call_id: &str, node_id: &str) {
    let boundary_end = index
        .iter()
        .find(|event| {
            event.get("type").and_then(Value::as_str) == Some("transition_committed")
                && event.get("call_id").and_then(Value::as_str) == Some(call_id)
        })
        .and_then(|event| event.get("boundary_end").and_then(Value::as_u64))
        .unwrap_or_else(|| panic!("missing boundary for {call_id}: {index:?}"));

    assert!(
        index.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("raw_items_recorded")
                && event.get("node_id").and_then(Value::as_str) == Some(node_id)
                && event
                    .get("start")
                    .and_then(Value::as_u64)
                    .is_some_and(|start| start >= boundary_end)
                && event
                    .get("end")
                    .and_then(Value::as_u64)
                    .zip(event.get("start").and_then(Value::as_u64))
                    .is_some_and(|(end, start)| end > start)
        }),
        "expected raw range for node {node_id} after {call_id} boundary {boundary_end}: {index:?}"
    );
}

fn assert_rollout_has_no_compaction_items(rollout_text: &str) -> anyhow::Result<()> {
    for line in rollout_text.lines().filter(|line| !line.trim().is_empty()) {
        let entry: RolloutLine = serde_json::from_str(line).context("parse rollout line")?;
        match entry.item {
            RolloutItem::Compacted(_)
            | RolloutItem::ResponseItem(ResponseItem::Compaction { .. })
            | RolloutItem::ResponseItem(ResponseItem::ContextCompaction { .. }) => {
                panic!("Plan1 smoke should not introduce compaction rollout items: {line}")
            }
            _ => {}
        }
    }
    Ok(())
}

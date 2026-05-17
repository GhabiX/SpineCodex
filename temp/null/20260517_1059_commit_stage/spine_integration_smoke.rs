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
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_failed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use serde_json::Value;
use serde_json::json;

const OPEN_CALL_ID: &str = "spine-open-call";
const NESTED_OPEN_CALL_ID: &str = "spine-nested-open-call";
const CHILD_SHELL_CALL_ID: &str = "child-shell-call";
const PLAN_CALL_ID: &str = "plan-call";
const NEXT_CALL_ID: &str = "spine-next-call";
const SIBLING_SHELL_CALL_ID: &str = "sibling-shell-call";
const CLOSE_CALL_ID: &str = "spine-close-call";
const ROOT_SHELL_CALL_ID: &str = "root-shell-call";

const OPEN_SUMMARY: &str = "open root child scope";
const NESTED_OPEN_SUMMARY: &str = "open focused child scope";
const NEXT_SUMMARY: &str = "finish child scope";
const CLOSE_CHILD_SUMMARY: &str = "finish sibling leaf";
const CLOSE_SUMMARY: &str = "finish sibling scope";
const EXPECTED_SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine as your task plan and context manager. Completed scopes are folded into runtime-generated worklog IR, and later turns carry the visible Spine Tree, completed worklogs, and the current live suffix instead of every old raw message.
Spine Worklog is internal context; never expose or imitate it in user-visible messages.
Use Spine effectively and efficiently.
At the start, use update_plan with spine_plantree to maintain one compact task tree draft for the current editable scope. This is planning only; it does not create Spine nodes or move the cursor.
Use update_plan's top-level plan for the current real Spine node's checklist; use spine_plantree.root.children for future planned child scopes, and put each future child scope's checklist in that scope's checkpoints.
Future PlanTree scopes may display as `~<predicted-id>` to distinguish planned nodes from real Spine nodes.
Default to staying in the current live node while it remains focused. Use update_plan to revise the current PlanTree when new evidence changes the task structure.
When your task structure or next work scope changes, promptly refresh the current spine_plantree with update_plan so the displayed PlanTree stays current.
When update_plan succeeds with a writable Spine tree, treat the returned `spine_tree` JSON as the authoritative updated tree for the next decision.
For non-trivial or multi-phase work, keep future planned scopes in `spine_plantree.root.children` rather than flattening them into the current node's top-level plan, and update them with `update_plan`; this manages planning only and does not create real Spine nodes.
Treat the current spine_plantree as the execution plan for the current real Spine node. Before starting a new coherent work scope, compare it with the current node's planned children: if the work matches a planned child, call spine.open to materialize that child before doing the work, then immediately call update_plan in the new child using that planned child's summary/checkpoints as the active scope plan. If the work no longer matches the planned children, update spine_plantree first; do not bypass planned child scopes by calling spine.next from the parent.
Move Spine at coherent scope boundaries rather than as a per-command habit:
- spine.open: start a focused child scope that should inherit the parent goal but keep its own local context; use it before working on a matching planned child scope. It takes no arguments.
- spine.next: finish the current leaf and move to its next sibling when the next work is sibling-level under the same parent.
- spine.close: finish the current leaf, close its non-root parent scope, and continue at the parent's next sibling when the parent scope is complete. Root cannot be closed. It requires `child_summary` for the current leaf and `summary` for the parent scope.
spine.next/close are not end-of-response cleanup; when the current response still belongs to the current node, finish its user-visible work there, and only move Spine when beginning genuinely new sibling/parent-sibling work.
Spine transitions are internal context-management steps, not substitutes for normal Codex turn delivery: after spine.next or spine.close, continue work if the latest user request remains unfinished, or send the user-facing final answer/update if that request is complete, paused, blocked, or needs a decision. Do not use a Spine Tree update, tool output, or generated worklog as the user-visible report.
Use spine.next or spine.close to fold completed scopes after substantial raw history has accumulated or when future work is likely to reuse the generated worklog IR.
At root depth, use spine.next to finish the current root child and continue with its next sibling; use spine.close only from a nested scope when closing its parent and returning to the parent's next sibling.
For spine.next, use summary as the short completion-time Spine Tree label. For spine.close, use child_summary as the label for the current leaf and summary as the label for the parent scope. Use the optional instruction argument when the automatic compact pass should prioritize specific facts to preserve from the completed leaf or scope. Do not use summary, child_summary, or instruction with spine.open.
Use spine.tree to inspect the current node and Spine Tree without moving the cursor.
Do not move spine only because a new user message arrived, because you answered a short question, or because you updated progress within the same scope.
Do not create one node per shell command, checklist item, short reply, or conversation turn.
After spine.next from `1.1` to `1.2`, the runtime folds `1.1`'s raw trace into `nodes/1/1/worklog.md`; later context shows the Spine Tree plus `1.1` worklog, not `1.1` raw trace.
After spine.close from `1.1.2` to `1.2`, the runtime first folds the closing child into `nodes/1/1/2/worklog.md`, then folds the completed `1.1` scope into `nodes/1/1/worklog.md`; child scopes remain available as durable worklog IR while parent context uses the parent worklog by default.
Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../worklog.md` against that Base, not against the workspace cwd.
After spine.next or spine.close, if unfinished work remains, use update_plan to refresh the current PlanTree from the generated worklog, latest user intent, and current evidence.
Keep working in the current node while its raw details are still useful. When a coherent work scope is complete, fold it so later turns use its worklog instead of its raw trace.
Avoid tiny splits for individual commands, small observations, or conversation turns.
The runtime may warn when the current node grows large: around 80k raw tokens, then every additional 30k. Treat the warning as a cue to finish the current scope cleanly, then use spine.next or spine.close if the next work can rely on the worklog.
When moving between nodes, rely on the runtime Spine Tree and generated worklogs; inspect sidecar trajs/worklog files only when you need historical details.
Completed Spine nodes are read-only; rely on their worklogs instead of restating their old PlanTree checkpoints.
In Plan mode, do not call mutating spine operations.
</spine_view>"#;
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_transitions_commit_and_compact_before_following_tools_in_same_response()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-open"),
                ev_spine_transition_call(OPEN_CALL_ID, "open", OPEN_SUMMARY, None),
                ev_completed("resp-open"),
            ]),
            sse(vec![
                ev_response_created("resp-nested-open"),
                ev_spine_transition_call(NESTED_OPEN_CALL_ID, "open", NESTED_OPEN_SUMMARY, None),
                ev_function_call(
                    CHILD_SHELL_CALL_ID,
                    "shell_command",
                    &shell_args("printf 'child-spine\\n'"),
                ),
                ev_completed("resp-nested-open"),
            ]),
            sse(vec![
                ev_response_created("resp-plan"),
                ev_function_call(PLAN_CALL_ID, "update_plan", &plan_args()),
                ev_completed("resp-plan"),
            ]),
            sse(vec![
                ev_response_created("resp-next"),
                ev_spine_transition_call(NEXT_CALL_ID, "next", NEXT_SUMMARY, None),
                ev_function_call(
                    SIBLING_SHELL_CALL_ID,
                    "shell_command",
                    &shell_args("printf 'sibling-spine\\n'"),
                ),
                ev_completed("resp-next"),
            ]),
            sse(vec![
                ev_response_created("resp-spine-compact"),
                ev_assistant_message("msg-spine-compact", "Compacted child findings."),
                ev_completed("resp-spine-compact"),
            ]),
            sse(vec![
                ev_response_created("resp-close"),
                ev_spine_transition_call(
                    CLOSE_CALL_ID,
                    "close",
                    CLOSE_SUMMARY,
                    Some(CLOSE_CHILD_SUMMARY),
                ),
                ev_function_call(
                    ROOT_SHELL_CALL_ID,
                    "shell_command",
                    &shell_args("printf 'root-spine\\n'"),
                ),
                ev_completed("resp-close"),
            ]),
            sse(vec![
                ev_response_created("resp-spine-close-child-compact"),
                ev_assistant_message(
                    "msg-spine-close-child-compact",
                    "Compacted sibling leaf findings.",
                ),
                ev_completed("resp-spine-close-child-compact"),
            ]),
            sse(vec![
                ev_response_created("resp-spine-close-parent-compact"),
                ev_assistant_message("msg-spine-close-parent-compact", "Compacted root findings."),
                ev_completed("resp-spine-close-parent-compact"),
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
    assert_function_output_contains(&requests, ROOT_SHELL_CALL_ID, "root-spine");
    let plan_output = function_output_json(&requests, PLAN_CALL_ID)?;
    assert_eq!(plan_output["status"], "plan_updated");
    assert_eq!(plan_output["spine_tree"]["activeNodeId"], "1.1.1.1");
    let output_active_node = plan_output["spine_tree"]["nodes"]
        .as_array()
        .expect("spine tree output nodes")
        .iter()
        .find(|node| node["nodeId"] == "1.1.1.1")
        .expect("active node in output");
    assert_eq!(
        output_active_node["plan"]["items"][0]["step"],
        "Exercise child node"
    );
    assert_eq!(
        output_active_node["plan"]["spinePlantree"]["root"]["children"][0]["checkpoints"][0]["task"],
        "Exercise future child scope"
    );

    let sidecar_dir = sidecar_dir_for_rollout_path(&rollout_path);
    let tree_path = sidecar_dir.join("tree.jsonl");
    let index_path = sidecar_dir.join("trajs.index.jsonl");
    let compact_index_path = sidecar_dir.join("compact.index.jsonl");
    let tree_text = std::fs::read_to_string(&tree_path)
        .with_context(|| format!("read {}", tree_path.display()))?;
    let index_text = std::fs::read_to_string(&index_path)
        .with_context(|| format!("read {}", index_path.display()))?;
    let compact_index_text = std::fs::read_to_string(&compact_index_path)
        .with_context(|| format!("read {}", compact_index_path.display()))?;
    let tree = parse_json_lines(&tree_text)?;
    let index = parse_json_lines(&index_text)?;
    let compact_index = parse_json_lines(&compact_index_text)?;

    assert_spine_initialized(&tree);
    assert_transition(&tree, "open", "1.1", "1.1.1", None);
    assert_transition(&tree, "open", "1.1.1", "1.1.1.1", None);
    let plan_event_seq = assert_plan_updated(&tree, "1.1.1.1", 1, &plan_turn_id);
    assert_transition(&tree, "next", "1.1.1.1", "1.1.1.2", Some(NEXT_SUMMARY));
    assert_transition_with_child_summary(
        &tree,
        "close",
        "1.1.1.2",
        "1.1.2",
        Some(CLOSE_SUMMARY),
        Some(CLOSE_CHILD_SUMMARY),
    );
    assert_transition_committed(&index, OPEN_CALL_ID, "1.1", "1.1.1");
    assert_transition_committed(&index, NESTED_OPEN_CALL_ID, "1.1.1", "1.1.1.1");
    assert_transition_committed(&index, NEXT_CALL_ID, "1.1.1.1", "1.1.1.2");
    assert_transition_committed(&index, CLOSE_CALL_ID, "1.1.1.2", "1.1.2");
    assert_raw_range_for_node_after_transition(&index, NESTED_OPEN_CALL_ID, "1.1.1.1");
    assert_raw_range_for_node_after_transition(&index, NEXT_CALL_ID, "1.1.1.2");
    assert_raw_range_for_node_after_transition(&index, CLOSE_CALL_ID, "1.1.2");

    let scope_worklog = std::fs::read_to_string(sidecar_dir.join("nodes/1/1/1/worklog.md"))?;
    let base_line = format!("Base: {}", sidecar_dir.display());
    assert!(scope_worklog.contains("spine:auto-compact-generated"));
    assert!(scope_worklog.contains(&base_line));
    assert!(scope_worklog.contains("Compacted root findings."));
    assert!(scope_worklog.contains("## Context Compacted"));
    assert!(scope_worklog.contains("[1.1.1] finish sibling scope (nodes/1/1/1/worklog.md)"));
    assert!(scope_worklog.contains("|-- [1.1.1.1] finish child scope (nodes/1/1/1/1/worklog.md)"));
    assert!(scope_worklog.contains("|-- [1.1.1.2] finish sibling leaf (nodes/1/1/1/2/worklog.md)"));
    let first_leaf_worklog = std::fs::read_to_string(sidecar_dir.join("nodes/1/1/1/1/worklog.md"))?;
    assert!(first_leaf_worklog.contains("spine:auto-compact-generated"));
    assert!(first_leaf_worklog.contains(&base_line));
    assert!(first_leaf_worklog.contains("Compacted child findings."));
    let closing_leaf_worklog =
        std::fs::read_to_string(sidecar_dir.join("nodes/1/1/1/2/worklog.md"))?;
    assert!(closing_leaf_worklog.contains("spine:auto-compact-generated"));
    assert!(closing_leaf_worklog.contains(&base_line));
    assert!(closing_leaf_worklog.contains("Compacted sibling leaf findings."));
    assert_compact_installed(&compact_index, "1.1.1.1", "next");
    assert_compact_installed_before(&compact_index, "1.1.1.2", "close", "1.1.1", "close");
    assert_compact_installed(&compact_index, "1.1.1", "close");
    let plan_snapshot = read_json(sidecar_dir.join("nodes/1/1/1/1/plan.json"))?;
    assert_eq!(plan_snapshot["node_id"], "1.1.1.1");
    assert_eq!(plan_snapshot["revision"], 1);
    assert_eq!(plan_snapshot["source_turn_id"], plan_turn_id);
    assert_eq!(plan_snapshot["event_seq"], plan_event_seq);
    assert_eq!(plan_snapshot["explanation"], "plan still works");
    assert_eq!(plan_snapshot["items"][0]["stable_task_id"], "step-1");
    assert_eq!(plan_snapshot["items"][0]["step"], "Exercise child node");
    assert_eq!(plan_snapshot["items"][0]["status"], "completed");
    assert_eq!(plan_snapshot["items"][1]["stable_task_id"], "step-2");
    assert_eq!(plan_snapshot["items"][1]["step"], "Exercise sibling node");
    assert_eq!(plan_snapshot["items"][1]["status"], "in_progress");
    assert_eq!(
        plan_snapshot["spine_plantree"]["root"]["children"][0]["checkpoints"][0]["task"],
        "Exercise future child scope"
    );
    assert_eq!(
        plan_snapshot["spine_plantree"]["root"]["children"][0]["existing_node_id"],
        Value::Null,
        "future planned child scope must not be materialized as a real node"
    );
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
    assert!(
        rollout_text.contains(ROOT_SHELL_CALL_ID) && rollout_text.contains("root-spine"),
        "rollout should remain the raw traj source for root shell output"
    );
    assert_rollout_has_spine_compaction_checkpoint(&rollout_text, 3)?;
    assert_raw_mirror_has_raw_items_and_compact_metadata(&sidecar_dir, 3)?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_auto_compact_archives_root_epoch_and_stays_mutable() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-open"),
                ev_spine_transition_call(OPEN_CALL_ID, "open", OPEN_SUMMARY, None),
                ev_completed_with_tokens("resp-open", 70_000),
            ]),
            sse(vec![
                ev_response_created("resp-work"),
                ev_assistant_message("msg-work", "work before pressure"),
                ev_completed_with_tokens("resp-work", 330_000),
            ]),
            sse(vec![
                ev_response_created("resp-auto-compact"),
                ev_assistant_message("msg-auto-compact", "auto root archive summary"),
                ev_completed_with_tokens("resp-auto-compact", 200),
            ]),
            sse(vec![
                ev_response_created("resp-after"),
                ev_spine_transition_call(
                    "post-archive-open-call",
                    "open",
                    "post archive scope",
                    None,
                ),
                ev_completed_with_tokens("resp-after", 120),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config.model_auto_compact_token_limit = Some(200_000);
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

    test.submit_turn("open spine before pressure").await?;
    test.submit_turn("raise token pressure").await?;
    test.submit_turn("continue after auto compact").await?;

    let requests = responses.requests();
    assert_eq!(requests.len(), 4, "expected user, user, compact, follow-up");
    assert!(
        requests[2].body_contains_text(SUMMARIZATION_PROMPT),
        "spine auto compact should use the normal compact prompt"
    );
    assert!(
        !tool_names(&requests[2]).iter().any(|name| name == "spine"),
        "spine root auto compact should reuse native textual compact, not the Spine suffix compact tool envelope"
    );
    assert!(
        !requests[2].body_contains_text("Compact only target Spine node"),
        "root auto compact should not use the Spine suffix compact prompt"
    );
    assert!(
        !requests[3].body_contains_text("<spine_worklog")
            && requests[3].body_contains_text("## Spine Worklog")
            && requests[3].body_contains_text("auto root archive summary"),
        "follow-up should use a readable spine root-epoch IR checkpoint"
    );
    assert!(!requests[3].body_contains_text("fold_start"));
    assert!(!requests[3].body_contains_text("fold_end"));
    assert!(!requests[3].body_contains_text("spine-ir:"));

    let sidecar_dir = sidecar_dir_for_rollout_path(&rollout_path);
    let tree_text = std::fs::read_to_string(sidecar_dir.join("tree.jsonl"))
        .with_context(|| format!("read {}", sidecar_dir.join("tree.jsonl").display()))?;
    assert!(
        tree_text.contains("\"root_epoch_reset\""),
        "auto compact should persist root_epoch_reset: {tree_text}"
    );
    assert!(
        tree_text.contains("\"op\":\"open\"")
            && tree_text.contains("\"from_node\":\"2.1\"")
            && tree_text.contains("\"to_node\":\"2.1.1\""),
        "spine should remain mutable after auto root archive: {tree_text}"
    );
    let rollout_text = std::fs::read_to_string(&rollout_path)
        .with_context(|| format!("read {}", rollout_path.display()))?;
    assert_rollout_has_spine_compaction_checkpoint(&rollout_text, 1)?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_manual_compact_uses_native_text_and_archives_root_epoch() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-open"),
                ev_spine_transition_call(OPEN_CALL_ID, "open", OPEN_SUMMARY, None),
                ev_completed("resp-open"),
            ]),
            sse(vec![
                ev_response_created("resp-open-follow-up"),
                ev_assistant_message("msg-open-follow-up", "ready before manual compact"),
                ev_completed("resp-open-follow-up"),
            ]),
            sse(vec![
                ev_response_created("resp-manual-compact"),
                ev_assistant_message("msg-manual-compact", "manual native root summary"),
                ev_completed("resp-manual-compact"),
            ]),
            sse(vec![
                ev_response_created("resp-after-manual"),
                ev_spine_transition_call(
                    "post-manual-archive-open-call",
                    "open",
                    "post manual archive scope",
                    None,
                ),
                ev_completed("resp-after-manual"),
            ]),
            sse(vec![
                ev_response_created("resp-after-manual-follow-up"),
                ev_assistant_message("msg-after-manual-follow-up", "done after manual compact"),
                ev_completed("resp-after-manual-follow-up"),
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

    test.submit_turn("open spine before manual compact").await?;
    test.codex.submit(Op::Compact).await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
    test.submit_turn("continue after manual compact").await?;

    let requests = responses.requests();
    assert!(
        requests.len() >= 3,
        "expected at least open, manual compact, post-compact turn"
    );
    let compact_index = requests
        .iter()
        .position(|request| request.body_contains_text(SUMMARIZATION_PROMPT))
        .unwrap_or_else(|| {
            panic!("expected one request to contain the native compact prompt: {requests:?}")
        });
    let compact_request = &requests[compact_index];
    assert!(
        compact_request.body_contains_text(SUMMARIZATION_PROMPT),
        "manual spine compact should reuse the normal native compact prompt"
    );
    assert!(
        !compact_request.body_contains_text("Compact only target Spine node"),
        "manual root compact should not use the Spine suffix compact prompt"
    );
    assert!(
        !tool_names(compact_request)
            .iter()
            .any(|name| name == "spine"),
        "manual root compact should use native textual compact without Spine tool schema"
    );
    let post_compact_request = requests[compact_index + 1..]
        .iter()
        .find(|request| request.body_contains_text("manual native root summary"))
        .unwrap_or_else(|| {
            panic!("expected a post-compact request to contain the manual root summary")
        });
    assert!(
        !post_compact_request.body_contains_text("<spine_worklog")
            && post_compact_request.body_contains_text("## Spine Worklog")
            && post_compact_request.body_contains_text("manual native root summary"),
        "post-compact turn should see the native summary as root-epoch worklog"
    );

    let sidecar_dir = sidecar_dir_for_rollout_path(&rollout_path);
    let tree_text = std::fs::read_to_string(sidecar_dir.join("tree.jsonl"))
        .with_context(|| format!("read {}", sidecar_dir.join("tree.jsonl").display()))?;
    assert!(
        tree_text.contains("\"root_epoch_reset\""),
        "manual compact should persist root_epoch_reset: {tree_text}"
    );
    assert!(
        tree_text.contains("\"op\":\"open\"")
            && tree_text.contains("\"from_node\":\"2.1\"")
            && tree_text.contains("\"to_node\":\"2.1.1\""),
        "spine should remain mutable after manual root archive: {tree_text}"
    );
    let rollout_text = std::fs::read_to_string(&rollout_path)
        .with_context(|| format!("read {}", rollout_path.display()))?;
    assert_rollout_has_spine_compaction_checkpoint(&rollout_text, 1)?;

    Ok(())
}

fn assert_raw_mirror_has_raw_items_and_compact_metadata(
    sidecar_dir: &Path,
    expected_compact_metadata: usize,
) -> anyhow::Result<()> {
    let raw_mirror_path = sidecar_dir.join("raw/rollout.raw.jsonl");
    let raw_mirror_text = std::fs::read_to_string(&raw_mirror_path)
        .with_context(|| format!("read {}", raw_mirror_path.display()))?;
    let raw_mirror = parse_json_lines(&raw_mirror_text)?;
    let response_items = raw_mirror
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("response_item"))
        .count();
    let compact_metadata = raw_mirror
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("raw_mirror_event"))
        .count();

    assert!(
        response_items > 0,
        "raw mirror should contain raw response items: {raw_mirror_text}"
    );
    assert_eq!(
        compact_metadata, expected_compact_metadata,
        "raw mirror should record compact checkpoints only as metadata: {raw_mirror_text}"
    );
    assert!(
        raw_mirror
            .iter()
            .all(|item| item.get("type").and_then(Value::as_str) != Some("compacted")),
        "raw mirror must not store compact replacement history items: {raw_mirror_text}"
    );
    Ok(())
}

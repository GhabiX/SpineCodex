#![cfg(not(target_os = "windows"))]
#![allow(clippy::expect_used)]

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use codex_core::compact::SUMMARIZATION_PROMPT;
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
const CLOSE_SUMMARY: &str = "finish sibling scope";
const EXPECTED_SPINE_VIEW_INSTRUCTIONS: &str = r#"<spine_view>
Use Spine as your task plan and context manager. Completed scopes are folded into runtime-generated worklog IR, and later turns carry the visible Spine Tree, completed worklogs, and the current live suffix instead of every old raw message.
Use Spine effectively and efficiently.
At the start, use update_plan with spine_plantree to maintain one compact task tree draft for the current editable scope. This is planning only; it does not create Spine nodes or move the cursor.
Use update_plan's top-level plan for the current real Spine node's checklist; use spine_plantree.root.children for future planned child scopes, and put each future child scope's checklist in that scope's checkpoints.
Future PlanTree scopes may display as `~<predicted-id>` to distinguish planned nodes from real Spine nodes.
Default to staying in the current live node while it remains focused. Use update_plan to revise the current PlanTree when new evidence changes the task structure.
When your task structure or next work scope changes, promptly refresh the current spine_plantree with update_plan so the displayed PlanTree stays current.
When update_plan succeeds with a writable Spine tree, treat the returned `spine_tree` JSON as the authoritative updated tree for the next decision.
For non-trivial or multi-phase work, keep future planned scopes in `spine_plantree.root.children` rather than flattening them into the current node's top-level plan, and update them with `update_plan`; this manages planning only and does not create real Spine nodes.
Move Spine when a completed scope has accumulated substantial raw history and future work is likely to reuse its generated worklog IR:
- spine.open: enter a focused child scope that should inherit the parent goal but keep its own local context; it takes no arguments.
- spine.next: finish the current leaf and move to its next sibling.
- spine.close: finish the current leaf, close its non-root parent scope, and continue at the parent's next sibling. Root cannot be closed.
At root depth, use spine.next to finish the current root child and continue with its next sibling; use spine.close only from a nested scope when closing its parent and returning to the parent's next sibling.
For spine.next or spine.close, use summary as the short completion-time Spine Tree label. Use the optional instruction argument when the automatic compact pass should prioritize specific facts to preserve from the completed leaf or scope. Do not use summary or instruction with spine.open.
Use spine.tree to inspect the current node and Spine Tree without moving the cursor.
Do not move spine only because a new user message arrived, because you answered a short question, or because you updated progress within the same scope.
Do not create one node per shell command, checklist item, short reply, or conversation turn.
After spine.next from `1.1` to `1.2`, the runtime folds `1.1`'s raw trace into `nodes/1/1/worklog.md`; later context shows the Spine Tree plus `1.1` worklog, not `1.1` raw trace.
After spine.close from `1.1.2` to `1.2`, the runtime folds the completed `1.1` scope into `nodes/1/1/worklog.md`; child scopes that were already folded are carried through the Spine Tree/worklog IR, while raw child traces stay expandable out of band.
Runtime output may show `Base: <spine sidecar root>`; resolve sidecar-relative paths such as `nodes/.../worklog.md` against that Base, not against the workspace cwd.
After spine.next or spine.close, if unfinished work remains, use update_plan to refresh the current PlanTree from the handoff summary and current evidence.
Keep working in the current node while its raw details are still useful. When a coherent work scope is complete, fold it so later turns use its worklog instead of its raw trace.
Avoid tiny splits for individual commands, small observations, or conversation turns.
The runtime may warn when the current node grows large: around 50k raw tokens, then every additional 30k. Treat the warning as a cue to finish the current scope cleanly, then use spine.next or spine.close if the next work can rely on the worklog.
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
                ev_spine_transition_call(OPEN_CALL_ID, "open", OPEN_SUMMARY),
                ev_completed("resp-open"),
            ]),
            sse(vec![
                ev_response_created("resp-nested-open"),
                ev_spine_transition_call(NESTED_OPEN_CALL_ID, "open", NESTED_OPEN_SUMMARY),
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
                ev_spine_transition_call(NEXT_CALL_ID, "next", NEXT_SUMMARY),
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
                ev_spine_transition_call(CLOSE_CALL_ID, "close", CLOSE_SUMMARY),
                ev_function_call(
                    ROOT_SHELL_CALL_ID,
                    "shell_command",
                    &shell_args("printf 'root-spine\\n'"),
                ),
                ev_completed("resp-close"),
            ]),
            sse(vec![
                ev_response_created("resp-spine-close-compact"),
                ev_assistant_message("msg-spine-close-compact", "Compacted root findings."),
                ev_completed("resp-spine-close-compact"),
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
    assert_transition(&tree, "close", "1.1.1.2", "1.1.2", Some(CLOSE_SUMMARY));
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
    assert!(scope_worklog.contains("|-- [1.1.1.2] finished (nodes/1/1/1/2/worklog.md)"));
    let leaf_worklog = std::fs::read_to_string(sidecar_dir.join("nodes/1/1/1/1/worklog.md"))?;
    assert!(leaf_worklog.contains("spine:auto-compact-generated"));
    assert!(leaf_worklog.contains(&base_line));
    assert!(leaf_worklog.contains("Compacted child findings."));
    assert_compact_installed(&compact_index, "1.1.1.1", "next");
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
    assert_rollout_has_spine_compaction_checkpoint(&rollout_text, 2)?;
    assert_raw_mirror_has_raw_items_and_compact_metadata(&sidecar_dir)?;

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
                ev_spine_transition_call(OPEN_CALL_ID, "open", OPEN_SUMMARY),
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
                ev_spine_transition_call("post-archive-open-call", "open", "post archive scope"),
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
                ev_spine_transition_call(OPEN_CALL_ID, "open", OPEN_SUMMARY),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_suffix_compact_failure_does_not_retry_completed_sampling_request()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-open"),
                ev_spine_transition_call(OPEN_CALL_ID, "open", OPEN_SUMMARY),
                ev_completed("resp-open"),
            ]),
            sse(vec![
                ev_response_created("resp-open-follow-up"),
                ev_assistant_message("msg-open-follow-up", "open follow-up complete"),
                ev_completed("resp-open-follow-up"),
            ]),
            sse(vec![
                ev_response_created("resp-next"),
                ev_spine_transition_call(NEXT_CALL_ID, "next", NEXT_SUMMARY),
                ev_completed("resp-next"),
            ]),
            sse_failed(
                "resp-spine-compact-fail-1",
                "server_error",
                "temporary spine compact failure one",
            ),
            sse_failed(
                "resp-spine-compact-fail-2",
                "server_error",
                "temporary spine compact failure two",
            ),
        ],
    )
    .await;

    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config
            .features
            .enable(Feature::SpineTaskTree)
            .expect("enable spine task tree");
        config.model_provider.stream_max_retries = Some(1);
    });
    let test = builder.build(&server).await?;
    let rollout_path = test
        .session_configured
        .rollout_path
        .clone()
        .expect("session should expose rollout path");

    test.submit_turn("open spine before failing suffix compact")
        .await?;

    submit_turn_expect_spine_compact_error(&test, "trigger next with failing suffix compact")
        .await?;

    let requests = responses.requests();
    assert_eq!(
        requests.len(),
        5,
        "expected open, open follow-up, next, compact attempt, compact retry; failed compact must not replay the completed next sampling request"
    );
    assert!(
        requests[2].body_contains_text("trigger next with failing suffix compact"),
        "third request should be the original next sampling request"
    );
    assert!(
        requests[3].body_contains_text("Compact only target Spine node")
            && requests[4].body_contains_text("Compact only target Spine node"),
        "suffix compact should retry within the compact request boundary"
    );
    assert!(
        !requests[3].body_contains_text(SUMMARIZATION_PROMPT)
            && !requests[4].body_contains_text(SUMMARIZATION_PROMPT),
        "suffix compact should use the Spine factual worklog prompt, not the normal compact prompt"
    );
    assert!(
        requests
            .iter()
            .enumerate()
            .filter(|(_, request)| {
                request.body_contains_text("trigger next with failing suffix compact")
                    && !request.body_contains_text("Compact only target Spine node")
            })
            .map(|(index, _)| index)
            .eq([2]),
        "failed compact must not replay the completed next sampling request"
    );

    let sidecar_dir = sidecar_dir_for_rollout_path(&rollout_path);
    let compact_index_text = std::fs::read_to_string(sidecar_dir.join("compact.index.jsonl"))
        .with_context(|| format!("read {}", sidecar_dir.join("compact.index.jsonl").display()))?;
    let compact_index = parse_json_lines(&compact_index_text)?;
    assert_compact_failed(&compact_index, "1.1.1", "next");

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

    let mut saw_spine_tree_update = false;
    let mut saw_plan_update = false;
    wait_for_event(&test.codex, |event| match event {
        EventMsg::SpineTreeUpdate(update) => {
            let Some(plan) = update
                .nodes
                .iter()
                .find(|node| node.node_id == "1.1.1.1")
                .and_then(|node| node.plan.as_ref())
            else {
                return false;
            };
            saw_spine_tree_update = true;
            assert_eq!(plan.revision, 1);
            assert_eq!(plan.items.len(), 2);
            assert_eq!(plan.items[0].stable_task_id, "step-1");
            assert_eq!(plan.items[0].step, "Exercise child node");
            false
        }
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
    assert!(saw_spine_tree_update, "expected SpineTreeUpdate event");
    assert!(saw_plan_update, "expected normal PlanUpdate event");

    Ok(turn_id)
}

async fn submit_turn_expect_spine_compact_error(
    test: &core_test_support::test_codex::TestCodex,
    prompt: &str,
) -> anyhow::Result<()> {
    let cwd_path = test.cwd.path().to_path_buf();
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, cwd_path.as_path());
    let session_model = test.session_configured.model.clone();

    test.codex
        .submit(Op::UserTurn {
            environments: None,
            items: vec![UserInput::Text {
                text: prompt.into(),
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

    let reconnect_message = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::StreamError(stream_error) => Some(stream_error.message.clone()),
        _ => None,
    })
    .await;
    assert_eq!(
        reconnect_message, "Reconnecting... 1/1",
        "spine suffix compact should surface retry status like Codex auto compact"
    );

    let error_message = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::Error(error) if error.message.contains("Error running Spine compact task") => {
            Some(error.message.clone())
        }
        _ => None,
    })
    .await;
    assert!(
        error_message.contains("Error running Spine compact task"),
        "expected Spine compact task error, got {error_message}"
    );
    Ok(())
}

fn shell_args(command: &str) -> String {
    json!({
        "command": command,
        "timeout_ms": 2_000,
    })
    .to_string()
}

fn ev_spine_transition_call(call_id: &str, name: &str, summary: &str) -> Value {
    let arguments = if name == "open" {
        "{}".to_string()
    } else {
        spine_args(summary)
    };
    ev_function_call_with_namespace(call_id, "spine", name, &arguments)
}

fn spine_args(summary: &str) -> String {
    json!({
        "summary": summary,
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
        "spine_plantree": {
            "root": {
                "summary": "Nested implementation scope",
                "children": [
                    {
                        "summary": "Future child scope",
                        "checkpoints": [
                            {"task": "Exercise future child scope", "status": "pending"}
                        ]
                    }
                ]
            }
        }
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
    let output = function_output_text(requests, call_id);
    assert!(
        output.contains(expected),
        "expected output for {call_id} to contain {expected:?}, got {output:?}"
    );
}

fn function_output_json(requests: &[ResponsesRequest], call_id: &str) -> anyhow::Result<Value> {
    let output = function_output_text(requests, call_id);
    serde_json::from_str(&output).with_context(|| format!("parse function output for {call_id}"))
}

fn function_output_text(requests: &[ResponsesRequest], call_id: &str) -> String {
    requests
        .iter()
        .find_map(|request| request.function_call_output_text(call_id))
        .unwrap_or_else(|| {
            let available = requests
                .iter()
                .flat_map(|request| request.input())
                .filter(|item| {
                    item.get("type").and_then(Value::as_str) == Some("function_call_output")
                })
                .filter_map(|item| {
                    item.get("call_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>();
            panic!("function_call_output missing for {call_id}; available outputs: {available:?}")
        })
}

fn assert_spine_initialized(tree: &[Value]) {
    let event = tree
        .iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some("spine_initialized"))
        .unwrap_or_else(|| panic!("tree should contain spine_initialized event: {tree:?}"));
    let state = event
        .get("state")
        .unwrap_or_else(|| panic!("spine_initialized should contain state: {event:?}"));
    assert_eq!(state.get("cursor").and_then(Value::as_str), Some("1.1"));
    let nodes = state
        .get("nodes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("spine_initialized state should contain nodes: {state:?}"));
    assert!(
        nodes.iter().any(|node| {
            node.get("node_id").and_then(Value::as_str) == Some("1")
                && node.get("parent_id").is_some_and(Value::is_null)
                && node.get("raw_start_ordinal").and_then(Value::as_u64) == Some(0)
                && node.get("status").and_then(Value::as_str) == Some("opened")
                && node.get("summary").is_some_and(Value::is_null)
        }),
        "state should contain root epoch 1: {state:?}"
    );
    assert!(
        nodes.iter().any(|node| {
            node.get("node_id").and_then(Value::as_str) == Some("1.1")
                && node.get("parent_id").and_then(Value::as_str) == Some("1")
                && node.get("raw_start_ordinal").and_then(Value::as_u64) == Some(0)
                && node.get("status").and_then(Value::as_str) == Some("live")
                && node.get("summary").is_some_and(Value::is_null)
        }),
        "state should contain initial live leaf 1.1: {state:?}"
    );
}

fn assert_transition(
    tree: &[Value],
    op: &str,
    from_node: &str,
    to_node: &str,
    summary: Option<&str>,
) {
    let event = tree
        .iter()
        .find(|event| {
            event.get("type").and_then(Value::as_str) == Some("transition_applied")
                && event.get("op").and_then(Value::as_str) == Some(op)
                && event.get("from_node").and_then(Value::as_str) == Some(from_node)
                && event.get("to_node").and_then(Value::as_str) == Some(to_node)
        })
        .unwrap_or_else(|| panic!("missing {op} transition {from_node} -> {to_node}: {tree:?}"));

    assert_eq!(event.get("summary").and_then(Value::as_str), summary);
}

fn assert_plan_updated(tree: &[Value], node_id: &str, revision: u64, source_turn_id: &str) -> u64 {
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
    event
        .get("seq")
        .and_then(Value::as_u64)
        .expect("task_plan_updated should have seq")
}

fn assert_transition_committed(index: &[Value], call_id: &str, from_node: &str, to_node: &str) {
    assert!(
        index.iter().any(|event| {
            let call_start = event.get("call_start_ordinal").and_then(Value::as_u64);
            let boundary_end = event.get("boundary_end").and_then(Value::as_u64);
            event.get("type").and_then(Value::as_str) == Some("transition_committed")
                && event.get("call_id").and_then(Value::as_str) == Some(call_id)
                && event.get("from_node").and_then(Value::as_str) == Some(from_node)
                && event.get("to_node").and_then(Value::as_str) == Some(to_node)
                && call_start
                    .zip(boundary_end)
                    .is_some_and(|(start, end)| start < end)
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

fn assert_compact_installed(index: &[Value], node_id: &str, op: &str) {
    assert!(
        index.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("compact_started")
                && event.get("node_id").and_then(Value::as_str) == Some(node_id)
                && event.get("op").and_then(Value::as_str) == Some(op)
        }),
        "compact index should contain start for {node_id} {op}: {index:?}"
    );
    assert!(
        index.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("compact_installed")
                && event.get("node_id").and_then(Value::as_str) == Some(node_id)
                && event.get("op").and_then(Value::as_str) == Some(op)
                && event
                    .get("replacement_history_len")
                    .and_then(Value::as_u64)
                    .is_some_and(|len| len > 0)
        }),
        "compact index should contain install for {node_id} {op}: {index:?}"
    );
}

fn assert_compact_failed(index: &[Value], node_id: &str, op: &str) {
    assert!(
        index.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("compact_started")
                && event.get("node_id").and_then(Value::as_str) == Some(node_id)
                && event.get("op").and_then(Value::as_str) == Some(op)
        }),
        "compact index should contain start for failed {node_id} {op}: {index:?}"
    );
    assert!(
        index.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("compact_failed")
                && event.get("node_id").and_then(Value::as_str) == Some(node_id)
                && event.get("op").and_then(Value::as_str) == Some(op)
                && event
                    .get("error")
                    .and_then(Value::as_str)
                    .is_some_and(|error| error.contains("temporary spine compact failure two"))
        }),
        "compact index should contain terminal failure for {node_id} {op}: {index:?}"
    );
}

fn assert_rollout_has_spine_compaction_checkpoint(
    rollout_text: &str,
    expected_count: usize,
) -> anyhow::Result<()> {
    let mut compacted = 0;
    for line in rollout_text.lines().filter(|line| !line.trim().is_empty()) {
        let entry: RolloutLine = serde_json::from_str(line).context("parse rollout line")?;
        if let RolloutItem::Compacted(item) = entry.item {
            compacted += 1;
            assert!(
                item.replacement_history
                    .as_ref()
                    .is_some_and(|history| !history.is_empty()),
                "spine compact checkpoint should include replacement_history: {line}"
            );
            assert!(
                item.message.contains("Spine compacted"),
                "unexpected spine compact message: {line}"
            );
        } else if matches!(
            entry.item,
            RolloutItem::ResponseItem(ResponseItem::Compaction { .. })
                | RolloutItem::ResponseItem(ResponseItem::ContextCompaction { .. })
        ) {
            panic!(
                "spine compact should use rollout checkpoint items, not raw compact response items: {line}"
            );
        }
    }
    assert_eq!(
        compacted, expected_count,
        "unexpected spine compact checkpoint count"
    );
    Ok(())
}

fn assert_raw_mirror_has_raw_items_and_compact_metadata(sidecar_dir: &Path) -> anyhow::Result<()> {
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
        compact_metadata, 2,
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

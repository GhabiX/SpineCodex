//! Integration tests that cover compacting, resuming, and forking conversations.
//!
//! Each test sets up a mocked SSE conversation and drives the conversation through
//! a specific sequence of operations. After every operation we capture the
//! request payload that Codex would send to the model and assert that the
//! model-visible history matches the expected sequence of messages.

use super::compact::FIRST_REPLY;
use super::compact::SUMMARY_TEXT;
use anyhow::Context;
use anyhow::Result;
use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_core::TurnInputRequest;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_core::config::Config;
use codex_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_history::CodexHarnessMetadata;
use codex_history::RolloutItem;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SpineTreeUpdateEvent;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::user_input::UserInput;
use core_test_support::ThreadIdle;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::test_codex::spine_test_codex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::MockServer;

const AFTER_SECOND_RESUME: &str = "AFTER_SECOND_RESUME";
const CHECKPOINT_METADATA_KEY: &str = "replacement_history_metadata";

fn network_disabled() -> bool {
    std::env::var(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok()
}

fn body_contains_text(body: &str, text: &str) -> bool {
    body.contains(&json_fragment(text))
}

fn json_fragment(text: &str) -> String {
    serde_json::to_string(text)
        .expect("serialize text to JSON")
        .trim_matches('"')
        .to_string()
}

fn normalize_line_endings_str(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

fn response_message_contains_text(item: &ResponseItem, expected: &str) -> bool {
    matches!(
        item,
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && content.iter().any(|item| {
                    matches!(item, ContentItem::InputText { text } if text == expected)
                })
    )
}

fn seed_first_checkpoint_harness_metadata(path: &Path, retained_text: &str) -> Result<()> {
    let mut lines = std::fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(codex_rollout::parse_rollout_line)
        .collect::<Result<Vec<_>, _>>()?;
    let replacement_history = lines
        .iter_mut()
        .find_map(|line| match &mut line.item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_mut(),
            _ => None,
        })
        .context("first compacted checkpoint missing replacement history")?;
    replacement_history
        .iter()
        .find(|envelope| response_message_contains_text(&envelope.item, retained_text))
        .context("retained user message missing from first compacted checkpoint")?;
    for envelope in replacement_history {
        envelope.metadata = Some(CodexHarnessMetadata::default());
    }

    let rewritten = lines
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    std::fs::write(path, format!("{rewritten}\n"))?;
    Ok(())
}

fn assert_latest_checkpoint_retains_harness_metadata(
    path: &Path,
    retained_text: &str,
) -> Result<()> {
    let replacement_history = std::fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(codex_rollout::parse_rollout_line)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .rev()
        .find_map(|line| match line.item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history,
            _ => None,
        })
        .context("latest compacted checkpoint missing replacement history")?;
    assert!(
        replacement_history.iter().any(|envelope| {
            response_message_contains_text(&envelope.item, retained_text)
                && envelope.metadata.is_some()
        }),
        "latest compacted checkpoint should retain aligned harness metadata on {retained_text:?}"
    );
    Ok(())
}

fn extract_summary_user_text(request: &Value, summary_text: &str) -> String {
    json_message_input_texts(request, "user")
        .into_iter()
        .find(|text| text.contains(summary_text))
        .expect("expected summary message")
}

fn json_message_input_texts(request: &Value, role: &str) -> Vec<String> {
    request
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some(role)
        })
        .filter_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|entry| entry.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn normalize_compact_prompts(requests: &mut [Value]) {
    let normalized_summary_prompt = normalize_line_endings_str(SUMMARIZATION_PROMPT);
    for request in requests {
        if let Some(input) = request.get_mut("input").and_then(Value::as_array_mut) {
            input.retain(|item| {
                if item.get("type").and_then(Value::as_str) != Some("message")
                    || item.get("role").and_then(Value::as_str) != Some("user")
                {
                    return true;
                }
                let Some(content) = item.get("content").and_then(Value::as_array) else {
                    return false;
                };
                let Some(first) = content.first() else {
                    return false;
                };
                let text = first
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let normalized_text = normalize_line_endings_str(text);
                !(text.is_empty() || normalized_text == normalized_summary_prompt)
            });
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Scenario: compact an initial conversation, resume it, fork one turn back, and
/// ensure the model-visible history matches expectations at each request.
async fn compact_resume_and_fork_preserve_model_history_view() {
    if network_disabled() {
        println!("Skipping test because network is disabled in this sandbox");
        return;
    }

    // 1. Arrange mocked SSE responses for the initial compact/resume/fork flow.
    let server = MockServer::start().await;
    let request_log = mount_initial_flow(&server).await;
    let expected_model = "gpt-5.4";
    // 2. Start a new conversation and drive it through the compact/resume/fork steps.
    let (_home, config, manager, base) =
        start_test_conversation(&server, Some(expected_model)).await;

    user_turn(&base, "hello world").await;
    compact_conversation(&base).await;
    user_turn(&base, "AFTER_COMPACT").await;
    let base_path = fetch_conversation_path(&base);
    assert!(
        base_path.exists(),
        "compact+resume test expects base path {base_path:?} to exist",
    );

    shutdown_conversation(&base).await;
    let resumed = resume_conversation(&manager, &config, base_path).await;
    user_turn(&resumed, "AFTER_RESUME").await;
    let resumed_path = fetch_conversation_path(&resumed);
    assert!(
        resumed_path.exists(),
        "compact+resume test expects resumed path {resumed_path:?} to exist",
    );

    let forked = fork_thread(&manager, &config, resumed_path, /*nth_user_message*/ 2).await;
    user_turn(&forked, "AFTER_FORK").await;

    // 3. Capture the requests to the model and validate the history slices.
    let mut requests = gather_request_bodies(&request_log);
    normalize_compact_prompts(&mut requests);

    // input after compact is a prefix of input after resume/fork
    let input_after_compact = json!(requests[requests.len() - 3]["input"]);
    let input_after_resume = json!(requests[requests.len() - 2]["input"]);
    let mut input_after_fork = json!(requests[requests.len() - 1]["input"]);
    // The request prefix has a thread-scoped ID; retained history IDs stay unchanged.
    input_after_fork[0]["id"] = input_after_compact[0]["id"].clone();

    let compact_arr = input_after_compact
        .as_array()
        .expect("input after compact should be an array");
    let resume_arr = input_after_resume
        .as_array()
        .expect("input after resume should be an array");
    let fork_arr = input_after_fork
        .as_array()
        .expect("input after fork should be an array");

    assert!(
        compact_arr.len() <= resume_arr.len(),
        "after-resume input should have at least as many items as after-compact",
    );
    assert_eq!(compact_arr.as_slice(), &resume_arr[..compact_arr.len()]);

    assert!(
        compact_arr.len() <= fork_arr.len(),
        "after-fork input should have at least as many items as after-compact",
    );
    assert_eq!(
        &compact_arr.as_slice()[..compact_arr.len()],
        &fork_arr[..compact_arr.len()]
    );

    let first_request_user_texts = json_message_input_texts(&requests[0], "user");
    let first_turn_user_index = first_request_user_texts
        .len()
        .checked_sub(1)
        .expect("first turn request missing user messages");
    assert_eq!(
        first_request_user_texts[first_turn_user_index],
        "hello world"
    );
    let seeded_user_prefix = &first_request_user_texts[..first_turn_user_index];
    let summary_after_compact = extract_summary_user_text(&requests[2], SUMMARY_TEXT);
    let summary_after_resume = extract_summary_user_text(&requests[3], SUMMARY_TEXT);
    let summary_after_fork = extract_summary_user_text(&requests[4], SUMMARY_TEXT);
    let mut expected_after_compact_user_texts = seeded_user_prefix.to_vec();
    expected_after_compact_user_texts.extend(["hello world".to_string(), summary_after_compact]);
    expected_after_compact_user_texts.push("AFTER_COMPACT".to_string());
    assert_eq!(
        json_message_input_texts(&requests[2], "user"),
        expected_after_compact_user_texts
    );

    let mut expected_after_resume_user_texts = seeded_user_prefix.to_vec();
    expected_after_resume_user_texts.extend(["hello world".to_string(), summary_after_resume]);
    expected_after_resume_user_texts.push("AFTER_COMPACT".to_string());
    let after_resume_user_texts = json_message_input_texts(&requests[3], "user");
    let (after_resume_last, after_resume_prefix) = after_resume_user_texts
        .split_last()
        .expect("after-resume request missing user messages");
    assert_eq!(after_resume_last, "AFTER_RESUME");
    assert!(
        after_resume_prefix.starts_with(&expected_after_resume_user_texts),
        "after-resume user texts should preserve compacted history prefix"
    );
    let after_resume_seeded_suffix = &after_resume_prefix[expected_after_resume_user_texts.len()..];
    if seeded_user_prefix.is_empty() {
        assert!(
            after_resume_seeded_suffix.is_empty(),
            "after-resume request should not append unexpected user prefix items"
        );
    } else {
        let mut chunks = after_resume_seeded_suffix.chunks_exact(seeded_user_prefix.len());
        assert!(
            chunks.remainder().is_empty(),
            "after-resume suffix should be whole seeded-prefix repeats"
        );
        for chunk in &mut chunks {
            assert_eq!(chunk, seeded_user_prefix);
        }
    }

    let after_fork_user_texts = json_message_input_texts(&requests[4], "user");
    let mut expected_after_fork_history_prefix = seeded_user_prefix.to_vec();
    expected_after_fork_history_prefix.extend(["hello world".to_string(), summary_after_fork]);
    expected_after_fork_history_prefix.push("AFTER_COMPACT".to_string());
    let (after_fork_last, after_fork_prefix) = after_fork_user_texts
        .split_last()
        .expect("after-fork request missing user messages");
    assert_eq!(after_fork_last, "AFTER_FORK");
    assert!(
        after_fork_prefix.starts_with(&expected_after_fork_history_prefix),
        "after-fork user texts should preserve compacted user history prefix"
    );
    let after_fork_seeded_suffix = &after_fork_prefix[expected_after_fork_history_prefix.len()..];
    if seeded_user_prefix.is_empty() {
        assert!(
            after_fork_seeded_suffix.is_empty(),
            "after-fork request should not append unexpected user prefix items"
        );
    } else {
        let mut chunks = after_fork_seeded_suffix.chunks_exact(seeded_user_prefix.len());
        assert!(
            chunks.remainder().is_empty(),
            "after-fork suffix should be whole seeded-prefix repeats"
        );
        for chunk in &mut chunks {
            assert_eq!(chunk, seeded_user_prefix);
        }
    }
    assert_eq!(requests.len(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_snapshot_replays_after_compact_resume_and_fork() {
    let server = MockServer::start().await;
    let _request_log = mount_spine_snapshot_flow(&server).await;
    let (_home, config, manager, base) =
        start_spine_test_conversation(&server, /*model*/ None).await;

    user_turn(&base, "hello world").await;
    base.submit(Op::Compact)
        .await
        .expect("submit compact conversation");
    let compact_snapshot = wait_for_spine_snapshot(&base, "2").await;
    let warning_event = wait_for_event(&base, |event| {
        matches!(
            event,
            EventMsg::Warning(WarningEvent { message }) if message == COMPACT_WARNING_MESSAGE
        )
    })
    .await;
    assert!(matches!(warning_event, EventMsg::Warning(_)));
    wait_for_event(&base, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    assert_eq!(compact_snapshot.active_node_id, "2");
    let compact_pressure = compact_snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == compact_snapshot.active_node_id)
        .and_then(|node| node.context_pressure.as_ref())
        .expect("compact snapshot should publish active context pressure");
    assert_eq!(compact_pressure.current_input_tokens, Some(900));
    assert_eq!(compact_pressure.open_input_tokens, None);
    assert!(
        compact_snapshot
            .nodes
            .iter()
            .any(|node| node.node_id == "1")
    );

    let base_path = fetch_conversation_path(&base);
    shutdown_conversation(&base).await;

    let resumed = resume_conversation(&manager, &config, base_path).await;
    let resumed_snapshot = wait_for_spine_snapshot(&resumed, "2").await;
    assert_eq!(
        resumed_snapshot.active_node_id,
        compact_snapshot.active_node_id
    );
    assert_eq!(resumed_snapshot.nodes, compact_snapshot.nodes);
    let resumed_path = fetch_conversation_path(&resumed);
    user_turn(&resumed, "AFTER_RESUME").await;

    let forked = fork_thread(&manager, &config, resumed_path, /*nth_user_message*/ 2).await;
    let forked_snapshot = wait_for_spine_snapshot(&forked, "2").await;
    assert_eq!(
        forked_snapshot.active_node_id,
        resumed_snapshot.active_node_id
    );
    assert_eq!(forked_snapshot.nodes, resumed_snapshot.nodes);
    user_turn(&forked, "AFTER_FORK").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_fork_before_later_prompt_replays_the_truncated_tree() {
    let server = MockServer::start().await;
    let request_log = mount_spine_truncated_fork_flow(&server).await;
    let (_home, config, manager, base) =
        start_spine_test_conversation(&server, /*model*/ None).await;

    let retained_updates = user_turn_with_spine_updates(&base, "open retained task").await;
    let retained_snapshot = retained_updates
        .iter()
        .rev()
        .find(|snapshot| snapshot.active_node_id == "1.1")
        .expect("first turn should open the retained node")
        .clone();

    let discarded_updates = user_turn_with_spine_updates(&base, "open discarded task").await;
    let discarded_snapshot = discarded_updates
        .iter()
        .rev()
        .find(|snapshot| snapshot.active_node_id == "1.1.1")
        .expect("second turn should open the node that rollback discards");
    assert!(
        discarded_snapshot
            .nodes
            .iter()
            .any(|node| node.node_id == "1.1.1")
    );

    let base_path = fetch_conversation_path(&base);
    shutdown_conversation(&base).await;
    let forked = fork_thread(&manager, &config, base_path, /*nth_user_message*/ 1).await;
    let forked_snapshot = wait_for_spine_snapshot(&forked, "1.1").await;

    assert_eq!(forked_snapshot.nodes, retained_snapshot.nodes);
    assert!(
        !forked_snapshot
            .nodes
            .iter()
            .any(|node| node.node_id == "1.1.1")
    );
    user_turn(&forked, "AFTER_FORK").await;
    let requests = request_log.requests();
    let forked_request = requests
        .last()
        .expect("fork follow-up should have a captured Responses request");
    let forked_request_body = forked_request.body_json().to_string();
    assert!(forked_request_body.contains("open retained task"));
    assert!(!forked_request_body.contains("open discarded task"));
    assert!(!forked_request_body.contains("1.1.1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paginated_prompt_revert_publishes_retained_and_empty_spine_tree() -> Result<()> {
    use codex_history::InitialHistory;
    use codex_history::ResumedHistory;
    use codex_protocol::protocol::ThreadHistoryMode;
    use codex_thread_store::LoadThreadHistoryParams;
    use codex_thread_store::RevertThreadParams;

    let server = MockServer::start().await;
    let result: Result<()> = async {
        let request_log = mount_spine_truncated_fork_flow(&server).await;
        let mut extensions = ExtensionRegistryBuilder::new();
        extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
        let base_url = format!("{}/v1", server.uri());
        let test = Box::pin(
            spine_test_codex()
                .with_history_mode(ThreadHistoryMode::Paginated)
                .with_extensions(Arc::new(extensions.build()))
                .with_config(move |config| {
                    config.model_provider.name = "Non-OpenAI Model provider".to_string();
                    config.model_provider.base_url = Some(base_url);
                })
                .build(&server),
        )
        .await?;
        let retained = user_turn_with_spine_updates(&test.codex, "open retained task")
            .await
            .into_iter()
            .find(|snapshot| snapshot.active_node_id == "1.1")
            .expect("first turn opens retained task");
        ThreadIdle::wait(&test.codex).await;
        let discarded = user_turn_with_spine_updates(&test.codex, "open discarded task")
            .await
            .into_iter()
            .find(|snapshot| snapshot.active_node_id == "1.1.1")
            .expect("second turn opens discarded task");
        assert!(discarded.nodes.len() > retained.nodes.len());
        ThreadIdle::wait(&test.codex).await;
        let thread_id = test.session_configured.thread_id;
        let store = codex_core::thread_store_from_config(
            &test.config,
            codex_core::init_state_db(&test.config).await,
        );
        test.codex.flush_rollout().await?;
        let saved = store
            .load_complete_history(LoadThreadHistoryParams {
                thread_id,
                include_archived: false,
            })
            .await?;
        let turns = saved
            .items
            .iter()
            .filter_map(|item| match item {
                RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => Some(event.turn_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(turns.len(), 2);
        let mut live = Arc::clone(&test.codex);
        for (before_turn_id, expected_active) in
            [(turns[1].clone(), "1.1"), (turns[0].clone(), "1")]
        {
            shutdown_conversation(&live).await;
            test.thread_manager.remove_thread(&thread_id).await;
            store
                .revert_thread(RevertThreadParams {
                    thread_id,
                    before_turn_id,
                    multi_agent_version: live.multi_agent_version(),
                })
                .await?;
            let params = LoadThreadHistoryParams {
                thread_id,
                include_archived: false,
            };
            let model_context = store.load_latest_model_context(params.clone()).await?;
            let complete = store.load_complete_history(params).await?;
            let resumed = Box::pin(test.thread_manager.resume_thread_with_history(
                test.config.clone(),
                InitialHistory::Resumed(ResumedHistory {
                    history_revision: model_context.revision,
                    spine_history: Some(Arc::new(complete.items)),
                    conversation_id: thread_id,
                    history: Arc::new(model_context.items),
                    rollout_path: None,
                }),
                codex_core::test_support::auth_manager_from_auth(
                    codex_login::CodexAuth::from_api_key("dummy"),
                ),
                /*parent_trace*/ None,
                ClientMcpExtensions::default(),
            ))
            .await?;
            assert_eq!(resumed.thread_id, thread_id);
            live = resumed.thread;
            let replayed = wait_for_spine_snapshot(&live, expected_active).await;
            if expected_active == "1.1" {
                assert_eq!(replayed.nodes, retained.nodes);
            } else {
                assert_eq!(replayed.nodes.len(), 1);
                assert_eq!(replayed.nodes[0].node_id, "1");
                assert_eq!(replayed.snapshot_seq, 0);
            }
            assert!(!replayed.nodes.iter().any(|node| node.node_id == "1.1.1"));
            if expected_active == "1.1" {
                user_turn(&live, "confirm rollback state").await;
                let requests = request_log.requests();
                let request = requests
                    .last()
                    .expect("post-revert model request")
                    .body_json();
                assert!(request.to_string().contains("open retained task"));
                assert!(!request.to_string().contains("open discarded task"));
                assert!(!request.to_string().contains("1.1.1"));
            }
        }
        shutdown_conversation(&live).await;
        Ok(())
    }
    .await;
    result.expect("native paginated revert/reload must succeed");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_replay_rollback_followup_trims_context_updates_and_spine_tree() -> Result<()> {
    use codex_protocol::config_types::CollaborationMode;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::config_types::Settings;
    use codex_protocol::protocol::ThreadSettingsOverrides;
    use std::io::Write;

    const DISCARDED_SETTINGS: &str = "DISCARDED_DEVELOPER_UPDATE_R036_5c91";
    let server = MockServer::start().await;
    let request_log = mount_spine_truncated_fork_flow(&server).await;
    let (_home, config, manager, base) =
        start_spine_test_conversation(&server, /*model*/ None).await;
    let retained_updates = user_turn_with_spine_updates(&base, "open retained task").await;
    let retained = retained_updates
        .iter()
        .rev()
        .find(|snapshot| snapshot.active_node_id == "1.1")
        .expect("first turn opens retained task")
        .clone();
    ThreadIdle::wait(&base).await;
    base.start_or_steer_turn(
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: "open discarded task".to_string(),
            text_elements: Vec::new(),
        }])
        .with_thread_settings(ThreadSettingsOverrides {
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Default,
                settings: Settings {
                    model: config.model.clone().expect("configured model"),
                    reasoning_effort: None,
                    developer_instructions: Some(DISCARDED_SETTINGS.to_string()),
                },
            }),
            ..Default::default()
        }),
    )
    .await?;
    collect_spine_updates_until_turn_complete(&base).await;
    ThreadIdle::wait(&base).await;
    let before = request_log.requests();
    assert!(
        before
            .last()
            .expect("discarded turn request")
            .body_json()
            .to_string()
            .contains(DISCARDED_SETTINGS),
        "fixture must actually send the soon-to-be-retracted developer update"
    );
    let path = fetch_conversation_path(&base);
    shutdown_conversation(&base).await;

    // Rollback is now a durable history input, not a live session operation.
    // Use the same saved marker consumed by native recovery/prompt editing.
    let marker = json!({
        "timestamp": "2026-10-14T00:00:00Z",
        "type": "event_msg",
        "payload": {"type": "thread_rolled_back", "num_turns": 1},
    });
    codex_rollout::parse_rollout_line(&marker.to_string())?;
    let mut saved = std::fs::OpenOptions::new().append(true).open(&path)?;
    writeln!(saved, "{marker}")?;
    drop(saved);
    let resumed = resume_conversation(&manager, &config, path).await;
    let restored = wait_for_spine_snapshot(&resumed, "1.1").await;
    assert_eq!(restored.nodes, retained.nodes);
    // Thread settings survive native history edits. Reset the live collaboration setting
    // before checking that the removed historical update is not reintroduced by replay.
    resumed
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "AFTER_NATIVE_REPLAY".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Default,
                    settings: Settings {
                        model: config.model.clone().expect("configured model"),
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            }),
        )
        .await?;
    collect_spine_updates_until_turn_complete(&resumed).await;
    let requests = request_log.requests();
    let request = requests
        .last()
        .expect("resume follow-up request")
        .body_json();
    assert_eq!(
        json_message_input_texts(&request, "user")
            .into_iter()
            .filter(|text| !text.starts_with("<environment_context>"))
            .collect::<Vec<_>>(),
        vec![
            "[U1]\nopen retained task".to_string(),
            "[U2]\nAFTER_NATIVE_REPLAY".to_string()
        ]
    );
    assert!(
        !request.to_string().contains(DISCARDED_SETTINGS),
        "discarded developer setting survived native replay: {request:#}"
    );
    assert!(!request.to_string().contains("discarded task"));
    assert!(!request.to_string().contains("1.1.1"));
    shutdown_conversation(&resumed).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_pressure_starts_a_new_epoch_and_replays_after_compact() {
    let server = MockServer::start().await;
    let _request_log = mount_spine_pressure_epoch_flow(&server).await;
    let (_home, config, manager, base) =
        start_spine_test_conversation(&server, /*model*/ None).await;

    let initial_updates = user_turn_with_spine_updates(&base, "measure initial pressure").await;
    let initial_pressure_updates = initial_updates
        .iter()
        .filter_map(|snapshot| active_pressure(snapshot, "1"))
        .filter(|pressure| pressure.current_input_tokens == Some(450))
        .collect::<Vec<_>>();
    assert_eq!(initial_pressure_updates.len(), 1);
    let initial_pressure = initial_pressure_updates[0];
    assert_eq!(initial_pressure.open_input_tokens, Some(450));
    assert_eq!(initial_pressure.context_tokens, Some(0));
    assert_eq!(initial_pressure.problem, None);

    base.submit(Op::Compact)
        .await
        .expect("submit compact conversation");
    let compact_updates = collect_spine_updates_until_turn_complete(&base).await;
    let compact_epoch_updates = compact_updates
        .iter()
        .filter(|snapshot| snapshot.active_node_id == "2")
        .collect::<Vec<_>>();
    let compact_snapshot = compact_epoch_updates
        .last()
        .copied()
        .expect("compact should publish the replacement epoch");
    let compact_pressure = active_pressure(compact_snapshot, "2")
        .expect("compact replacement epoch should retain confirmed pressure");
    assert_eq!(compact_pressure.current_input_tokens, Some(900));
    assert_eq!(compact_pressure.open_input_tokens, None);
    assert!(
        compact_epoch_updates
            .iter()
            .all(|snapshot| active_pressure(snapshot, "2") == Some(compact_pressure)),
        "duplicate compact notifications must project one canonical pressure value"
    );
    assert_eq!(
        compact_snapshot
            .nodes
            .iter()
            .find(|node| node.node_id == "1")
            .expect("archived epoch should remain inspectable")
            .context_pressure,
        None,
        "the archived epoch must not retain live pressure"
    );

    let base_path = fetch_conversation_path(&base);
    shutdown_conversation(&base).await;
    let resumed = resume_conversation(&manager, &config, base_path).await;
    let resumed_snapshot = wait_for_spine_snapshot(&resumed, "2").await;
    assert_eq!(resumed_snapshot.nodes, compact_snapshot.nodes);

    let resumed_updates = user_turn_with_spine_updates(&resumed, "measure resumed pressure").await;
    let resumed_pressure_updates = resumed_updates
        .iter()
        .filter_map(|snapshot| active_pressure(snapshot, "2"))
        .filter(|pressure| pressure.current_input_tokens == Some(120))
        .collect::<Vec<_>>();
    assert_eq!(resumed_pressure_updates.len(), 1);
    let resumed_pressure = resumed_pressure_updates[0];
    assert_eq!(resumed_pressure.open_input_tokens, None);
    assert_eq!(resumed_pressure.context_tokens, None);
    assert_eq!(
        resumed_pressure.problem,
        Some(
            codex_protocol::spine_tree::SpineNodeContextPressureProblem::MissingOpenContextBaseline
        )
    );

    let steady_updates = user_turn_with_spine_updates(&resumed, "measure steady pressure").await;
    let steady_pressure_updates = steady_updates
        .iter()
        .filter_map(|snapshot| active_pressure(snapshot, "2"))
        .filter(|pressure| pressure.current_input_tokens == Some(220))
        .collect::<Vec<_>>();
    assert_eq!(steady_pressure_updates.len(), 1);
    let steady_pressure = steady_pressure_updates[0];
    assert_eq!(steady_pressure.open_input_tokens, None);
    assert_eq!(steady_pressure.context_tokens, None);
    assert_eq!(
        steady_pressure.problem,
        Some(
            codex_protocol::spine_tree::SpineNodeContextPressureProblem::MissingOpenContextBaseline
        )
    );

    let opened_updates = user_turn_with_spine_updates(&resumed, "open a measured task").await;
    let opened_snapshot = opened_updates
        .iter()
        .rev()
        .find(|snapshot| snapshot.active_node_id == "2.1")
        .expect("spine.open should publish the measured task node");
    let opened_pressure = active_pressure(opened_snapshot, "2.1")
        .expect("opened task should expose its pressure checkpoint");
    assert_eq!(opened_pressure.open_input_tokens, Some(300));
    assert_eq!(opened_pressure.current_input_tokens, Some(330));
    assert_eq!(opened_pressure.context_tokens, Some(30));
    assert_eq!(opened_pressure.problem, None);
    let final_snapshot = steady_updates
        .iter()
        .rev()
        .find(|snapshot| active_pressure(snapshot, "2") == Some(steady_pressure))
        .expect("steady sampling should publish its final pressure");
    assert_eq!(
        final_snapshot
            .nodes
            .iter()
            .find(|node| node.node_id == "1")
            .expect("archived epoch should survive replay")
            .context_pressure,
        None,
        "pre-compact pressure must not leak into the resumed epoch"
    );

    let resumed_path = fetch_conversation_path(&resumed);
    let expected_replayed_nodes = opened_snapshot.nodes.clone();
    shutdown_conversation(&resumed).await;
    let replayed = resume_conversation(&manager, &config, resumed_path).await;
    let replayed_snapshot = wait_for_spine_snapshot(&replayed, "2.1").await;
    assert_eq!(replayed_snapshot.nodes, expected_replayed_nodes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Scenario: after the forked branch is compacted, resuming again should reuse
/// the compacted history and only append the new user message.
async fn compact_resume_after_second_compaction_preserves_history() -> Result<()> {
    if network_disabled() {
        println!("Skipping test because network is disabled in this sandbox");
        return Ok(());
    }

    // 1. Arrange mocked SSE responses as a single ordered stream so assertions
    // observe the real request sequence instead of per-mock duplicate captures.
    let server = MockServer::start().await;
    let request_log = mount_second_compact_sequence(&server).await;

    // 2. Drive the conversation through compact -> resume -> fork -> compact -> resume.
    let (_home, config, manager, base) = start_test_conversation(&server, /*model*/ None).await;

    user_turn(&base, "hello world").await;
    compact_conversation(&base).await;
    user_turn(&base, "AFTER_COMPACT").await;
    let base_path = fetch_conversation_path(&base);
    assert!(
        base_path.exists(),
        "second compact test expects base path {base_path:?} to exist",
    );

    shutdown_conversation(&base).await;
    seed_first_checkpoint_harness_metadata(&base_path, "hello world")?;
    let resumed = resume_conversation(&manager, &config, base_path).await;
    user_turn(&resumed, "AFTER_RESUME").await;
    let resumed_path = fetch_conversation_path(&resumed);
    assert!(
        resumed_path.exists(),
        "second compact test expects resumed path {resumed_path:?} to exist",
    );

    let forked = fork_thread(&manager, &config, resumed_path, /*nth_user_message*/ 3).await;
    user_turn(&forked, "AFTER_FORK").await;

    compact_conversation(&forked).await;
    user_turn(&forked, "AFTER_COMPACT_2").await;
    let forked_path = fetch_conversation_path(&forked);
    assert!(
        forked_path.exists(),
        "second compact test expects forked path {forked_path:?} to exist",
    );

    shutdown_conversation(&forked).await;
    assert_latest_checkpoint_retains_harness_metadata(&forked_path, "hello world")?;
    let resumed_again = resume_conversation(&manager, &config, forked_path).await;
    user_turn(&resumed_again, AFTER_SECOND_RESUME).await;

    let mut requests = request_log
        .requests()
        .into_iter()
        .map(|request| request.body_json())
        .collect::<Vec<_>>();
    requests.iter_mut().for_each(normalize_line_endings);
    normalize_compact_prompts(&mut requests);
    assert!(
        requests
            .iter()
            .flat_map(|request| request["input"].as_array().expect("provider request input"))
            .all(|item| {
                item.get(CHECKPOINT_METADATA_KEY).is_none() && item.get("metadata").is_none()
            }),
        "provider requests must not contain harness metadata"
    );
    let input_after_compact = json!(requests[requests.len() - 2]["input"]);
    let input_after_resume = json!(requests[requests.len() - 1]["input"]);

    // test input after compact before resume is the same as input after resume
    let compact_input_array = input_after_compact
        .as_array()
        .expect("input after compact should be an array");
    let resume_input_array = input_after_resume
        .as_array()
        .expect("input after resume should be an array");
    assert!(
        compact_input_array.len() <= resume_input_array.len(),
        "after-resume input should have at least as many items as after-compact"
    );
    assert_eq!(
        compact_input_array.as_slice(),
        &resume_input_array[..compact_input_array.len()]
    );
    let first_request_user_texts = json_message_input_texts(&requests[0], "user");
    let first_turn_user_index = first_request_user_texts
        .len()
        .checked_sub(1)
        .expect("first turn request missing user messages");
    assert_eq!(
        first_request_user_texts[first_turn_user_index],
        "hello world"
    );
    let seeded_user_prefix = &first_request_user_texts[..first_turn_user_index];
    let summary_after_second_compact =
        extract_summary_user_text(&requests[requests.len() - 2], SUMMARY_TEXT);
    let mut expected_after_second_compact_user_texts = vec![
        "hello world".to_string(),
        "AFTER_COMPACT".to_string(),
        "AFTER_RESUME".to_string(),
    ];
    expected_after_second_compact_user_texts.extend_from_slice(seeded_user_prefix);
    expected_after_second_compact_user_texts.extend([
        "AFTER_FORK".to_string(),
        summary_after_second_compact.clone(),
        "AFTER_COMPACT_2".to_string(),
    ]);
    let mut expected_fork_local_user_texts = seeded_user_prefix.to_vec();
    expected_fork_local_user_texts.extend([
        "AFTER_FORK".to_string(),
        summary_after_second_compact,
        "AFTER_COMPACT_2".to_string(),
    ]);
    let final_user_texts = json_message_input_texts(&requests[requests.len() - 1], "user");
    let (final_last, final_prefix) = final_user_texts
        .split_last()
        .expect("after-second-resume request missing user messages");
    assert_eq!(final_last, AFTER_SECOND_RESUME);
    let matched_prefix_len = if let Some(start) = final_prefix
        .windows(expected_after_second_compact_user_texts.len())
        .position(|window| window == expected_after_second_compact_user_texts)
    {
        start + expected_after_second_compact_user_texts.len()
    } else if let Some(start) = final_prefix
        .windows(expected_fork_local_user_texts.len())
        .position(|window| window == expected_fork_local_user_texts)
    {
        start + expected_fork_local_user_texts.len()
    } else {
        panic!("after-second-resume user texts should preserve post-compact user history prefix");
    };
    let final_seeded_suffix = &final_prefix[matched_prefix_len..];
    if seeded_user_prefix.is_empty() {
        assert!(
            final_seeded_suffix.is_empty(),
            "after-second-resume request should not append unexpected user prefix items"
        );
    } else {
        let mut chunks = final_seeded_suffix.chunks_exact(seeded_user_prefix.len());
        assert!(
            chunks.remainder().is_empty(),
            "after-second-resume suffix should be whole seeded-prefix repeats"
        );
        for chunk in &mut chunks {
            assert_eq!(chunk, seeded_user_prefix);
        }
    }
    Ok(())
}

fn normalize_line_endings(value: &mut Value) {
    match value {
        Value::String(text) if text.contains('\r') => {
            *text = text.replace("\r\n", "\n").replace('\r', "\n");
        }
        Value::Array(items) => {
            for item in items {
                normalize_line_endings(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                normalize_line_endings(item);
            }
        }
        _ => {}
    }
}

fn gather_requests(request_log: &[ResponseMock]) -> Vec<ResponsesRequest> {
    request_log
        .iter()
        .flat_map(ResponseMock::requests)
        .collect::<Vec<_>>()
}

fn gather_request_bodies(request_log: &[ResponseMock]) -> Vec<Value> {
    let mut bodies = gather_requests(request_log)
        .into_iter()
        .map(|request| request.body_json())
        .collect::<Vec<_>>();
    bodies.iter_mut().for_each(normalize_line_endings);
    bodies
}

async fn mount_initial_flow(server: &MockServer) -> Vec<ResponseMock> {
    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed("r2"),
    ]);
    let sse3 = sse(vec![
        ev_assistant_message("m3", "AFTER_COMPACT_REPLY"),
        ev_completed("r3"),
    ]);
    let sse4 = sse(vec![ev_completed("r4")]);
    let sse5 = sse(vec![ev_completed("r5")]);

    let match_first = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains("\"text\":\"hello world\"")
            && !body.contains(&format!("\"text\":\"{SUMMARY_TEXT}\""))
            && !body.contains("\"text\":\"AFTER_COMPACT\"")
            && !body.contains("\"text\":\"AFTER_RESUME\"")
            && !body.contains("\"text\":\"AFTER_FORK\"")
    };
    let first = mount_sse_once_match(server, match_first, sse1).await;

    let match_compact = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body_contains_text(body, SUMMARIZATION_PROMPT) || body.contains(&json_fragment(FIRST_REPLY))
    };
    let compact = mount_sse_once_match(server, match_compact, sse2).await;

    let match_after_compact = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains("\"text\":\"AFTER_COMPACT\"")
            && !body.contains("\"text\":\"AFTER_RESUME\"")
            && !body.contains("\"text\":\"AFTER_FORK\"")
    };
    let after_compact = mount_sse_once_match(server, match_after_compact, sse3).await;

    let match_after_resume = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains("\"text\":\"AFTER_RESUME\"")
    };
    let after_resume = mount_sse_once_match(server, match_after_resume, sse4).await;

    let match_after_fork = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains("\"text\":\"AFTER_FORK\"")
    };
    let after_fork = mount_sse_once_match(server, match_after_fork, sse5).await;

    vec![first, compact, after_compact, after_resume, after_fork]
}

async fn mount_spine_snapshot_flow(server: &MockServer) -> ResponseMock {
    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed_with_tokens("r1", /*total_tokens*/ 450),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed_with_tokens("r2", /*total_tokens*/ 900),
    ]);
    let sse3 = sse(vec![ev_completed("r4")]);
    let sse4 = sse(vec![ev_completed("r5")]);
    mount_sse_sequence(server, vec![sse1, sse2, sse3, sse4]).await
}

async fn mount_spine_truncated_fork_flow(server: &MockServer) -> ResponseMock {
    let retained_open = sse(vec![
        ev_response_created("retained-open-response"),
        ev_function_call_with_namespace(
            "retained-open-call",
            "spine",
            "open",
            r#"{"summary":"retained task"}"#,
        ),
        ev_completed("retained-open-response"),
    ]);
    let retained_final = sse(vec![
        ev_assistant_message("retained-final", "retained task opened"),
        ev_completed("retained-final-response"),
    ]);
    let discarded_open = sse(vec![
        ev_response_created("discarded-open-response"),
        ev_function_call_with_namespace(
            "discarded-open-call",
            "spine",
            "open",
            r#"{"summary":"discarded task"}"#,
        ),
        ev_completed("discarded-open-response"),
    ]);
    let discarded_final = sse(vec![
        ev_assistant_message("discarded-final", "discarded task opened"),
        ev_completed("discarded-final-response"),
    ]);
    let fork_followup = sse(vec![
        ev_assistant_message("fork-followup", "forked context confirmed"),
        ev_completed("fork-followup-response"),
    ]);
    mount_sse_sequence(
        server,
        vec![
            retained_open,
            retained_final,
            discarded_open,
            discarded_final,
            fork_followup,
        ],
    )
    .await
}

async fn mount_spine_pressure_epoch_flow(server: &MockServer) -> ResponseMock {
    let initial = sse(vec![
        ev_assistant_message("pressure-initial", "initial pressure measured"),
        ev_completed_with_tokens("pressure-initial-response", /*total_tokens*/ 450),
    ]);
    let compact = sse(vec![
        ev_assistant_message("pressure-compact", SUMMARY_TEXT),
        ev_completed_with_tokens("pressure-compact-response", /*total_tokens*/ 900),
    ]);
    let resumed = sse(vec![
        ev_assistant_message("pressure-resumed", "resumed pressure measured"),
        ev_completed_with_tokens("pressure-resumed-response", /*total_tokens*/ 120),
    ]);
    let steady = sse(vec![
        ev_assistant_message("pressure-steady", "steady pressure measured"),
        ev_completed_with_tokens("pressure-steady-response", /*total_tokens*/ 220),
    ]);
    let opened = sse(vec![
        ev_response_created("pressure-open-response"),
        ev_function_call_with_namespace(
            "pressure-open-call",
            "spine",
            "open",
            r#"{"summary":"measured task"}"#,
        ),
        ev_completed_with_tokens("pressure-open-response", /*total_tokens*/ 300),
    ]);
    let opened_follow_up = sse(vec![
        ev_assistant_message("pressure-open-done", "measured task opened"),
        ev_completed_with_tokens("pressure-open-follow-up", /*total_tokens*/ 330),
    ]);
    mount_sse_sequence(
        server,
        vec![initial, compact, resumed, steady, opened, opened_follow_up],
    )
    .await
}

async fn mount_second_compact_sequence(server: &MockServer) -> ResponseMock {
    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed("r2"),
    ]);
    let sse3 = sse(vec![
        ev_assistant_message("m3", "AFTER_COMPACT_REPLY"),
        ev_completed("r3"),
    ]);
    let sse4 = sse(vec![ev_completed("r4")]);
    let sse5 = sse(vec![ev_completed("r5")]);
    let sse6 = sse(vec![
        ev_assistant_message("m4", SUMMARY_TEXT),
        ev_completed("r6"),
    ]);
    let sse7 = sse(vec![ev_completed("r7")]);
    let sse8 = sse(vec![ev_completed("r8")]);

    mount_sse_sequence(server, vec![sse1, sse2, sse3, sse4, sse5, sse6, sse7, sse8]).await
}

async fn start_test_conversation(
    server: &MockServer,
    model: Option<&str>,
) -> (Arc<TempDir>, Config, Arc<ThreadManager>, Arc<CodexThread>) {
    let base_url = format!("{}/v1", server.uri());
    let model = model.map(str::to_string);
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
    let mut builder = test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_config(move |config| {
            config.update_plan_enabled = true;
            config.model_provider.name = "Non-OpenAI Model provider".to_string();
            config.model_provider.base_url = Some(base_url);
            config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
            if let Some(model) = model {
                config.model = Some(model);
            }
        });
    let test = Box::pin(builder.build(server))
        .await
        .expect("create conversation");
    (test.home, test.config, test.thread_manager, test.codex)
}

async fn start_spine_test_conversation(
    server: &MockServer,
    model: Option<&str>,
) -> (Arc<TempDir>, Config, Arc<ThreadManager>, Arc<CodexThread>) {
    let base_url = format!("{}/v1", server.uri());
    let model = model.map(str::to_string);
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
    let mut builder = spine_test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_config(move |config| {
            config.model_provider.name = "Non-OpenAI Model provider".to_string();
            config.model_provider.base_url = Some(base_url);
            config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
            if let Some(model) = model {
                config.model = Some(model);
            }
        });
    let test = Box::pin(builder.build(server))
        .await
        .expect("create Spine conversation");
    (test.home, test.config, test.thread_manager, test.codex)
}

async fn user_turn(conversation: &Arc<CodexThread>, text: &str) {
    conversation
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.into(),
            text_elements: Vec::new(),
        }]))
        .await
        .expect("submit user turn");
    wait_for_event(conversation, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
    ThreadIdle::wait(conversation).await;
}

async fn user_turn_with_spine_updates(
    conversation: &Arc<CodexThread>,
    text: &str,
) -> Vec<SpineTreeUpdateEvent> {
    conversation
        .start_or_steer_turn(
            codex_protocol::turn_input::TurnInputRequest::user_input(vec![UserInput::Text {
                text: text.into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(Default::default()),
        )
        .await
        .expect("submit user turn");
    collect_spine_updates_until_turn_complete(conversation).await
}

async fn collect_spine_updates_until_turn_complete(
    conversation: &Arc<CodexThread>,
) -> Vec<SpineTreeUpdateEvent> {
    let mut snapshots = Vec::new();
    loop {
        let event = conversation.next_event().await.expect("next event");
        match event.msg {
            EventMsg::SpineTreeUpdate(snapshot) => snapshots.push(snapshot),
            EventMsg::TurnComplete(_) => return snapshots,
            _ => {}
        }
    }
}

fn active_pressure<'snapshot>(
    snapshot: &'snapshot SpineTreeUpdateEvent,
    active_node_id: &str,
) -> Option<&'snapshot codex_protocol::spine_tree::SpineNodeContextPressureSnapshot> {
    snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == active_node_id)
        .and_then(|node| node.context_pressure.as_ref())
}

async fn compact_conversation(conversation: &Arc<CodexThread>) {
    conversation
        .submit(Op::Compact)
        .await
        .expect("compact conversation");
    wait_for_event(conversation, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
    ThreadIdle::wait(conversation).await;
}

async fn wait_for_spine_snapshot(
    conversation: &Arc<CodexThread>,
    active_node_id: &str,
) -> codex_protocol::protocol::SpineTreeUpdateEvent {
    wait_for_event_match(conversation, |event| match event {
        EventMsg::SpineTreeUpdate(snapshot) if snapshot.active_node_id == active_node_id => {
            Some(snapshot.clone())
        }
        _ => None,
    })
    .await
}

fn fetch_conversation_path(conversation: &Arc<CodexThread>) -> std::path::PathBuf {
    conversation.rollout_path().expect("rollout path")
}

async fn shutdown_conversation(conversation: &Arc<CodexThread>) {
    conversation
        .shutdown_and_wait()
        .await
        .expect("shutdown conversation");
}

async fn resume_conversation(
    manager: &ThreadManager,
    config: &Config,
    path: std::path::PathBuf,
) -> Arc<CodexThread> {
    let auth_manager = codex_core::test_support::auth_manager_from_auth(
        codex_login::CodexAuth::from_api_key("dummy"),
    );
    Box::pin(manager.resume_legacy_thread_from_rollout(
        config.clone(),
        path,
        auth_manager,
        /*parent_trace*/ None,
        ClientMcpExtensions::default(),
    ))
    .await
    .expect("resume conversation")
    .thread
}

#[cfg(test)]
async fn fork_thread(
    manager: &ThreadManager,
    config: &Config,
    path: std::path::PathBuf,
    nth_user_message: usize,
) -> Arc<CodexThread> {
    Box::pin(manager.fork_legacy_thread(
        nth_user_message,
        codex_core::StartThreadOptions::new(config.clone()),
        path,
    ))
    .await
    .expect("fork conversation")
    .thread
}

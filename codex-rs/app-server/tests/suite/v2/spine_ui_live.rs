use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use tempfile::TempDir;
use tokio::time::timeout;

#[cfg(any(target_os = "macos", windows))]
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(not(any(target_os = "macos", windows)))]
const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

const SERVER_NAME: &str = "__codex_internal_spine_tree_ui__";
const TOOL_NAME: &str = "spine_tree";
const RESOURCE_URI: &str = "ui://spine/tree.html";
const ROOT_SUMMARY: &str = "root";
const BEFORE_RESUME_SUMMARY: &str = "before cold resume";
const AFTER_RESUME_SUMMARY: &str = "after cold resume";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_ui_is_live_only_and_rebuilds_core_tree_after_cold_resume() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = create_mock_responses_server_sequence(vec![
        responses::sse(vec![
            responses::ev_response_created("spine-open"),
            responses::ev_function_call_with_namespace(
                "spine-open",
                "spine",
                "open",
                &format!(r#"{{"summary":"{BEFORE_RESUME_SUMMARY}"}}"#),
            ),
            responses::ev_completed("spine-open"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("spine-done"),
            responses::ev_assistant_message("spine-message", "done"),
            responses::ev_completed("spine-done"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("spine-resumed-open"),
            responses::ev_function_call_with_namespace(
                "spine-resumed-open",
                "spine",
                "open",
                &format!(r#"{{"summary":"{AFTER_RESUME_SUMMARY}"}}"#),
            ),
            responses::ev_completed("spine-resumed-open"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("spine-resumed-done"),
            responses::ev_assistant_message("spine-resumed-message", "done after resume"),
            responses::ev_completed("spine-resumed-done"),
        ]),
    ])
    .await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 100_000,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;

    let mut app = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("CODEX_SPINE_APP_UI", Some("1"))])
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, app.initialize()).await??;

    let thread_request = app
        .send_thread_start_request_with_auto_env(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_response_message(RequestId::Integer(thread_request)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response(thread_response)?;

    let turn_request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "open a Spine task".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_response_message(RequestId::Integer(turn_request)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response(turn_response)?;

    let started = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification("live Spine item/started", |notification| {
            notification.method == "item/started"
                && notification.params.as_ref().is_some_and(|params| {
                    serde_json::from_value::<ItemStartedNotification>(params.clone())
                        .is_ok_and(|notification| is_internal_item(&notification.item))
                })
        }),
    )
    .await??;
    let started: ItemStartedNotification =
        serde_json::from_value(started.params.expect("item/started params"))?;
    assert_eq!(started.thread_id, thread.id);
    assert_eq!(started.turn_id, turn.id);

    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let completed = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification("live Spine item/completed", |notification| {
            notification.method == "item/completed"
                && notification.params.as_ref().is_some_and(|params| {
                    serde_json::from_value::<ItemCompletedNotification>(params.clone())
                        .is_ok_and(|notification| is_internal_item(&notification.item))
                })
        }),
    )
    .await??;
    let completed: ItemCompletedNotification =
        serde_json::from_value(completed.params.expect("item/completed params"))?;
    assert_eq!(completed.thread_id, thread.id);
    assert_eq!(completed.turn_id, turn.id);

    let loaded = read_thread(&mut app, &thread.id).await?;
    assert_no_internal_items(&loaded);
    let rollout_path = loaded.path.as_ref().expect("materialized rollout path");
    let rollout = std::fs::read_to_string(rollout_path)?;
    for marker in [SERVER_NAME, "spine-ui-", RESOURCE_URI] {
        assert!(
            !rollout.contains(marker),
            "live-only Spine item leaked into rollout via marker {marker}"
        );
    }

    drop(app);

    let mut app = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("CODEX_SPINE_APP_UI", Some("1"))])
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, app.initialize()).await??;

    let reloaded = read_thread(&mut app, &thread.id).await?;
    assert_no_internal_items(&reloaded);

    let resume_request = app
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread.id,
            ..Default::default()
        })
        .await?;
    let resume_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_response_message(RequestId::Integer(resume_request)),
    )
    .await??;
    let ThreadResumeResponse {
        thread: resumed, ..
    } = to_response(resume_response)?;
    assert_no_internal_items(&resumed);

    let resumed_turn_request = app
        .send_turn_start_request(TurnStartParams {
            thread_id: resumed.id.clone(),
            input: vec![UserInput::Text {
                text: "open another Spine task after cold resume".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let resumed_turn_response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_response_message(RequestId::Integer(resumed_turn_request)),
    )
    .await??;
    let TurnStartResponse { turn: resumed_turn } = to_response(resumed_turn_response)?;

    let rebuilt = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification(
            "resumed live Spine item with rebuilt Core nodes",
            |notification| {
                notification.method == "item/started"
                    && notification.params.as_ref().is_some_and(|params| {
                        serde_json::from_value::<ItemStartedNotification>(params.clone()).is_ok_and(
                            |notification| {
                                snapshot_node_summaries(&notification.item).is_some_and(
                                    |summaries| {
                                        summaries.contains(BEFORE_RESUME_SUMMARY)
                                            && summaries.contains(AFTER_RESUME_SUMMARY)
                                    },
                                )
                            },
                        )
                    })
            },
        ),
    )
    .await??;
    let rebuilt: ItemStartedNotification =
        serde_json::from_value(rebuilt.params.expect("item/started params"))?;
    assert_eq!(rebuilt.thread_id, resumed.id);
    assert_eq!(rebuilt.turn_id, resumed_turn.id);
    assert_eq!(
        snapshot_node_summaries(&rebuilt.item),
        Some(BTreeSet::from([
            ROOT_SUMMARY.to_string(),
            BEFORE_RESUME_SUMMARY.to_string(),
            AFTER_RESUME_SUMMARY.to_string(),
        ]))
    );
    let structured_content =
        internal_structured_content(&rebuilt.item).expect("internal Spine structured content");
    assert_eq!(
        structured_content.get("agentSubtrees"),
        Some(&serde_json::json!([]))
    );

    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_matching_notification(
            "resumed live Spine item/completed",
            |notification| {
                notification.method == "item/completed"
                    && notification.params.as_ref().is_some_and(|params| {
                        serde_json::from_value::<ItemCompletedNotification>(params.clone())
                            .is_ok_and(|notification| {
                                notification.turn_id == resumed_turn.id
                                    && is_internal_item(&notification.item)
                            })
                    })
            },
        ),
    )
    .await??;

    let after_resumed_turn = read_thread(&mut app, &resumed.id).await?;
    assert_no_internal_items(&after_resumed_turn);

    Ok(())
}

async fn read_thread(app: &mut TestAppServer, thread_id: &str) -> Result<Thread> {
    let request = app
        .send_thread_read_request(ThreadReadParams {
            thread_id: thread_id.to_string(),
            include_turns: true,
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        app.read_stream_until_response_message(RequestId::Integer(request)),
    )
    .await??;
    let ThreadReadResponse { thread } = to_response(response)?;
    Ok(thread)
}

fn assert_no_internal_items(thread: &Thread) {
    assert!(
        thread
            .turns
            .iter()
            .flat_map(|turn| &turn.items)
            .all(|item| !is_internal_item(item)),
        "thread history restored a live-only Spine item"
    );
}

fn is_internal_item(item: &ThreadItem) -> bool {
    matches!(
        item,
        ThreadItem::McpToolCall {
            id,
            server,
            tool,
            mcp_app_resource_uri: Some(resource_uri),
            ..
        } if id.starts_with("spine-ui-")
            && server == SERVER_NAME
            && tool == TOOL_NAME
            && resource_uri == RESOURCE_URI
    )
}

fn internal_structured_content(item: &ThreadItem) -> Option<&serde_json::Value> {
    let ThreadItem::McpToolCall {
        result: Some(result),
        ..
    } = item
    else {
        return None;
    };
    result.structured_content.as_ref()
}

fn snapshot_node_summaries(item: &ThreadItem) -> Option<BTreeSet<String>> {
    Some(
        internal_structured_content(item)?
            .pointer("/snapshot/nodes")?
            .as_array()?
            .iter()
            .filter_map(|node| node.get("summary")?.as_str().map(str::to_string))
            .collect(),
    )
}

use std::time::Duration;

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use codex_app_server_protocol::ThreadCompactStartParams;
use codex_app_server_protocol::ThreadCompactStartResponse;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const READ_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compacted_spine_lineage_replays_across_resume_fork_and_child_resume() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_log = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("pre-open-response"),
                responses::ev_function_call_with_namespace(
                    "pre-open-call",
                    "spine",
                    "open",
                    r#"{"goal":"pre compact node"}"#,
                ),
                responses::ev_completed_with_tokens("pre-open-response", /*total_tokens*/ 100),
            ]),
            assistant_response("pre-open-final", "PRE_OPEN_REPLY"),
            assistant_response("compact", "COMPACT_SUMMARY"),
            responses::sse(vec![
                responses::ev_response_created("post-open-response"),
                responses::ev_function_call_with_namespace(
                    "post-open-call",
                    "spine",
                    "open",
                    r#"{"goal":"post compact node"}"#,
                ),
                responses::ev_completed_with_tokens(
                    "post-open-response",
                    /*total_tokens*/ 100,
                ),
            ]),
            assistant_response("post-open-final", "POST_OPEN_REPLY"),
            assistant_response("parent-resume", "PARENT_RESUME_REPLY"),
            assistant_response("fork", "FORK_REPLY"),
            assistant_response("child-resume", "CHILD_RESUME_REPLY"),
        ],
    )
    .await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::SpineJit)
        .with_root_config("compact_prompt = \"Summarize the conversation.\"")
        .write(codex_home.path())?;

    let mut primary = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let started = primary
        .start_thread(ThreadStartParams {
            model: Some("gpt-5.4".to_string()),
            ..Default::default()
        })
        .await?;
    let source_thread_id = started.thread.id;
    let source_path = started.thread.path.expect("persistent thread path");

    run_turn(&mut primary, &source_thread_id, "before compact").await?;
    let pre_compact_tree = wait_for_tree(&mut primary, &source_thread_id, "1.1").await?;
    assert!(
        pre_compact_tree
            .nodes
            .iter()
            .any(|node| node.node_id == "1.1")
    );
    compact_thread(&mut primary, &source_thread_id).await?;
    let compact_tree = wait_for_tree(&mut primary, &source_thread_id, "2").await?;
    assert!(compact_tree.nodes.iter().any(|node| node.node_id == "1.1"));
    let post_compact =
        run_turn(&mut primary, &source_thread_id, "post compact source turn").await?;
    wait_for_tree(&mut primary, &source_thread_id, "2.1").await?;
    timeout(READ_TIMEOUT, primary.shutdown_gracefully()).await??;

    let mut secondary = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    resume_thread(&mut secondary, &source_thread_id).await?;
    wait_for_tree(&mut secondary, &source_thread_id, "2.1").await?;
    run_turn(
        &mut secondary,
        &source_thread_id,
        "after parent compact resume",
    )
    .await?;
    let source_before_fork = std::fs::read(source_path.as_path())?;

    let fork_id = secondary
        .send_thread_fork_request(ThreadForkParams {
            thread_id: source_thread_id.clone(),
            before_turn_id: Some(post_compact.turn.id),
            ..Default::default()
        })
        .await?;
    let ThreadForkResponse { thread: child, .. } =
        timeout(READ_TIMEOUT, secondary.read_response(fork_id)).await??;
    let fork_tree = wait_for_tree(&mut secondary, &child.id, "2").await?;
    assert!(fork_tree.nodes.iter().all(|node| node.node_id != "2.1"));
    assert!(fork_tree.nodes.iter().any(|node| node.node_id == "1.1"));
    run_turn(&mut secondary, &child.id, "after compact fork").await?;
    assert_eq!(std::fs::read(source_path.as_path())?, source_before_fork);
    timeout(READ_TIMEOUT, secondary.shutdown_gracefully()).await??;

    let mut tertiary = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    resume_thread(&mut tertiary, &child.id).await?;
    let resumed_child_tree = wait_for_tree(&mut tertiary, &child.id, "2").await?;
    assert_eq!(resumed_child_tree.nodes, fork_tree.nodes);
    run_turn(&mut tertiary, &child.id, "after child resume").await?;
    timeout(READ_TIMEOUT, tertiary.shutdown_gracefully()).await??;

    let requests = response_log.requests();
    assert_eq!(requests.len(), 8);
    assert!(requests[5].body_contains_text("post compact source turn"));
    assert!(requests[6].body_contains_text("before compact"));
    assert!(!requests[6].body_contains_text("post compact source turn"));
    assert!(!requests[6].body_contains_text("after parent compact resume"));
    assert!(requests[7].body_contains_text("after compact fork"));
    assert!(!requests[7].body_contains_text("post compact source turn"));

    Ok(())
}

fn assistant_response(id: &str, text: &str) -> String {
    responses::sse(vec![
        responses::ev_assistant_message(id, text),
        responses::ev_completed_with_tokens(
            format!("{id}-response").as_str(),
            /*total_tokens*/ 100,
        ),
    ])
}

async fn run_turn(
    app: &mut TestAppServer,
    thread_id: &str,
    text: &str,
) -> Result<TurnCompletedNotification> {
    timeout(
        READ_TIMEOUT,
        app.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread_id.to_string(),
            input: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await?
}

async fn compact_thread(app: &mut TestAppServer, thread_id: &str) -> Result<()> {
    let request_id = app
        .send_thread_compact_start_request(ThreadCompactStartParams {
            thread_id: thread_id.to_string(),
        })
        .await?;
    let _: ThreadCompactStartResponse =
        timeout(READ_TIMEOUT, app.read_response(request_id)).await??;
    loop {
        let completed: ItemCompletedNotification =
            timeout(READ_TIMEOUT, app.read_notification("item/completed")).await??;
        if completed.thread_id == thread_id
            && matches!(completed.item, ThreadItem::ContextCompaction { .. })
        {
            return Ok(());
        }
    }
}

async fn resume_thread(app: &mut TestAppServer, thread_id: &str) -> Result<()> {
    let request_id = app
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.to_string(),
            exclude_turns: true,
            ..Default::default()
        })
        .await?;
    let ThreadResumeResponse { thread, .. } =
        timeout(READ_TIMEOUT, app.read_response(request_id)).await??;
    assert_eq!(thread.id, thread_id);
    Ok(())
}

async fn wait_for_tree(
    app: &mut TestAppServer,
    thread_id: &str,
    active_node_id: &str,
) -> Result<SpineTreeUpdatedNotification> {
    loop {
        let snapshot: SpineTreeUpdatedNotification = timeout(
            READ_TIMEOUT,
            app.read_notification("turn/spineTree/updated"),
        )
        .await??;
        if snapshot.thread_id == thread_id && snapshot.active_node_id == active_node_id {
            return Ok(snapshot);
        }
    }
}

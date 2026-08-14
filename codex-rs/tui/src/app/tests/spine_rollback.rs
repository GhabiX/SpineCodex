use super::*;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use crate::bottom_pane::StatusLineItem;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use pretty_assertions::assert_eq;
use std::time::Duration;

const MODEL: &str = "gpt-5.6-sol";
const MODEL_PROVIDER_ID: &str = "tui-spine-rollback";

fn submit_user_turn(
    app: &mut App,
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
    text: &str,
) -> impl std::future::Future<Output = Result<()>> {
    let turn = AppCommand::user_turn(
        vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }],
        app.config.cwd.to_path_buf(),
        AskForApproval::Never,
        /*active_permission_profile*/ None,
        MODEL.to_string(),
        /*effort*/ None,
        /*summary*/ None,
        /*service_tier*/ None,
        /*final_output_json_schema*/ None,
        /*collaboration_mode*/ None,
        /*personality*/ None,
    );
    app.submit_thread_op(app_server, thread_id, turn)
}

fn drain_active_thread_events(app: &mut App) -> bool {
    let mut drained = false;
    while let Some(event) = app
        .active_thread_rx
        .as_mut()
        .and_then(|receiver| receiver.try_recv().ok())
    {
        drained = true;
        app.handle_thread_event_now(event);
    }
    drained
}

async fn drain_queued_app_events(
    app: &mut App,
    app_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tui: &mut tui::Tui,
    app_server: &mut AppServerSession,
) -> Result<()> {
    loop {
        let mut drained = drain_active_thread_events(app);
        while let Ok(event) = app_event_rx.try_recv() {
            drained = true;
            app.handle_event(tui, app_server, event).await?;
        }
        if !drained {
            return Ok(());
        }
    }
}

async fn drive_turn_until_complete(
    app: &mut App,
    app_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tui: &mut tui::Tui,
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
) -> Result<Vec<SpineTreeUpdatedNotification>> {
    let mut snapshots = Vec::new();
    let mut completed = false;
    let timeout = tokio::time::sleep(Duration::from_secs(/*secs*/ 15));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            event = app_server.next_event() => {
                let event = event.expect("embedded app-server stream should remain open");
                if let AppServerEvent::ServerNotification(notification) = &event {
                    match notification.as_ref() {
                        ServerNotification::SpineTreeUpdated(snapshot)
                            if snapshot.thread_id == thread_id.to_string() =>
                        {
                            snapshots.push(snapshot.clone());
                        }
                        ServerNotification::TurnCompleted(notification)
                            if notification.thread_id == thread_id.to_string() =>
                        {
                            completed = true;
                        }
                        _ => {}
                    }
                }
                app.handle_app_server_event(app_server, event).await;
            }
            event = app_event_rx.recv() => {
                let event = event.expect("app event stream should remain open");
                app.handle_event(tui, app_server, event).await?;
            }
            () = &mut timeout => panic!("timed out waiting for thread {thread_id} to complete"),
        }
        drain_queued_app_events(app, app_event_rx, tui, app_server).await?;
        if completed {
            return Ok(snapshots);
        }
    }
}

async fn wait_for_replayed_spine_tree(
    app: &mut App,
    app_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tui: &mut tui::Tui,
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
    active_node_id: &str,
) -> Result<Vec<SpineTreeUpdatedNotification>> {
    let mut snapshots = Vec::new();
    let timeout = tokio::time::sleep(Duration::from_secs(/*secs*/ 15));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            event = app_server.next_event() => {
                let event = event.expect("embedded app-server stream should remain open");
                if let AppServerEvent::ServerNotification(notification) = &event
                    && let ServerNotification::SpineTreeUpdated(snapshot) = notification.as_ref()
                    && snapshot.thread_id == thread_id.to_string()
                {
                    snapshots.push(snapshot.clone());
                }
                app.handle_app_server_event(app_server, event).await;
            }
            event = app_event_rx.recv() => {
                let event = event.expect("app event stream should remain open");
                app.handle_event(tui, app_server, event).await?;
            }
            () = &mut timeout => panic!(
                "timed out waiting for replayed Spine node {active_node_id} on {thread_id}"
            ),
        }
        drain_queued_app_events(app, app_event_rx, tui, app_server).await?;
        if snapshots
            .iter()
            .any(|snapshot| snapshot.active_node_id == active_node_id)
        {
            return Ok(snapshots);
        }
    }
}

async fn mount_rollback_flow(server: &wiremock::MockServer) -> ResponseMock {
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
    let fork_barrier = sse(vec![
        ev_assistant_message("fork-barrier", "rollback state confirmed"),
        ev_completed("fork-barrier-response"),
    ]);
    mount_sse_sequence(
        server,
        vec![
            retained_open,
            retained_final,
            discarded_open,
            discarded_final,
            fork_barrier,
        ],
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompt_edit_replays_truncated_spine_tree_through_embedded_app_server() -> Result<()> {
    let server = start_mock_server().await;
    let request_log = mount_rollback_flow(&server).await;
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"
model = "{MODEL}"
model_provider = "{MODEL_PROVIDER_ID}"

[model_providers.{MODEL_PROVIDER_ID}]
name = "TUI Spine Rollback"
base_url = "{}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
supports_websockets = false

[features]
spine_jit = true
"#,
            server.uri()
        ),
    )?;
    let cwd = app.config.cwd.to_path_buf();
    app.config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .harness_overrides(ConfigOverrides {
            cwd: Some(cwd),
            ..Default::default()
        })
        .build()
        .await?;

    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = app_server.start_thread(&app.config).await?;
    let source_thread_id = started.session.thread_id;
    let source_rollout_path = started
        .session
        .rollout_path
        .clone()
        .expect("persistent source thread should expose its rollout path");
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        started,
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    app.chat_widget.setup_status_line(
        vec![StatusLineItem::SpineNode],
        /*use_theme_colors*/ true,
    );
    while app_event_rx.try_recv().is_ok() {}

    submit_user_turn(
        &mut app,
        &mut app_server,
        source_thread_id,
        "open retained task",
    )
    .await?;
    let retained_snapshots = drive_turn_until_complete(
        &mut app,
        &mut app_event_rx,
        &mut tui,
        &mut app_server,
        source_thread_id,
    )
    .await?;
    assert!(
        retained_snapshots
            .iter()
            .any(|snapshot| snapshot.active_node_id == "1.1")
    );

    submit_user_turn(
        &mut app,
        &mut app_server,
        source_thread_id,
        "open discarded task",
    )
    .await?;
    let discarded_snapshots = drive_turn_until_complete(
        &mut app,
        &mut app_event_rx,
        &mut tui,
        &mut app_server,
        source_thread_id,
    )
    .await?;
    assert!(discarded_snapshots.iter().any(|snapshot| {
        snapshot.active_node_id == "1.1.1"
            && snapshot.nodes.iter().any(|node| node.node_id == "1.1.1")
    }));
    assert_eq!(
        app.chat_widget.status_line_text(),
        Some("1.1.1 discarded task".to_string())
    );
    {
        let store = app
            .thread_event_channels
            .get(&source_thread_id)
            .expect("source thread event channel")
            .store
            .lock()
            .await;
        assert!(
            store.turns.is_empty(),
            "new live turns should still reside only in the replay buffer"
        );
    }
    let source_before = std::fs::read_to_string(&source_rollout_path)?;

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::ForkSessionForPromptEdit {
            thread_id: source_thread_id,
            nth_user_message: 1,
            prompt: crate::chatwidget::UserMessage::from("open discarded task"),
        },
    )
    .await?;
    let forked_thread_id = app
        .chat_widget
        .thread_id()
        .expect("prompt edit should attach the forked thread");
    assert_ne!(forked_thread_id, source_thread_id);
    assert!(!app.spine_tree_views.contains_key(&source_thread_id));
    app.chat_widget.setup_status_line(
        vec![StatusLineItem::SpineNode],
        /*use_theme_colors*/ true,
    );

    let mut forked_snapshots = wait_for_replayed_spine_tree(
        &mut app,
        &mut app_event_rx,
        &mut tui,
        &mut app_server,
        forked_thread_id,
        "1.1",
    )
    .await?;
    assert_eq!(
        app.chat_widget.status_line_text(),
        Some("1.1 retained task".to_string())
    );

    submit_user_turn(
        &mut app,
        &mut app_server,
        forked_thread_id,
        "confirm rollback state",
    )
    .await?;
    forked_snapshots.extend(
        drive_turn_until_complete(
            &mut app,
            &mut app_event_rx,
            &mut tui,
            &mut app_server,
            forked_thread_id,
        )
        .await?,
    );

    assert!(forked_snapshots.iter().all(|snapshot| {
        snapshot.active_node_id == "1.1"
            && !snapshot.nodes.iter().any(|node| node.node_id == "1.1.1")
    }));
    let final_snapshot = app
        .spine_tree_views
        .get(&forked_thread_id)
        .and_then(crate::history_cell::SpineTreeViewState::snapshot)
        .expect("forked TUI should retain the replayed Spine tree");
    assert_eq!(final_snapshot.active_node_id, "1.1");
    assert!(
        !final_snapshot
            .nodes
            .iter()
            .any(|node| node.node_id == "1.1.1")
    );
    assert_eq!(
        app.chat_widget.status_line_text(),
        Some("1.1 retained task".to_string())
    );
    let forked_request = request_log
        .requests()
        .last()
        .expect("fork follow-up should have a captured Responses request");
    let forked_request_body = forked_request.body_json().to_string();
    assert!(forked_request_body.contains("retained task"));
    assert!(!forked_request_body.contains("open discarded task"));
    assert!(!forked_request_body.contains("1.1.1"));
    assert_eq!(std::fs::read_to_string(source_rollout_path)?, source_before);
    assert_eq!(request_log.requests().len(), 5);
    app_server.shutdown().await?;

    Ok(())
}

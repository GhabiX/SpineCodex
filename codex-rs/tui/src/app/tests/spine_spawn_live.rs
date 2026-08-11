use super::*;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeKind;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use codex_app_server_protocol::SubAgentActivityKind;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_reasoning_item;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once_match;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

const MODEL: &str = "gpt-5.6-sol";
const MODEL_PROVIDER_ID: &str = "tui-spine-spawn";
const PARENT_PROMPT: &str = "render a live Spine spawn batch";
const SPAWN_CALL_ID: &str = "tui-spawn-call";
const BRANCH_PROMPT_MARKER: &str = "You are a spawned execution branch.";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    serde_json::from_slice::<Value>(&request.body)
        .ok()
        .is_some_and(|body| body.to_string().contains(text))
}

fn child_request(request: &wiremock::Request, marker: &str) -> bool {
    body_contains(request, BRANCH_PROMPT_MARKER) && body_contains(request, marker)
}

fn matching_request_count(
    response_mock: &ResponseMock,
    required: &[&str],
    excluded: &[&str],
) -> usize {
    response_mock
        .requests()
        .iter()
        .filter(|request| {
            required.iter().all(|text| request.body_contains_text(text))
                && excluded
                    .iter()
                    .all(|text| !request.body_contains_text(text))
        })
        .count()
}

fn spawn_args() -> String {
    json!({
        "tasks": [
            {"summary": "first visible task", "prompt": "first-child-marker"},
            {"summary": "second visible task", "prompt": "second-child-marker"}
        ]
    })
    .to_string()
}

fn turn_started(thread_id: ThreadId, turn_id: &str) -> ServerNotification {
    ServerNotification::TurnStarted(TurnStartedNotification {
        thread_id: thread_id.to_string(),
        turn: Turn {
            id: turn_id.to_string(),
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            items: Vec::new(),
            status: TurnStatus::InProgress,
            error: None,
            started_at: Some(0),
            completed_at: None,
            duration_ms: None,
        },
    })
}

fn tree_snapshot(thread_id: ThreadId, turn_id: &str) -> SpineTreeUpdatedNotification {
    SpineTreeUpdatedNotification {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        snapshot_seq: 1,
        active_node_id: "1".to_string(),
        nodes: vec![SpineTreeNode {
            node_id: "1".to_string(),
            parent_id: None,
            kind: SpineTreeNodeKind::Task,
            status: SpineTreeNodeStatus::Live,
            summary: Some("live root task".to_string()),
            memory_summary: None,
            spawn_outcome: None,
            start: 0,
            end: None,
            context_pressure: None,
        }],
        settled_spawn_call_ids: Vec::new(),
    }
}

fn spawn_progress(
    parent_thread_id: ThreadId,
    turn_id: &str,
    child_thread_id: ThreadId,
) -> SpineSpawnProgressUpdatedNotification {
    SpineSpawnProgressUpdatedNotification {
        thread_id: parent_thread_id.to_string(),
        turn_id: turn_id.to_string(),
        call_id: "spawn-order".to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: "visible child task".to_string(),
            thread_id: child_thread_id.to_string(),
            agent_path: Some("/root/visible-child".to_string()),
            status: CollabAgentStatus::Running,
        }],
    }
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

async fn drain_app_events(
    app: &mut App,
    app_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tui: &mut tui::Tui,
    app_server: &mut AppServerSession,
) -> Result<Vec<String>> {
    let mut trace = Vec::new();
    loop {
        let mut drained = drain_active_thread_events(app);
        while let Ok(event) = app_event_rx.try_recv() {
            drained = true;
            if let Some(summary) = projection_event_summary(&event) {
                trace.push(summary);
            }
            app.handle_event(tui, app_server, event).await?;
        }
        if !drained {
            break;
        }
    }
    Ok(trace)
}

fn projection_event_summary(event: &AppEvent) -> Option<String> {
    let AppEvent::ApplySpineProjection { epoch, event } = event else {
        return None;
    };
    let detail = match event {
        SpineProjectionEvent::TreeUpdated(snapshot) => format!(
            "tree thread={} seq={} turn={}",
            snapshot.thread_id, snapshot.snapshot_seq, snapshot.turn_id
        ),
        SpineProjectionEvent::SpawnProgressUpdated(notification) => format!(
            "progress thread={} turn={}",
            notification.thread_id, notification.turn_id
        ),
        SpineProjectionEvent::ViewChanged { parent_thread_id } => {
            format!("view thread={parent_thread_id}")
        }
        SpineProjectionEvent::ClearIncompleteOverlays {
            parent_thread_id,
            turn_id,
        } => format!("clear-incomplete thread={parent_thread_id} turn={turn_id:?}"),
        SpineProjectionEvent::ClearCompletedTurnOverlays {
            parent_thread_id,
            turn_id,
        } => format!("clear-completed thread={parent_thread_id} turn={turn_id}"),
        SpineProjectionEvent::Invalidate { thread_id } => format!("invalidate thread={thread_id}"),
    };
    Some(format!("app epoch={epoch} {detail}"))
}

fn active_transcript(app: &App) -> String {
    app.chat_widget
        .active_cell_transcript_lines(/*width*/ 120)
        .unwrap_or_default()
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn spine_projection_summary(app: &App, thread_id: ThreadId) -> String {
    let Some(state) = app.spine_tree_views.get(&thread_id) else {
        return "none".to_string();
    };
    let snapshot = state
        .snapshot()
        .map(|snapshot| format!("{}:{}", snapshot.snapshot_seq, snapshot.turn_id))
        .unwrap_or_else(|| "none".to_string());
    let roots = state.incomplete_spawn_root_thread_ids(/*turn_id*/ None);
    let live = state
        .render_cell()
        .map(|cell| {
            cell.display_lines(/*width*/ 120)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default();
    format!("snapshot={snapshot} roots={roots:?} live={live:?}")
}

async fn projection_order_case(progress_first: bool) -> Result<String> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let turn_id = "turn-order";
    app.primary_thread_id = Some(parent_thread_id);
    app.chat_widget
        .handle_thread_session_quiet(test_thread_session(
            parent_thread_id,
            test_path_buf("/tmp/spine-projection-order"),
        ));
    let channel = ThreadEventChannel::new(/*capacity*/ 8);
    channel
        .store
        .lock()
        .await
        .push_notification(turn_started(parent_thread_id, turn_id));
    app.thread_event_channels.insert(parent_thread_id, channel);

    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let progress = ServerNotification::SpineSpawnProgressUpdated(spawn_progress(
        parent_thread_id,
        turn_id,
        child_thread_id,
    ));
    let tree = ServerNotification::SpineTreeUpdated(tree_snapshot(parent_thread_id, turn_id));
    let notifications = if progress_first {
        [progress, tree]
    } else {
        [tree, progress]
    };
    for notification in notifications {
        app.handle_app_server_event(
            &app_server,
            AppServerEvent::ServerNotification(Box::new(notification)),
        )
        .await;
        let _ = drain_app_events(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;
    }
    let rendered = active_transcript(&app);
    app_server.shutdown().await?;
    Ok(rendered)
}

#[tokio::test]
async fn live_tree_renders_for_both_snapshot_and_progress_arrival_orders() -> Result<()> {
    for progress_first in [true, false] {
        let rendered = projection_order_case(progress_first).await?;
        assert!(rendered.contains("live root task"), "{rendered}");
        assert!(rendered.contains("visible child task"), "{rendered}");
    }
    Ok(())
}

#[tokio::test]
async fn interacted_activity_updates_path_without_reviving_a_stopped_thread() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    let activity = |id: &str, path: &str| ThreadItem::SubAgentActivity {
        id: id.to_string(),
        kind: SubAgentActivityKind::Interacted,
        agent_thread_id: thread_id.to_string(),
        agent_path: path.to_string(),
    };

    app.agent_navigation.record_sub_agent_activity(
        crate::multi_agents::sub_agent_activity_display(&activity("interaction-1", "/root/child"))
            .expect("interacted activity should carry canonical metadata"),
    );
    assert_eq!(
        app.agent_navigation.get(&thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: None,
            agent_role: None,
            agent_path: Some("/root/child".to_string()),
            is_running: true,
            is_closed: false,
        })
    );

    app.agent_navigation.mark_closed(thread_id);
    app.agent_navigation.record_sub_agent_activity(
        crate::multi_agents::sub_agent_activity_display(&activity(
            "interaction-2",
            "/root/renamed-child",
        ))
        .expect("later interaction should still refresh the canonical path"),
    );
    assert_eq!(
        app.agent_navigation.get(&thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: None,
            agent_role: None,
            agent_path: Some("/root/renamed-child".to_string()),
            is_running: false,
            is_closed: true,
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn embedded_spine_spawn_streams_tree_activity_and_retires_children() -> Result<()> {
    let server = start_mock_server().await;
    let parent_spawn = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, PARENT_PROMPT)
                && !body_contains(request, BRANCH_PROMPT_MARKER)
                && !body_contains(request, "first child final")
        },
        sse(vec![
            ev_response_created("parent-spawn-response"),
            ev_reasoning_item("parent-reasoning", &["prepare visible spawn"], &[]),
            ev_function_call_with_namespace(SPAWN_CALL_ID, "spine", "spawn", &spawn_args()),
            ev_completed("parent-spawn-response"),
        ]),
    )
    .await;
    let first_child = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_request(request, "first-child-marker"),
        sse_response(sse(vec![
            ev_response_created("first-child-response"),
            ev_reasoning_item("first-child-reasoning", &["first child reasoning"], &[]),
            ev_assistant_message("first-child-message", "first child final"),
            ev_completed("first-child-response"),
        ]))
        .set_delay(Duration::from_millis(250)),
    )
    .await;
    let second_child = mount_response_once_match(
        &server,
        |request: &wiremock::Request| child_request(request, "second-child-marker"),
        sse_response(sse(vec![
            ev_response_created("second-child-response"),
            ev_reasoning_item("second-child-reasoning", &["second child reasoning"], &[]),
            ev_assistant_message("second-child-message", "second child final"),
            ev_completed("second-child-response"),
        ]))
        .set_delay(Duration::from_millis(50)),
    )
    .await;
    let parent_followup = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, "first child final")
                && body_contains(request, "second child final")
                && !body_contains(request, BRANCH_PROMPT_MARKER)
        },
        sse(vec![
            ev_response_created("parent-followup-response"),
            ev_assistant_message("parent-followup-message", "parent final"),
            ev_completed("parent-followup-response"),
        ]),
    )
    .await;

    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"
model = "{MODEL}"
model_provider = "{MODEL_PROVIDER_ID}"

[model_providers.{MODEL_PROVIDER_ID}]
name = "TUI Spine Spawn"
base_url = "{}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
supports_websockets = false

[features]
spine_jit = true
spine_spawn = true

[spine_spawn]
max_concurrent_threads_per_session = 3
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
    let parent_thread_id = started.session.thread_id;
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        started,
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    while app_event_rx.try_recv().is_ok() {}

    let turn = AppCommand::user_turn(
        vec![UserInput::Text {
            text: PARENT_PROMPT.to_string(),
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
    app.submit_thread_op(&mut app_server, parent_thread_id, turn)
        .await?;

    let mut child_thread_ids = HashSet::new();
    let mut child_thread_ids_by_summary = HashMap::new();
    let mut child_completion_order = Vec::new();
    let mut render_samples = Vec::new();
    let mut saw_tree = false;
    let mut saw_progress = false;
    let mut saw_settlement = false;
    let mut parent_completed = false;
    let mut projection_trace = Vec::new();
    let timeout = tokio::time::sleep(Duration::from_secs(/*secs*/ 15));
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            event = app_server.next_event() => {
                let event = event.expect("embedded app-server stream should remain open");
                if let AppServerEvent::ServerNotification(notification) = &event {
                    match notification.as_ref() {
                        ServerNotification::SpineTreeUpdated(notification) => {
                            saw_tree = true;
                            saw_settlement |= !notification.settled_spawn_call_ids.is_empty();
                            projection_trace.push(format!(
                                "tree thread={} seq={} turn={} settled={:?}",
                                notification.thread_id,
                                notification.snapshot_seq,
                                notification.turn_id,
                                notification.settled_spawn_call_ids
                            ));
                        }
                        ServerNotification::SpineSpawnProgressUpdated(notification) => {
                            saw_progress = true;
                            projection_trace.push(format!(
                                "progress thread={} turn={} statuses={:?}",
                                notification.thread_id,
                                notification.turn_id,
                                notification
                                    .tasks
                                    .iter()
                                    .map(|task| (&task.summary, &task.status))
                                    .collect::<Vec<_>>()
                            ));
                            for task in &notification.tasks {
                                if let Ok(thread_id) = ThreadId::from_string(&task.thread_id) {
                                    child_thread_ids.insert(thread_id);
                                    child_thread_ids_by_summary
                                        .insert(task.summary.clone(), thread_id);
                                }
                            }
                        }
                        ServerNotification::TurnStarted(notification)
                            if notification.thread_id == parent_thread_id.to_string() =>
                        {
                            projection_trace.push(format!("parent turn started {}", notification.turn.id));
                        }
                        ServerNotification::TurnCompleted(notification)
                            if notification.thread_id == parent_thread_id.to_string() =>
                        {
                            parent_completed = true;
                            projection_trace.push(format!("parent turn completed {}", notification.turn.id));
                        }
                        ServerNotification::TurnCompleted(notification) => {
                            child_completion_order.push(notification.thread_id.clone());
                            projection_trace.push(format!(
                                "child turn completed thread={} turn={}",
                                notification.thread_id, notification.turn.id
                            ));
                        }
                        _ => {}
                    }
                }
                app.handle_app_server_event(&app_server, event).await;
            }
            event = app_event_rx.recv() => {
                let event = event.expect("app event stream should remain open");
                if let Some(summary) = projection_event_summary(&event) {
                    projection_trace.push(summary);
                }
                app.handle_event(&mut tui, &mut app_server, event).await?;
            }
            () = &mut timeout => panic!("timed out waiting for embedded Spine Spawn"),
        }
        projection_trace.extend(
            drain_app_events(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?,
        );
        let rendered = active_transcript(&app);
        projection_trace.push(format!(
            "after drain active_turn={:?} epoch={} transcript={:?} {}",
            app.active_turn_id_for_thread(parent_thread_id).await,
            app.app_event_tx
                .current_spine_projection_epoch(parent_thread_id),
            rendered,
            spine_projection_summary(&app, parent_thread_id)
        ));
        if !rendered.is_empty() {
            let phase = if saw_settlement { "settled" } else { "live" };
            render_samples.push((phase, rendered));
        }
        if parent_completed
            && matching_request_count(
                &parent_followup,
                &["first child final", "second child final"],
                &[BRANCH_PROMPT_MARKER],
            ) == 1
            && app.settling_spine_spawn_threads.is_empty()
        {
            break;
        }
    }

    let rendered = render_samples
        .iter()
        .map(|(phase, rendered)| format!("[{phase}]\n{rendered}"))
        .collect::<Vec<_>>()
        .join("\n---\n");
    let tree_state = spine_projection_summary(&app, parent_thread_id);
    let projection_trace = projection_trace.join("\n");
    assert!(saw_tree);
    assert!(saw_progress);
    assert!(saw_settlement);
    assert_eq!(child_thread_ids.len(), 2);
    let first_child_thread_id = child_thread_ids_by_summary
        .get("first visible task")
        .copied()
        .expect("first task should expose its child thread id");
    let second_child_thread_id = child_thread_ids_by_summary
        .get("second visible task")
        .copied()
        .expect("second task should expose its child thread id");
    let completed_spawn_children = child_completion_order
        .iter()
        .filter_map(|thread_id| ThreadId::from_string(thread_id).ok())
        .filter(|thread_id| child_thread_ids.contains(thread_id))
        .collect::<Vec<_>>();
    assert_eq!(
        completed_spawn_children,
        vec![second_child_thread_id, first_child_thread_id],
        "child completion notifications must preserve the fixture's reverse order:\n{projection_trace}"
    );
    assert!(
        render_samples.iter().any(|(phase, rendered)| {
            *phase == "live"
                && rendered.contains("first visible task")
                && rendered.contains("second visible task")
                && rendered.contains("first child reasoning")
                && rendered.contains("first child final")
                && rendered.contains("second child reasoning")
                && rendered.contains("second child final")
        }),
        "the pre-settlement live viewport must contain both tasks and exact child activity:\n\
         {rendered}\nstate:\n{tree_state}\ntrace:\n{projection_trace}"
    );
    assert!(
        rendered.contains("first visible task"),
        "rendered:\n{rendered}\nstate:\n{tree_state}\ntrace:\n{projection_trace}"
    );
    assert!(
        rendered.contains("second visible task"),
        "rendered:\n{rendered}\nstate:\n{tree_state}\ntrace:\n{projection_trace}"
    );
    assert!(
        rendered.contains("first child reasoning") || rendered.contains("first child final"),
        "{rendered}"
    );
    assert!(
        rendered.contains("second child reasoning") || rendered.contains("second child final"),
        "{rendered}"
    );
    for child_thread_id in child_thread_ids {
        assert!(app.agent_navigation.get(&child_thread_id).is_none());
        assert!(!app.thread_event_channels.contains_key(&child_thread_id));
        assert!(app.abandoned_side_threads.contains(&child_thread_id));
    }
    assert_eq!(
        matching_request_count(
            &parent_spawn,
            &[PARENT_PROMPT],
            &[BRANCH_PROMPT_MARKER, "first child final"],
        ),
        1
    );
    assert_eq!(
        matching_request_count(
            &first_child,
            &[BRANCH_PROMPT_MARKER, "first-child-marker"],
            &[],
        ),
        1
    );
    assert_eq!(
        matching_request_count(
            &second_child,
            &[BRANCH_PROMPT_MARKER, "second-child-marker"],
            &[],
        ),
        1
    );
    assert_eq!(
        matching_request_count(
            &parent_followup,
            &["first child final", "second child final"],
            &[BRANCH_PROMPT_MARKER],
        ),
        1
    );

    app_server.shutdown().await?;
    Ok(())
}

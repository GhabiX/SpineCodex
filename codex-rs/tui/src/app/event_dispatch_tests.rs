use super::*;
use crate::app::test_support::make_test_app;
use crate::app::thread_events::ThreadEventChannel;
use crate::app_event_sender::AppEventSender;
use codex_app_server_protocol::AgentMessageDeltaNotification;
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SpineSpawnProgressUpdatedNotification;
use codex_app_server_protocol::SpineSpawnTaskProgress;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeKind;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use codex_app_server_protocol::ThreadClosedNotification;
use codex_app_server_protocol::ThreadRolledBackNotification;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::ThreadStatusChangedNotification;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc;

fn tree_snapshot(
    thread_id: ThreadId,
    turn_id: &str,
    snapshot_seq: u64,
    summary: &str,
    settled_spawn_call_ids: Vec<String>,
) -> SpineTreeUpdatedNotification {
    SpineTreeUpdatedNotification {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        snapshot_seq,
        active_node_id: "1".to_string(),
        nodes: vec![SpineTreeNode {
            node_id: "1".to_string(),
            parent_id: None,
            kind: SpineTreeNodeKind::Task,
            status: SpineTreeNodeStatus::Live,
            summary: Some(summary.to_string()),
            memory_summary: None,
            spawn_outcome: None,
            start: 0,
            end: None,
            context_pressure: None,
        }],
        settled_spawn_call_ids,
    }
}

fn spawn_progress(
    parent_thread_id: ThreadId,
    turn_id: &str,
    call_id: &str,
    child_thread_id: ThreadId,
) -> SpineSpawnProgressUpdatedNotification {
    SpineSpawnProgressUpdatedNotification {
        thread_id: parent_thread_id.to_string(),
        turn_id: turn_id.to_string(),
        call_id: call_id.to_string(),
        tasks: vec![SpineSpawnTaskProgress {
            ordinal: 0,
            summary: format!("spawn {child_thread_id}"),
            thread_id: child_thread_id.to_string(),
            agent_path: None,
            status: CollabAgentStatus::Running,
        }],
    }
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

fn agent_message_delta(thread_id: ThreadId, turn_id: &str, text: &str) -> ServerNotification {
    ServerNotification::AgentMessageDelta(AgentMessageDeltaNotification {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        item_id: "message".to_string(),
        delta: text.to_string(),
    })
}

fn turn_completed(thread_id: ThreadId, turn_id: &str) -> ServerNotification {
    ServerNotification::TurnCompleted(TurnCompletedNotification {
        thread_id: thread_id.to_string(),
        turn: Turn {
            id: turn_id.to_string(),
            items_view: codex_app_server_protocol::TurnItemsView::Full,
            items: Vec::new(),
            status: TurnStatus::Completed,
            error: None,
            started_at: Some(0),
            completed_at: Some(1),
            duration_ms: Some(1),
        },
    })
}

fn thread_closed(thread_id: ThreadId) -> ServerNotification {
    ServerNotification::ThreadClosed(ThreadClosedNotification {
        thread_id: thread_id.to_string(),
    })
}

fn thread_status_changed(thread_id: ThreadId, status: ThreadStatus) -> ServerNotification {
    ServerNotification::ThreadStatusChanged(ThreadStatusChangedNotification {
        thread_id: thread_id.to_string(),
        status,
    })
}

fn rendered_tree(state: &crate::history_cell::SpineTreeViewState) -> String {
    state
        .render_cell()
        .expect("Spine tree should remain live")
        .display_lines(/*width*/ 100)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn register_running_thread(app: &mut App, thread_id: ThreadId, turn_id: &str) {
    let channel = ThreadEventChannel::new(/*capacity*/ 4);
    channel
        .store
        .lock()
        .await
        .push_notification(turn_started(thread_id, turn_id));
    app.thread_event_channels.insert(thread_id, channel);
    app.agent_navigation.upsert(
        thread_id,
        /*agent_nickname*/ None,
        Some("worker".to_string()),
        /*is_closed*/ false,
    );
}

fn install_test_sender(app: &mut App) -> mpsc::UnboundedReceiver<AppEvent> {
    let (raw_tx, raw_rx) = mpsc::unbounded_channel();
    app.app_event_tx = AppEventSender::new(raw_tx);
    raw_rx
}

async fn handle_next_projection(
    app: &mut App,
    app_event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    tui: &mut tui::Tui,
    app_server: &mut AppServerSession,
) -> Result<()> {
    let event = app_event_rx
        .recv()
        .await
        .expect("projection event channel should remain open");
    app.handle_event(tui, app_server, event).await?;
    Ok(())
}

#[tokio::test]
async fn settled_spawn_retires_nested_subtree_and_fences_late_activity() -> Result<()> {
    let mut app = make_test_app().await;
    let mut app_event_rx = install_test_sender(&mut app);
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let grandchild_thread_id = ThreadId::new();
    let native_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    register_running_thread(&mut app, parent_thread_id, "turn-parent").await;
    register_running_thread(&mut app, child_thread_id, "turn-child").await;
    register_running_thread(&mut app, grandchild_thread_id, "turn-grandchild").await;
    register_running_thread(&mut app, native_thread_id, "turn-native").await;
    app.agent_navigation.mark_closed(native_thread_id);

    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;

    app.app_event_tx.send(AppEvent::UpsertSpineTreeCell {
        snapshot: tree_snapshot(parent_thread_id, "turn-parent", 1, "live", Vec::new()),
    });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;
    app.app_event_tx
        .send(AppEvent::UpsertSpineSpawnProgressCell {
            notification: spawn_progress(
                parent_thread_id,
                "turn-parent",
                "spawn-root",
                child_thread_id,
            ),
        });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;
    app.app_event_tx
        .send(AppEvent::UpsertSpineSpawnProgressCell {
            notification: spawn_progress(
                child_thread_id,
                "turn-child",
                "spawn-nested",
                grandchild_thread_id,
            ),
        });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;

    app.app_event_tx.send(AppEvent::UpsertSpineTreeCell {
        snapshot: tree_snapshot(
            parent_thread_id,
            "turn-parent",
            2,
            "settled",
            vec!["spawn-root".to_string()],
        ),
    });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;

    assert_eq!(
        app.agent_navigation.ordered_thread_ids(),
        vec![parent_thread_id, native_thread_id]
    );
    assert!(
        app.thread_event_channels.contains_key(&child_thread_id)
            && app
                .thread_event_channels
                .contains_key(&grandchild_thread_id)
            && app.settling_spine_spawn_threads.get(&child_thread_id) == Some(&parent_thread_id)
            && app.settling_spine_spawn_threads.get(&grandchild_thread_id)
                == Some(&parent_thread_id),
        "settled children stay hidden but drain their own FIFO streams"
    );
    assert!(app.thread_event_channels.contains_key(&native_thread_id));
    assert!(
        app.agent_navigation
            .get(&native_thread_id)
            .is_some_and(|entry| entry.is_closed)
    );

    app.enqueue_thread_notification(
        child_thread_id,
        agent_message_delta(
            child_thread_id,
            "turn-child",
            "final child result after parent settlement",
        ),
    )
    .await?;
    let rendered = rendered_tree(
        app.spine_tree_views
            .get(&parent_thread_id)
            .expect("parent tree state"),
    );
    assert!(
        rendered.contains("final child result after parent settlement"),
        "{rendered}"
    );

    app.enqueue_thread_notification(
        child_thread_id,
        turn_completed(child_thread_id, "turn-child"),
    )
    .await?;
    assert!(!app.thread_event_channels.contains_key(&child_thread_id));
    assert!(
        app.thread_event_channels
            .contains_key(&grandchild_thread_id)
    );

    app.enqueue_thread_notification(
        grandchild_thread_id,
        turn_completed(grandchild_thread_id, "turn-grandchild"),
    )
    .await?;
    assert!(app.settling_spine_spawn_threads.is_empty());
    assert!(!app.thread_event_channels.contains_key(&child_thread_id));
    assert!(
        !app.thread_event_channels
            .contains_key(&grandchild_thread_id)
    );
    assert!(app.abandoned_side_threads.contains(&child_thread_id));
    assert!(app.abandoned_side_threads.contains(&grandchild_thread_id));

    app.enqueue_thread_notification(
        grandchild_thread_id,
        turn_started(grandchild_thread_id, "turn-late"),
    )
    .await?;
    assert!(
        !app.thread_event_channels
            .contains_key(&grandchild_thread_id),
        "late descendant activity must not recreate a retired thread channel"
    );
    assert!(app.agent_navigation.get(&grandchild_thread_id).is_none());

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn settled_spawn_without_a_child_channel_drains_until_its_fifo_barrier() -> Result<()> {
    let mut app = make_test_app().await;
    let mut app_event_rx = install_test_sender(&mut app);
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    register_running_thread(&mut app, parent_thread_id, "turn-parent").await;

    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    app.app_event_tx.send(AppEvent::UpsertSpineTreeCell {
        snapshot: tree_snapshot(parent_thread_id, "turn-parent", 1, "live", Vec::new()),
    });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;
    app.app_event_tx
        .send(AppEvent::UpsertSpineSpawnProgressCell {
            notification: spawn_progress(
                parent_thread_id,
                "turn-parent",
                "spawn-fast",
                child_thread_id,
            ),
        });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;
    assert!(!app.thread_event_channels.contains_key(&child_thread_id));

    app.app_event_tx.send(AppEvent::UpsertSpineTreeCell {
        snapshot: tree_snapshot(
            parent_thread_id,
            "turn-parent",
            2,
            "settled",
            vec!["spawn-fast".to_string()],
        ),
    });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;

    assert!(app.agent_navigation.get(&child_thread_id).is_none());
    assert_eq!(
        app.settling_spine_spawn_threads.get(&child_thread_id),
        Some(&parent_thread_id)
    );
    assert!(!app.abandoned_side_threads.contains(&child_thread_id));
    assert!(!app.thread_event_channels.contains_key(&child_thread_id));

    app.enqueue_thread_notification(
        child_thread_id,
        agent_message_delta(
            child_thread_id,
            "turn-child",
            "fast child final result after parent settlement",
        ),
    )
    .await?;
    assert!(app.thread_event_channels.contains_key(&child_thread_id));
    let rendered = rendered_tree(
        app.spine_tree_views
            .get(&parent_thread_id)
            .expect("parent tree state"),
    );
    assert!(
        rendered.contains("fast child final result after parent settlement"),
        "{rendered}"
    );

    app.enqueue_thread_notification(
        child_thread_id,
        turn_completed(child_thread_id, "turn-child"),
    )
    .await?;
    assert!(!app.thread_event_channels.contains_key(&child_thread_id));
    assert!(
        !app.settling_spine_spawn_threads
            .contains_key(&child_thread_id)
    );
    assert!(app.abandoned_side_threads.contains(&child_thread_id));

    app.enqueue_thread_notification(
        child_thread_id,
        agent_message_delta(child_thread_id, "turn-late", "must stay fenced"),
    )
    .await?;
    assert!(!app.thread_event_channels.contains_key(&child_thread_id));
    assert!(
        !rendered_tree(
            app.spine_tree_views
                .get(&parent_thread_id)
                .expect("parent tree state"),
        )
        .contains("must stay fenced")
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn alternate_terminal_barriers_finish_hidden_spawn_retirement() -> Result<()> {
    let barriers: [fn(ThreadId) -> ServerNotification; 3] = [
        thread_closed,
        |thread_id| thread_status_changed(thread_id, ThreadStatus::NotLoaded),
        |thread_id| thread_status_changed(thread_id, ThreadStatus::SystemError),
    ];
    for barrier in barriers {
        let mut app = make_test_app().await;
        let mut app_event_rx = install_test_sender(&mut app);
        let parent_thread_id = ThreadId::new();
        let child_thread_id = ThreadId::new();
        app.primary_thread_id = Some(parent_thread_id);
        register_running_thread(&mut app, parent_thread_id, "turn-parent").await;

        let mut tui = crate::tui::test_support::make_test_tui()?;
        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        app.app_event_tx
            .send(AppEvent::UpsertSpineSpawnProgressCell {
                notification: spawn_progress(
                    parent_thread_id,
                    "turn-parent",
                    "spawn-terminal",
                    child_thread_id,
                ),
            });
        handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;
        app.app_event_tx
            .send(AppEvent::ClearIncompleteSpineOverlays {
                parent_thread_id,
                turn_id: Some("turn-parent".to_string()),
            });
        handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;

        assert_eq!(
            app.settling_spine_spawn_threads.get(&child_thread_id),
            Some(&parent_thread_id)
        );
        assert!(!app.abandoned_side_threads.contains(&child_thread_id));

        app.enqueue_thread_notification(child_thread_id, barrier(child_thread_id))
            .await?;
        assert!(!app.thread_event_channels.contains_key(&child_thread_id));
        assert!(
            !app.settling_spine_spawn_threads
                .contains_key(&child_thread_id)
        );
        assert!(app.abandoned_side_threads.contains(&child_thread_id));

        app.enqueue_thread_notification(
            child_thread_id,
            agent_message_delta(child_thread_id, "turn-late", "must stay fenced"),
        )
        .await?;
        assert!(!app.thread_event_channels.contains_key(&child_thread_id));

        app_server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn incomplete_spawn_cleanup_retires_cancelled_branch() -> Result<()> {
    let mut app = make_test_app().await;
    let mut app_event_rx = install_test_sender(&mut app);
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    register_running_thread(&mut app, parent_thread_id, "turn-parent").await;
    register_running_thread(&mut app, child_thread_id, "turn-child").await;

    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    app.app_event_tx
        .send(AppEvent::UpsertSpineSpawnProgressCell {
            notification: spawn_progress(
                parent_thread_id,
                "turn-parent",
                "spawn-cancelled",
                child_thread_id,
            ),
        });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;

    app.app_event_tx
        .send(AppEvent::ClearIncompleteSpineOverlays {
            parent_thread_id,
            turn_id: Some("turn-parent".to_string()),
        });
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;

    assert!(app.agent_navigation.get(&child_thread_id).is_none());
    assert!(app.thread_event_channels.contains_key(&child_thread_id));
    assert_eq!(
        app.settling_spine_spawn_threads.get(&child_thread_id),
        Some(&parent_thread_id)
    );

    app.enqueue_thread_notification(
        child_thread_id,
        turn_completed(child_thread_id, "turn-child"),
    )
    .await?;
    assert!(!app.thread_event_channels.contains_key(&child_thread_id));
    assert!(app.abandoned_side_threads.contains(&child_thread_id));
    assert!(
        !app.spine_tree_views
            .get(&parent_thread_id)
            .is_some_and(|state| state.has_spawn_call("spawn-cancelled"))
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn rollback_epochs_drop_queued_work_and_allow_low_sequence_replacement() -> Result<()> {
    let mut app = make_test_app().await;
    let mut app_event_rx = install_test_sender(&mut app);
    let thread_id = ThreadId::new();
    let other_thread_id = ThreadId::new();
    app.primary_thread_id = Some(thread_id);
    register_running_thread(&mut app, thread_id, "turn").await;
    let state = app.spine_tree_views.entry(thread_id).or_default();
    state.apply_tree_update(tree_snapshot(thread_id, "turn", 9, "initial", Vec::new()));
    state.apply_tree_update(tree_snapshot(thread_id, "turn", 10, "current", Vec::new()));
    app.spine_tree_views
        .entry(other_thread_id)
        .or_default()
        .apply_tree_update(tree_snapshot(
            other_thread_id,
            "turn-other",
            8,
            "unrelated",
            Vec::new(),
        ));

    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::SpineTreeUpdated(tree_snapshot(
            thread_id,
            "turn",
            11,
            "queued-old",
            Vec::new(),
        )),
    )
    .await?;
    app.app_event_tx.send(AppEvent::SpineTreeViewChanged {
        parent_thread_id: thread_id,
    });
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::SpineSpawnProgressUpdated(spawn_progress(
            thread_id,
            "turn",
            "stale-progress",
            ThreadId::new(),
        )),
    )
    .await?;
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::ThreadRolledBack(ThreadRolledBackNotification {
            thread_id: thread_id.to_string(),
        }),
    )
    .await?;
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::SpineTreeUpdated(tree_snapshot(
            thread_id,
            "turn",
            12,
            "between",
            Vec::new(),
        )),
    )
    .await?;
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::ThreadRolledBack(ThreadRolledBackNotification {
            thread_id: thread_id.to_string(),
        }),
    )
    .await?;
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::SpineTreeUpdated(tree_snapshot(
            thread_id,
            "turn",
            4,
            "replacement",
            Vec::new(),
        )),
    )
    .await?;
    assert_eq!(
        app.app_event_tx.current_spine_projection_epoch(thread_id),
        2
    );

    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    for index in 0..7 {
        handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;
        match index {
            0..=4 => {
                let state = app
                    .spine_tree_views
                    .get(&thread_id)
                    .expect("stale epochs must not invalidate or replace current state");
                assert_eq!(
                    state.snapshot().map(|snapshot| snapshot.snapshot_seq),
                    Some(10)
                );
                assert!(state.has_pending_history());
                assert!(!state.has_spawn_call("stale-progress"));
            }
            5 => assert!(!app.spine_tree_views.contains_key(&thread_id)),
            6 => assert_eq!(
                app.spine_tree_views
                    .get(&thread_id)
                    .and_then(crate::history_cell::SpineTreeViewState::snapshot)
                    .map(|snapshot| (snapshot.snapshot_seq, snapshot.nodes[0].summary.as_deref())),
                Some((4, Some("replacement")))
            ),
            _ => unreachable!(),
        }
    }
    assert!(app.spine_tree_views.contains_key(&other_thread_id));

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn discarded_side_thread_rejects_queued_and_late_projections() -> Result<()> {
    let mut app = make_test_app().await;
    let mut app_event_rx = install_test_sender(&mut app);
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    app.side_threads.insert(
        side_thread_id,
        crate::app::side::SideThreadState::new(parent_thread_id),
    );
    register_running_thread(&mut app, side_thread_id, "turn-side").await;

    app.app_event_tx.send(AppEvent::UpsertSpineTreeCell {
        snapshot: tree_snapshot(side_thread_id, "turn-side", 1, "queued", Vec::new()),
    });
    app.abandoned_side_threads.insert(side_thread_id);
    app.discard_thread_local_state(side_thread_id).await;

    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    handle_next_projection(&mut app, &mut app_event_rx, &mut tui, &mut app_server).await?;
    assert!(!app.spine_tree_views.contains_key(&side_thread_id));

    app.enqueue_thread_notification(
        side_thread_id,
        ServerNotification::SpineTreeUpdated(tree_snapshot(
            side_thread_id,
            "turn-side",
            2,
            "late",
            Vec::new(),
        )),
    )
    .await?;
    assert!(!app.thread_event_channels.contains_key(&side_thread_id));
    assert!(app.agent_navigation.get(&side_thread_id).is_none());
    assert!(app_event_rx.try_recv().is_err());

    app_server.shutdown().await?;
    Ok(())
}

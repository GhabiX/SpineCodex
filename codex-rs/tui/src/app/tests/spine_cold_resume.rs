use super::*;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeKind;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeSnapshot;
use pretty_assertions::assert_eq;

async fn replay_cold_tree(spine_enabled: bool) -> Result<()> {
    let (mut app, mut events, _op_rx) = make_test_app_with_channels().await;
    app.chat_widget
        .set_feature_enabled(Feature::SpineJit, spine_enabled);
    while events.try_recv().is_ok() {}
    let thread_id = ThreadId::new();
    let snapshot = ThreadEventSnapshot {
        delegated_turns: Vec::new(),
        session: Some(test_thread_session(
            thread_id,
            test_path_buf("/tmp/project"),
        )),
        turns: vec![test_turn(
            "cold-turn",
            TurnStatus::Completed,
            vec![
                ThreadItem::UserMessage {
                    id: "cold-user".to_string(),
                    client_id: None,
                    content: vec![AppServerUserInput::Text {
                        text: "Resume the saved work".to_string(),
                        text_elements: Vec::new(),
                    }],
                },
                ThreadItem::SpineTree {
                    id: "spine-tree:cold-turn".to_string(),
                    snapshot: SpineTreeSnapshot {
                        turn_id: "cold-turn".to_string(),
                        snapshot_seq: 7,
                        active_node_id: "root.1".to_string(),
                        nodes: vec![
                            SpineTreeNode {
                                node_id: "root".to_string(),
                                parent_id: None,
                                kind: SpineTreeNodeKind::RootEpoch,
                                status: SpineTreeNodeStatus::Live,
                                summary: None,
                                memory_summary: None,
                                spawn_outcome: None,
                                start: 0,
                                end: None,
                                context_pressure: None,
                            },
                            SpineTreeNode {
                                node_id: "root.1".to_string(),
                                parent_id: Some("root".to_string()),
                                kind: SpineTreeNodeKind::Task,
                                status: SpineTreeNodeStatus::Live,
                                summary: Some("Saved cold tree".to_string()),
                                memory_summary: None,
                                spawn_outcome: None,
                                start: 0,
                                end: None,
                                context_pressure: None,
                            },
                        ],
                        settled_spawn_call_ids: Vec::new(),
                        settled_spawn_thread_ids: Vec::new(),
                    },
                },
            ],
        )],
        events: Vec::new(),
        active_reasoning_item: None,
        input_state: None,
    };
    assert!(
        app.spine_tree_views.is_empty(),
        "must start with no live prior"
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);
    while let Ok(event) = events.try_recv() {
        Box::pin(app.handle_event(&mut tui, &mut app_server, event)).await?;
    }
    app_server.shutdown().await?;

    let tree_cells = app
        .transcript_cells
        .iter()
        .filter(|cell| cell.as_any().is::<history_cell::SpineTreeUpdateCell>())
        .collect::<Vec<_>>();
    let rendered = app
        .transcript_cells
        .iter()
        .flat_map(|cell| cell.display_lines(/*width*/ 120))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Resume the saved work"), "{rendered}");
    if spine_enabled {
        assert_eq!(
            tree_cells.len(),
            1,
            "cold replay must publish a history card: {rendered}"
        );
        let card = lines_to_single_string(&tree_cells[0].display_lines(/*width*/ 120));
        assert!(card.contains("Spine Tree"), "{card}");
        assert!(card.contains("Saved cold tree"), "{card}");
    } else {
        assert!(
            tree_cells.is_empty(),
            "OFF must not resurrect Spine UI: {rendered}"
        );
        assert!(app.spine_tree_views.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn cold_resume_renders_saved_spine_tree_in_app_history() -> Result<()> {
    replay_cold_tree(true).await
}

#[tokio::test]
async fn cold_resume_spine_off_keeps_native_history_without_tree() -> Result<()> {
    replay_cold_tree(false).await
}

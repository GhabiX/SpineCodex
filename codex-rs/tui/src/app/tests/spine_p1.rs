use super::*;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeKind;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn feedback_dispatch_preserves_optional_screenshots_over_json_rpc() -> Result<()> {
    let (mut app, mut events, _) = make_test_app_with_channels().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
    let thread_id = ThreadId::new();
    let (record_tx, mut records) = tokio::sync::mpsc::unbounded_channel();
    let proxy = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        super::disconnect::serve_reconnect_requests(
            tokio_tungstenite::accept_async(stream).await?,
            |request| {
                assert_eq!(request.method, "feedback/spineUpload");
                record_tx
                    .send(request.params.expect("typed upload params"))
                    .expect("record request");
                std::future::ready(Some(
                    serde_json::json!({"result": {"reportId": "local-report"}}),
                ))
            },
        )
        .await
    });
    let server = AppServerSession::new(
        crate::connect_remote_app_server(endpoint).await?,
        crate::app_server_session::ThreadParamsMode::Remote,
    );
    for screenshots in [
        vec![crate::clipboard_paste::PreparedFeedbackScreenshot {
            png: b"png".to_vec(),
            width: 1,
            height: 1,
        }],
        Vec::new(),
    ] {
        let expected = serde_json::json!({"threadId": thread_id.to_string(), "note": "report note",
            "screenshots": screenshots.iter().map(|_| serde_json::json!({"pngBase64": "cG5n"})).collect::<Vec<_>>()});
        let draft = crate::bottom_pane::SpineFeedbackDraft {
            thread_id,
            note: "  report note  ".into(),
            screenshots,
        };
        app.submit_spine_feedback(&server, draft);
        let actual = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), records.recv())
            .await?
            .expect("recorded upload");
        assert_eq!(actual, expected);
        let completion = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
            loop {
                if let Some(AppEvent::SpineFeedbackSubmitted {
                    request_generation,
                    draft,
                    result,
                }) = events.recv().await
                {
                    break (request_generation, draft, result);
                }
            }
        })
        .await?;
        assert_eq!(completion.2, Ok("local-report".into()));
        app.handle_spine_feedback_submitted(completion.0, completion.1, completion.2)
            .await;
        // Deliver success as the live thread route would, releasing its guard for the second upload.
        app.handle_feedback_thread_event(FeedbackThreadEvent::SpineSuccess {
            thread_id,
            request_generation: completion.0,
            report_id: "local-report".into(),
        });
    }
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

fn automatic_tree_cell() -> crate::history_cell::SpineTreeUpdateCell {
    let root = SpineTreeNode {
        node_id: "1".into(),
        parent_id: None,
        kind: SpineTreeNodeKind::Task,
        status: SpineTreeNodeStatus::Live,
        summary: Some("root task".into()),
        memory_summary: None,
        spawn_outcome: None,
        start: 0,
        end: None,
        context_pressure: None,
    };
    let mut snapshot = SpineTreeUpdatedNotification {
        thread_id: ThreadId::new().to_string(),
        turn_id: "turn-1".into(),
        snapshot_seq: 1,
        active_node_id: "1".into(),
        nodes: vec![root.clone()],
        settled_spawn_call_ids: Vec::new(),
        settled_spawn_thread_ids: Vec::new(),
    };
    let mut state = crate::history_cell::SpineTreeViewState::default();
    state.apply_tree_update(snapshot.clone());
    snapshot.snapshot_seq = 2;
    snapshot.active_node_id = "1.1".into();
    snapshot.nodes[0].status = SpineTreeNodeStatus::Opened;
    snapshot.nodes.push(SpineTreeNode {
        node_id: "1.1".into(),
        parent_id: Some("1".into()),
        summary: Some("nested task".into()),
        ..root
    });
    state.apply_tree_update(snapshot);
    state
        .take_pending_history_cell()
        .expect("accepted tree edge should emit automatic history")
}

#[tokio::test]
async fn automatic_tree_history_preserves_deferred_answer_consolidation() -> Result<()> {
    let mut app = make_test_app().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let before = plain_line_cell("earlier user context");
    let tree: Arc<dyn HistoryCell> = Arc::new(automatic_tree_cell());
    app.transcript_cells = vec![
        Arc::clone(&before),
        Arc::new(AgentMessageCell::new(
            vec![Line::from("partial answer")],
            /*is_first_line*/ true,
        )),
        Arc::clone(&tree),
    ];
    let source = "complete answer\n\n| Name | Value |\n| --- | --- |\n| Tail | retained |";
    let canonical = AgentMarkdownCell::new(source.to_string(), app.config.cwd.as_path());
    let expected = canonical.display_lines(80);
    app.handle_consolidate_agent_message(
        &mut tui,
        canonical,
        ConsolidationScrollbackReflow::Required,
        Some(Box::new(AgentMessageCell::new(
            vec![Line::from("deferred tail")],
            /*is_first_line*/ false,
        ))),
    )?;
    assert_eq!(app.transcript_cells.len(), 3);
    assert!(Arc::ptr_eq(&app.transcript_cells[0], &before));
    assert_eq!(app.transcript_cells[1].display_lines(80), expected);
    assert!(app.transcript_cells[1].as_any().is::<AgentMarkdownCell>());
    assert!(Arc::ptr_eq(&app.transcript_cells[2], &tree));
    Ok(())
}

#[tokio::test]
async fn automatic_tree_history_preserves_proposed_plan_consolidation() -> Result<()> {
    let mut app = make_test_app().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let before = plain_line_cell("earlier user context");
    let tree: Arc<dyn HistoryCell> = Arc::new(automatic_tree_cell());
    app.transcript_cells = vec![
        Arc::clone(&before),
        Arc::new(crate::history_cell::new_proposed_plan_stream(
            vec![Line::from("partial plan")],
            /*is_stream_continuation*/ false,
        )),
        Arc::clone(&tree),
    ];
    let source = "1. Inspect input\n2. Verify output";
    let expected =
        crate::history_cell::new_proposed_plan(source.into(), &app.config.cwd).display_lines(80);
    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::ConsolidateProposedPlan(source.into()),
    )
    .await?;
    assert_eq!(app.transcript_cells.len(), 3);
    assert!(Arc::ptr_eq(&app.transcript_cells[0], &before));
    assert_eq!(app.transcript_cells[1].display_lines(80), expected);
    assert!(Arc::ptr_eq(&app.transcript_cells[2], &tree));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn widget_replacement_preserves_feedback_generation_through_replay_and_old_callback() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    app.spine_feedback_in_flight.insert(thread_id, 2);
    app.spine_feedback_latest_generation.insert(thread_id, 2);
    app.chat_widget
        .set_spine_feedback_in_flight(thread_id, /*in_flight*/ true);
    let replacement = ChatWidget::new_with_app_event(ChatWidgetInit {
        requires_openai_auth: true,
        local_settings: crate::local_settings::LocalSettings::from(&app.config),
        config: app.config.clone(),
        frame_requester: crate::tui::FrameRequester::test_dummy(),
        app_event_tx: app.app_event_tx.clone(),
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: app.enhanced_keys_supported,
        has_chatgpt_account: app.chat_widget.has_chatgpt_account(),
        has_codex_backend_auth: app.chat_widget.has_codex_backend_auth(),
        model_catalog: app.model_catalog.clone(),
        feedback: app.feedback.clone(),
        is_first_run: false,
        status_account_display: app.chat_widget.status_account_display().cloned(),
        initial_plan_type: app.chat_widget.current_plan_type(),
        model: Some(app.chat_widget.current_model().to_string()),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
        terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
        session_telemetry: app.session_telemetry.clone(),
    });
    app.replace_chat_widget(replacement);
    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            delegated_turns: Vec::new(),
            session: None,
            turns: Vec::new(),
            events: Vec::new(),
            active_reasoning_item: None,
            input_state: None,
        },
        /*resume_restored_queue*/ false,
    );
    assert!(app.chat_widget.is_spine_feedback_in_flight(thread_id));
    let draft = crate::bottom_pane::SpineFeedbackDraft {
        thread_id,
        note: "older draft".into(),
        screenshots: Vec::new(),
    };
    app.handle_spine_feedback_submitted(1, draft.clone(), Err("old callback".into()))
        .await;
    app.handle_feedback_thread_event(FeedbackThreadEvent::SpineFailure {
        request_generation: 1,
        draft: draft.clone(),
        error: "old delivery".into(),
    });
    assert_eq!(app.spine_feedback_in_flight.get(&thread_id), Some(&2));
    assert!(app.chat_widget.is_spine_feedback_in_flight(thread_id));
    assert!(app.chat_widget.no_modal_or_popup_active());
    app.handle_feedback_thread_event(FeedbackThreadEvent::SpineFailure {
        request_generation: 2,
        draft,
        error: "current delivery".into(),
    });
    assert!(!app.spine_feedback_in_flight.contains_key(&thread_id));
    assert!(!app.chat_widget.is_spine_feedback_in_flight(thread_id));
    assert!(!app.chat_widget.no_modal_or_popup_active());
}

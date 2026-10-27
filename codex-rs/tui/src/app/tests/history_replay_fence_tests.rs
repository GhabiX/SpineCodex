use super::*;
use pretty_assertions::assert_eq;

fn snapshot(thread_id: ThreadId, prompt: &str) -> ThreadEventSnapshot {
    ThreadEventSnapshot {
        delegated_turns: Vec::new(),
        session: Some(test_thread_session(thread_id, test_path_buf("/tmp/replay"))),
        turns: vec![test_turn(
            "replay-turn",
            TurnStatus::Completed,
            vec![ThreadItem::UserMessage {
                id: "replay-user".to_string(),
                client_id: None,
                content: vec![AppServerUserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                }],
            }],
        )],
        events: Vec::new(),
        active_reasoning_item: None,
        input_state: None,
    }
}

#[tokio::test]
async fn own_queued_reset_preserves_latest_replay_after_another_switch() -> Result<()> {
    // Same-thread reselection must reject the old generation, not merely compare thread ids.
    for reselect_same_thread in [false, true] {
        let (mut app, mut events, _op_rx) = make_test_app_with_channels().await;
        let mut tui = crate::tui::test_support::make_test_tui()?;
        let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let first = ThreadId::new();
        let second = if reselect_same_thread {
            first
        } else {
            ThreadId::new()
        };
        for (thread_id, prompt) in [(first, "stale queued prompt"), (second, "current prompt")] {
            app.render_thread_snapshot(
                &mut tui,
                &server,
                thread_id,
                snapshot(thread_id, prompt),
                /*resume_restored_queue*/ false,
            )?;
        }
        while let Ok(event) = events.try_recv() {
            app.handle_event(&mut tui, &mut server, event).await?;
        }
        let prompts = app
            .transcript_cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<UserHistoryCell>()
                    .map(|user| user.message.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(prompts, vec!["current prompt"]);
        assert_eq!(app.pending_thread_switch_resets, 0);
        server.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn later_clear_rejects_already_queued_replay() -> Result<()> {
    let (mut app, mut events, _op_rx) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config).await?;
    let thread_id = ThreadId::new();
    app.render_thread_snapshot(
        &mut tui,
        &server,
        thread_id,
        snapshot(thread_id, "cleared queued prompt"),
        /*resume_restored_queue*/ false,
    )?;
    app.reset_transcript_state_after_clear();
    let mut skills_requests = 0;
    while let Ok(event) = events.try_recv() {
        match event {
            // Snapshot selection also requests native skill discovery. This fixture
            // has no live server thread; verify that request without dispatching it
            // as an active-thread operation and manufacturing an unrelated error cell.
            AppEvent::CodexOp(Op::ListSkills { cwds, force_reload }) => {
                assert_eq!(cwds, vec![test_path_buf("/tmp/replay")]);
                assert!(force_reload);
                skills_requests += 1;
            }
            event => {
                app.handle_event(&mut tui, &mut server, event).await?;
            }
        }
    }
    assert_eq!(skills_requests, 1);
    assert!(
        app.transcript_cells.is_empty(),
        "unexpected cells after clear: {:?}",
        app.transcript_cells
            .iter()
            .map(|cell| cell.display_lines(80))
            .collect::<Vec<_>>()
    );
    assert_eq!(app.pending_thread_switch_resets, 0);
    server.shutdown().await?;
    Ok(())
}

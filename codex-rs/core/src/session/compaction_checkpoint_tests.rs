use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn failed_checkpoint_keeps_the_previous_history_and_window() {
    let session = make_session_with_config(|config| {
        let _ = config.features.enable(Feature::SpineJit);
    })
    .await
    .expect("create Spine session");
    let turn_context = session.new_default_turn().await;
    session
        .record_conversation_items(&turn_context, &[user_message("history before compaction")])
        .await;
    let step_context = session
        .capture_step_context(Arc::clone(&turn_context), &CancellationToken::new())
        .await
        .expect("capture step");
    let world_state = Arc::new(
        session
            .build_world_state_for_step(&step_context)
            .await
            .expect("build world state"),
    );
    let before = {
        let state = session.state.lock().await;
        (
            state.history.annotated_items().to_vec(),
            state.clone_model_context().annotated_items().to_vec(),
            state.auto_compact_window_ids(),
        )
    };
    let previous_epoch = session
        .lock_spine_coordinator()
        .as_ref()
        .expect("Spine coordinator")
        .runtime
        .epoch();
    session
        .live_thread()
        .expect("persisted thread")
        .shutdown()
        .await
        .expect("close writer");

    session
        .start_new_context_window(&step_context, world_state)
        .await
        .expect_err("a closed writer cannot persist a new window");

    assert_eq!(
        session
            .lock_spine_coordinator()
            .as_ref()
            .expect("Spine coordinator")
            .runtime
            .epoch(),
        previous_epoch
    );
    let state = session.state.lock().await;
    assert_eq!(
        (
            state.history.annotated_items().to_vec(),
            state.clone_model_context().annotated_items().to_vec(),
            state.auto_compact_window_ids()
        ),
        before,
    );
}

#[tokio::test]
async fn fork_checkpoint_keeps_parent_guardian_evidence_local() -> anyhow::Result<()> {
    let session = make_session_with_config(|config| {
        let _ = config.features.enable(Feature::SpineJit);
    })
    .await?;
    let parent_checkpoint = codex_history::GuardianHistoryCheckpoint(vec![user_message(
        "parent-local review evidence",
    )]);
    session
        .state
        .lock()
        .await
        .history
        .restore_guardian_history(Some(&parent_checkpoint));
    let child_item = user_message("child-visible task");
    let history = session
        .checkpoint_spine_fork_context(&[], &[RolloutItem::ResponseItem(child_item.clone().into())])
        .await;
    let RolloutItem::Compacted(checkpoint) = &history[0] else {
        panic!("fork checkpoint");
    };
    assert_eq!(checkpoint.guardian_history, None);
    assert_eq!(
        checkpoint.replacement_history,
        Some(vec![child_item.into()])
    );
    assert_eq!(
        session.clone_history().await.guardian_history_checkpoint(),
        Some(parent_checkpoint)
    );
    Ok(())
}

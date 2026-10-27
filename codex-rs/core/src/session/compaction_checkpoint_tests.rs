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
        .record_conversation_items(
            &turn_context,
            turn_context.model_info(),
            &[user_message("history before compaction")],
        )
        .await;
    let step_context = session
        .capture_step_context(Arc::clone(&turn_context), &CancellationToken::new())
        .await
        .expect("capture step");
    let world_state = Arc::new(
        session
            .build_world_state_for_step(&step_context, /*new_window*/ true)
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
async fn fork_checkpoint_preserves_transformed_resume_and_retained_metadata() -> anyhow::Result<()>
{
    let session = make_session_with_config(|config| {
        let _ = config.features.enable(Feature::SpineJit);
    })
    .await?;
    let child_item = user_message("child-visible task");
    let mut transformed = session
        .checkpoint_spine_fork_context(&[], &[RolloutItem::ResponseItem(child_item.into())])
        .await;
    let answer = codex_history::VerifiedAnswer {
        turn_id: "answered-turn".to_string(),
        call_id: "question-call".to_string(),
        questions: vec![codex_history::VerifiedQuestionAnswer {
            question: "Which scope?".to_string(),
            answer: "Child scope".to_string(),
        }],
    };
    let expected_metadata = CompactionResumeMetadata {
        multi_agent_version: session.multi_agent_version(),
        last_started_turn_id: Some("answered-turn".to_string()),
        turn_attribution: None,
        previous_turn_settings: Some(codex_history::PreviousTurnSettings {
            model: "gpt-6.1-sol".to_string(),
            cyber_access_program: None,
            comp_hash: Some("child-compaction".to_string()),
            realtime_active: Some(false),
        }),
    };
    let RolloutItem::Compacted(source) = &mut transformed[0] else {
        panic!("source checkpoint");
    };
    source.resume_metadata = Some(expected_metadata.clone());
    source
        .retained_context
        .as_mut()
        .expect("retained context checkpoint")
        .record(&codex_history::RetainedContextEvent::VerifiedAnswer {
            answer: answer.clone(),
            acceptance_order: Some(1),
        });

    let history = session
        .checkpoint_spine_fork_context(&[], &transformed)
        .await;
    let RolloutItem::Compacted(checkpoint) = &history[0] else {
        panic!("fork checkpoint");
    };
    assert_eq!(checkpoint.resume_metadata, Some(expected_metadata));
    assert_eq!(
        checkpoint
            .retained_context
            .as_ref()
            .expect("retained context")
            .verified_answers()
            .cloned()
            .collect::<Vec<_>>(),
        vec![answer]
    );
    Ok(())
}

#[tokio::test]
async fn fork_checkpoint_keeps_parent_guardian_evidence_local() -> anyhow::Result<()> {
    let session = make_session_with_config(|config| {
        let _ = config.features.enable(Feature::SpineJit);
    })
    .await?;
    let parent_checkpoint = codex_history::GuardianHistoryCheckpoint(vec![
        user_message("parent-local review evidence").into(),
    ]);
    session.state.lock().await.history.restore_review_context(
        /*retained_context*/ None,
        Some(&parent_checkpoint),
        /*reviewer_compaction_hash*/ None,
    );
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

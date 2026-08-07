use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpineTransitionItem;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::is_persisted_rollout_item;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

#[test]
fn spine_transition_is_durable_but_not_model_visible() -> anyhow::Result<()> {
    let extension = RolloutItem::SpineTransition(SpineTransitionItem {
        version: 1,
        payload: serde_json::json!({
            "type": "sampling_shadow_v1",
            "record": {"digest": "diagnostic-only"}
        }),
    });

    let encoded = serde_json::to_vec(&extension)?;
    assert_eq!(
        serde_json::to_value(serde_json::from_slice::<RolloutItem>(&encoded)?)?,
        serde_json::to_value(&extension)?
    );
    assert!(is_persisted_rollout_item(
        &extension,
        ThreadHistoryMode::Legacy
    ));
    assert!(!super::is_spine_source_item(&extension));
    assert!(super::user_message_projection_entries(&[extension]).is_empty());
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoordinatorStep {
    Prepared,
    PersistenceAcknowledged,
    Installed,
    SourceAppended,
    CompactStarted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConcurrentMutation {
    SourceAppend,
    Compact,
}

#[tokio::test]
async fn spine_persistence_lock_order() {
    let sequence = Arc::new(Mutex::new(Vec::new()));
    let (persist_requested_tx, persist_requested_rx) = oneshot::channel();
    let (persist_ack_tx, persist_ack_rx) = oneshot::channel();
    let coordinator_sequence = Arc::clone(&sequence);
    let coordinator = tokio::spawn(async move {
        let mut state = coordinator_sequence.lock().await;
        state.push(CoordinatorStep::Prepared);
        persist_requested_tx
            .send(())
            .expect("test persistence worker should still be waiting");
        persist_ack_rx
            .await
            .expect("test persistence worker should acknowledge");
        state.push(CoordinatorStep::PersistenceAcknowledged);
        state.push(CoordinatorStep::Installed);
    });

    persist_requested_rx
        .await
        .expect("coordinator should request persistence while holding ownership");

    let (attempted_tx, mut attempted_rx) = mpsc::unbounded_channel();
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let mut mutations = Vec::new();
    for mutation in [
        ConcurrentMutation::SourceAppend,
        ConcurrentMutation::Compact,
    ] {
        let mutation_sequence = Arc::clone(&sequence);
        let attempted_tx = attempted_tx.clone();
        let entered_tx = entered_tx.clone();
        mutations.push(tokio::spawn(async move {
            attempted_tx
                .send(mutation)
                .expect("attempt receiver should remain open");
            let mut state = mutation_sequence.lock().await;
            entered_tx
                .send(mutation)
                .expect("entry receiver should remain open");
            state.push(match mutation {
                ConcurrentMutation::SourceAppend => CoordinatorStep::SourceAppended,
                ConcurrentMutation::Compact => CoordinatorStep::CompactStarted,
            });
        }));
    }
    drop(attempted_tx);
    drop(entered_tx);

    let mut attempted = vec![
        attempted_rx
            .recv()
            .await
            .expect("source append should attempt entry"),
        attempted_rx
            .recv()
            .await
            .expect("compact should attempt entry"),
    ];
    attempted.sort_by_key(|mutation| match mutation {
        ConcurrentMutation::SourceAppend => 0,
        ConcurrentMutation::Compact => 1,
    });
    assert_eq!(
        attempted,
        vec![
            ConcurrentMutation::SourceAppend,
            ConcurrentMutation::Compact
        ]
    );
    assert_eq!(
        entered_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty),
        "source append and compact must remain excluded until persistence is acknowledged and installed"
    );

    persist_ack_tx
        .send(())
        .expect("coordinator should still await durable acknowledgement");
    coordinator.await.expect("coordinator task should complete");
    for mutation in mutations {
        mutation.await.expect("mutation task should complete");
    }

    let steps = sequence.lock().await.clone();
    assert_eq!(
        &steps[..3],
        &[
            CoordinatorStep::Prepared,
            CoordinatorStep::PersistenceAcknowledged,
            CoordinatorStep::Installed,
        ]
    );
    let mut post_install = steps[3..].to_vec();
    post_install.sort_by_key(|step| match step {
        CoordinatorStep::SourceAppended => 0,
        CoordinatorStep::CompactStarted => 1,
        CoordinatorStep::Prepared
        | CoordinatorStep::PersistenceAcknowledged
        | CoordinatorStep::Installed => 2,
    });
    assert_eq!(
        post_install,
        vec![
            CoordinatorStep::SourceAppended,
            CoordinatorStep::CompactStarted,
        ]
    );
}

//! Stop a real LocalThreadStore writer at the restored-settings checkpoint, not
//! a fabricated handler error. The same store is shared by Core and the handler.
use super::*;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use tokio::sync::Mutex;

struct WriterFaultStore {
    inner: Arc<dyn ThreadStore>,
    entered: Notify,
    release: Notify,
    settings_left: AtomicUsize,
    fail_flush: AtomicBool,
    flush_pending: AtomicBool,
    resumed: Mutex<Option<ResumeThreadParams>>,
    observed: Mutex<Option<String>>,
    hold_revert: AtomicBool,
    fault_checkpoint: AtomicBool,
}

impl WriterFaultStore {
    fn wrap(inner: Arc<dyn ThreadStore>) -> Arc<Self> {
        assert!(inner.as_any().is::<LocalThreadStore>());
        Arc::new(Self {
            inner,
            entered: Notify::new(),
            release: Notify::new(),
            settings_left: AtomicUsize::new(0),
            fail_flush: AtomicBool::new(false),
            flush_pending: AtomicBool::new(false),
            resumed: Mutex::new(None),
            observed: Mutex::new(None),
            hold_revert: AtomicBool::new(false),
            fault_checkpoint: AtomicBool::new(false),
        })
    }

    async fn stop_writer(&self, thread_id: ThreadId) {
        self.entered.notify_one();
        self.release.notified().await;
        let shutdown = self.inner.shutdown_thread(thread_id).await;
        // The closing scenario already shut this writer down through real idle unload.
        assert!(matches!(
            shutdown,
            Ok(()) | Err(ThreadStoreError::ThreadNotFound { .. })
        ));
    }

    async fn observe(&self, result: &ThreadStoreResult<()>) {
        let error = result
            .as_ref()
            .expect_err("real writer operation must fail");
        assert!(matches!(error, ThreadStoreError::ThreadNotFound { .. }));
        *self.observed.lock().await = Some(error.to_string());
    }

    async fn recover_writer(&self) -> Result<()> {
        let mut params = self
            .resumed
            .lock()
            .await
            .clone()
            .context("resume parameters")?;
        params.history = None;
        params.history_revision = None;
        self.inner.resume_thread(params).await?;
        Ok(())
    }
}

impl ThreadStore for WriterFaultStore {
    fn as_any(&self) -> &dyn Any {
        self.inner.as_any()
    }
    fn default_history_mode(&self) -> codex_protocol::protocol::ThreadHistoryMode {
        self.inner.default_history_mode()
    }
    fn supports_paginated_history_lists(&self) -> bool {
        self.inner.supports_paginated_history_lists()
    }
    delegate_store! {
        create_thread(CreateThreadParams) -> ();
        shutdown_thread(ThreadId) -> ();
        discard_thread(ThreadId) -> ();
        load_history(LoadThreadHistoryParams) -> StoredThreadHistory;
        load_complete_history(LoadThreadHistoryParams) -> StoredThreadHistory;
        load_latest_model_context(LoadThreadHistoryParams) -> StoredModelContext;
        read_thread(ReadThreadParams) -> StoredThread;
        read_thread_by_rollout_path(ReadThreadByRolloutPathParams) -> StoredThread;
        list_threads(ListThreadsParams) -> ThreadPage;
        update_thread_metadata(UpdateThreadMetadataParams) -> Option<StoredThread>;
        list_items(ListItemsParams) -> ItemPage;
        list_turns(ListTurnsParams) -> TurnPage;
        archive_thread(ArchiveThreadParams) -> ();
        unarchive_thread(ArchiveThreadParams) -> StoredThread;
        delete_thread(DeleteThreadParams) -> ();
    }
    fn revert_thread(&self, params: RevertThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.inner.revert_thread(params).await?;
            if self.hold_revert.load(Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(())
        })
    }
    fn persist_thread(&self, id: ThreadId, context: PersistContext) -> ThreadStoreFuture<'_, ()> {
        self.inner.persist_thread(id, context)
    }
    fn resume_thread(
        &self,
        params: ResumeThreadParams,
    ) -> ThreadStoreFuture<'_, Arc<Vec<RolloutItem>>> {
        Box::pin(async move {
            let history = self.inner.resume_thread(params.clone()).await?;
            *self.resumed.lock().await = Some(params);
            // Resume writes its own checkpoint; fault the second one, after registration.
            self.settings_left.store(2, Ordering::SeqCst);
            Ok(history)
        })
    }
    fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            let checkpoint = params.items.iter().any(|item| {
                matches!(
                    item,
                    RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(_))
                )
            });
            let fault = self.fault_checkpoint.load(Ordering::SeqCst)
                && checkpoint
                && self
                    .settings_left
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                        left.checked_sub(1)
                    })
                    == Ok(1);
            if fault && self.fail_flush.load(Ordering::SeqCst) {
                self.inner.append_items(params).await?;
                self.flush_pending.store(true, Ordering::SeqCst);
                return Ok(());
            }
            if fault {
                self.stop_writer(params.thread_id).await;
                let result = self.inner.append_items(params).await;
                self.observe(&result).await;
                return result;
            }
            self.inner.append_items(params).await
        })
    }
    fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            if self.flush_pending.swap(false, Ordering::SeqCst) {
                self.stop_writer(thread_id).await;
                let result = self.inner.flush_thread(thread_id).await;
                self.observe(&result).await;
                return result;
            }
            self.inner.flush_thread(thread_id).await
        })
    }
}

impl Harness {
    async fn change_disabled_plugins(&mut self, id: i64, ids: Vec<&str>) -> Result<()> {
        self.request(json!({"id":id,"method":"thread/settings/update","params":{
            "threadId":self.thread_id.to_string(),"disabledPluginIds":ids
        }}))
        .await;
        let mut response_seen = false;
        let mut updated = None;
        while !response_seen || updated.is_none() {
            match self.envelope().await? {
                OutgoingEnvelope::ToConnection {
                    message: OutgoingMessage::Response(response),
                    ..
                } if response.id == RequestId::Integer(id) => {
                    response_seen = true;
                }
                OutgoingEnvelope::Broadcast {
                    message: OutgoingMessage::AppServerNotification(notification),
                }
                | OutgoingEnvelope::ToConnection {
                    message: OutgoingMessage::AppServerNotification(notification),
                    ..
                } => {
                    if let ServerNotification::ThreadSettingsUpdated(settings) =
                        notification.notification
                    {
                        updated = Some(settings);
                    }
                }
                _ => {}
            }
        }
        let updated = updated.unwrap();
        assert_eq!(updated.thread_id, self.thread_id.to_string());
        assert_eq!(updated.thread_settings.disabled_plugin_ids, ids);
        Ok(())
    }

    async fn silently_restore_different_settings(&self) -> Result<()> {
        let thread = self
            .thread_processor
            .thread_manager
            .get_thread(self.thread_id)
            .await?;
        assert!(
            thread
                .config_snapshot()
                .await
                .disabled_plugin_ids
                .is_empty()
        );
        let mut settings = thread.restorable_thread_settings().await;
        settings.disabled_plugin_ids = Some(vec!["slack@openai".into()]);
        // Runtime restoration is silent and does not change persisted resume settings.
        // Revert must reapply this in-memory snapshot over the different resume baseline.
        thread.restore_thread_settings(settings).await?;
        assert_eq!(
            thread.config_snapshot().await.disabled_plugin_ids,
            vec!["slack@openai".to_string()]
        );
        Ok(())
    }

    async fn revert_rpc(&self) {
        self.request(json!({"id":5,"method":"thread/revert","params":{
            "threadId":self.thread_id.to_string(),"beforeTurnId":self.turn_ids[1]
        }}))
        .await;
    }

    async fn assert_writer_failure(&mut self, fault: &WriterFaultStore) -> Result<()> {
        fault.release.notify_one();
        let error = loop {
            if let OutgoingEnvelope::ToConnection {
                message: OutgoingMessage::Error(error),
                ..
            } = self.envelope().await?
                && error.id == RequestId::Integer(5)
            {
                break error.error;
            }
        };
        let observed = fault
            .observed
            .lock()
            .await
            .clone()
            .context("real writer error")?;
        assert!(
            error
                .message
                .contains("failed to persist restored thread settings"),
            "{error:?}"
        );
        assert!(
            error.message.contains(&observed),
            "{error:?}; writer={observed}"
        );
        while let Ok(envelope) = self.outgoing.try_recv() {
            assert!(!matches!(&envelope,
                OutgoingEnvelope::Broadcast { message: OutgoingMessage::AppServerNotification(notification) }
                    | OutgoingEnvelope::ToConnection { message: OutgoingMessage::AppServerNotification(notification), .. }
                if matches!(notification.notification, ServerNotification::ThreadReverted(_))
            ));
            assert!(!matches!(envelope,
                OutgoingEnvelope::ToConnection { message: OutgoingMessage::Response(response), .. }
                if response.id == RequestId::Integer(5)
            ));
        }
        Ok(())
    }
}

async fn writer_harness() -> Result<(Harness, Arc<WriterFaultStore>)> {
    let slot = Arc::new(std::sync::OnceLock::new());
    let captured = Arc::clone(&slot);
    let h = Harness::with_store(Some(Box::new(move |inner| {
        let store = WriterFaultStore::wrap(inner);
        assert!(captured.set(Arc::clone(&store)).is_ok());
        store
    })))
    .await?;
    Ok((h, slot.get().unwrap().clone()))
}

async fn checkpoint_failure_keeps_listener(fail_flush: bool) -> Result<()> {
    let (mut h, fault) = writer_harness().await?;
    fault.fault_checkpoint.store(true, Ordering::SeqCst);
    fault.fail_flush.store(fail_flush, Ordering::SeqCst);
    h.silently_restore_different_settings().await?;
    h.revert_rpc().await;
    timeout(Duration::from_secs(/*secs*/ 10), fault.entered.notified()).await?;
    assert_eq!(h.persisted_turns().await?, h.turn_ids[..1]);
    h.assert_writer_failure(&fault).await?;
    // Repair only the writer, not the runtime/listener. A real follow-up must work.
    fault.recover_writer().await?;
    // Error activation must use the restored baseline too, not just success activation.
    h.change_disabled_plugins(21, vec![]).await?;
    h.turn(6, "third").await?;
    let requests = h.server.received_requests().await.context("requests")?;
    let body: serde_json::Value = requests.last().context("follow-up request")?.body_json()?;
    let inputs = body["input"].as_array().context("input")?;
    let texts: Vec<_> = inputs
        .iter()
        .filter(|item| item["role"] == "user")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|content| content["text"].as_str())
        .collect();
    assert!(texts.iter().any(|text| text.contains("first")));
    assert!(texts.iter().any(|text| text.contains("third")));
    assert!(!texts.iter().any(|text| text.contains("second")));
    assert_eq!(
        h.persisted_turns().await?,
        vec![h.turn_ids[0].clone(), h.turn_ids[2].clone()]
    );
    h.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn checkpoint_append_failure_keeps_replacement_listener() -> Result<()> {
    checkpoint_failure_keeps_listener(/*fail_flush*/ false).await
}

#[tokio::test]
async fn checkpoint_flush_failure_keeps_replacement_listener() -> Result<()> {
    checkpoint_failure_keeps_listener(/*fail_flush*/ true).await
}

#[tokio::test]
async fn closing_during_checkpoint_failure_does_not_revive_replacement() -> Result<()> {
    let (mut h, fault) = writer_harness().await?;
    fault.fault_checkpoint.store(true, Ordering::SeqCst);
    h.revert_rpc().await;
    timeout(Duration::from_secs(10), fault.entered.notified()).await?;
    let tx = h
        .thread_processor
        .thread_state_manager
        .current_listener_command_tx(h.thread_id)
        .context("replacement listener")?;
    let processor = Arc::clone(&h.processor);
    let session = Arc::clone(&h.session);
    // Real transport-close path. The product's 30s RPC drain deadline expires;
    // connection removal then triggers the held listener's ordinary idle unload.
    let close = tokio::spawn(async move {
        processor.connection_closed(ConnectionId(7), &session).await;
    });
    timeout(Duration::from_secs(45), tx.closed()).await?;
    close.await?;
    h.assert_writer_failure(&fault).await?;
    while h
        .thread_processor
        .thread_manager
        .get_thread(h.thread_id)
        .await
        .is_ok()
    {
        h.envelope().await?;
    }
    assert!(
        h.thread_processor
            .thread_state_manager
            .current_listener_command_tx(h.thread_id)
            .is_none()
    );
    assert_eq!(h.persisted_turns().await?, h.turn_ids[..1]);
    h.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn disconnect_during_reload_gap_preserves_replacement_until_idle_close() -> Result<()> {
    let (mut h, fault) = writer_harness().await?;
    let old_tx = h
        .thread_processor
        .thread_state_manager
        .current_listener_command_tx(h.thread_id)
        .context("original listener")?;
    let mut subscribers = h
        .thread_processor
        .thread_state_manager
        .subscribe_to_has_connections(h.thread_id)
        .await
        .context("subscriber watch")?;
    fault.hold_revert.store(true, Ordering::SeqCst);
    h.revert_rpc().await;
    timeout(Duration::from_secs(10), fault.entered.notified()).await?;
    // No disconnect yet: shutdown drain alone must finish the old listener.
    timeout(Duration::from_secs(10), old_tx.closed())
        .await
        .context("drained listener must exit before reload")?;
    let processor = Arc::clone(&h.processor);
    let session = Arc::clone(&h.session);
    let close = tokio::spawn(async move {
        processor.connection_closed(ConnectionId(7), &session).await;
    });
    timeout(Duration::from_secs(45), async {
        while *subscribers.borrow_and_update() {
            subscribers.changed().await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert!(!close.is_finished());
    fault.release.notify_one();
    let _: ThreadRevertResponse = serde_json::from_value(h.response(5).await?)?;
    close.await?;
    loop {
        if let OutgoingEnvelope::Broadcast {
            message: OutgoingMessage::AppServerNotification(notification),
        }
        | OutgoingEnvelope::ToConnection {
            message: OutgoingMessage::AppServerNotification(notification),
            ..
        } = h.envelope().await?
            && matches!(
                notification.notification,
                ServerNotification::ThreadClosed(_)
            )
        {
            break;
        }
    }
    assert!(
        h.thread_processor
            .thread_manager
            .get_thread(h.thread_id)
            .await
            .is_err()
    );
    assert!(
        h.thread_processor
            .thread_state_manager
            .current_listener_command_tx(h.thread_id)
            .is_none()
    );
    assert_eq!(h.persisted_turns().await?, h.turn_ids[..1]);
    h.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn restored_settings_baseline_notifies_real_change_and_deduplicates_same_value() -> Result<()>
{
    let mut h = Harness::new().await?;
    h.silently_restore_different_settings().await?;
    let revert = h.revert();
    timeout(
        Duration::from_secs(/*secs*/ 10),
        h.barrier.entered.notified(),
    )
    .await?;
    h.barrier.release.notify_one();
    assert!(revert.await?.is_ok());
    let _: ThreadRevertResponse = serde_json::from_value(h.response(5).await?)?;
    // Check the restored value FIRST: a change back to [] can mask a stale
    // baseline because restore also normalizes serviceTier=None to "default".
    h.request(json!({"id":22,"method":"thread/settings/update","params":{
        "threadId":h.thread_id.to_string(),"disabledPluginIds":["slack@openai"]
    }}))
    .await;
    h.request(json!({"id":23,"method":"turn/start","params":{
        "threadId":h.thread_id.to_string(),"input":[{"type":"text","text":"third"}]
    }}))
    .await;
    let (mut updated_response, mut turn_response, mut completed) = (false, false, false);
    // TurnCompleted is an ordered Core event after the duplicate settings event.
    // It proves consumption without using a quiet timeout as evidence of dedup.
    while !updated_response || !turn_response || !completed {
        match h.envelope().await? {
            OutgoingEnvelope::ToConnection {
                message: OutgoingMessage::Response(response),
                ..
            } => {
                updated_response |= response.id == RequestId::Integer(22);
                turn_response |= response.id == RequestId::Integer(23);
            }
            OutgoingEnvelope::Broadcast {
                message: OutgoingMessage::AppServerNotification(notification),
            }
            | OutgoingEnvelope::ToConnection {
                message: OutgoingMessage::AppServerNotification(notification),
                ..
            } => {
                assert!(
                    !matches!(
                        notification.notification,
                        ServerNotification::ThreadSettingsUpdated(_)
                    ),
                    "same-value update must not emit settings notification"
                );
                if let ServerNotification::TurnCompleted(turn) = notification.notification {
                    h.turn_ids.push(turn.turn.id);
                    completed = true;
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        h.persisted_turns().await?,
        vec![h.turn_ids[0].clone(), h.turn_ids[2].clone()]
    );
    h.change_disabled_plugins(24, vec![]).await?;
    h.shutdown().await;
    Ok(())
}

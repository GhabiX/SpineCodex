//! Real Core/store reloads with a fault at the response cursor read, and teardown
//! while that read is held. Native receipt queries still reach the real store.
//! No timing sleeps or fabricated handler return values.
use super::*;
use crate::message_processor::ConnectionSessionState;
use crate::message_processor::MessageProcessor;
use crate::message_processor::thread_lifecycle_tests::build_test_thread_processor_with_store;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use crate::transport::AppServerTransport;
use crate::transport::ConnectionOrigin;
use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::create_mock_responses_server_repeating_assistant;
use codex_core::config::ConfigBuilder;
use codex_thread_store::*;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::any::Any;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::time::timeout;
use wiremock::MockServer;

struct CursorReadBarrier {
    inner: Arc<dyn ThreadStore>,
    entered: Notify,
    release: Notify,
    fail: AtomicBool,
    hold_revert: AtomicBool,
}

macro_rules! delegate_store {
    ($($method:ident($param:ty) -> $result:ty;)*) => {$(
        fn $method(&self, params: $param) -> ThreadStoreFuture<'_, $result> {
            self.inner.$method(params)
        }
    )*};
}

#[path = "thread_revert_writer_tests.rs"]
mod writer_tests;

impl ThreadStore for CursorReadBarrier {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn default_history_mode(&self) -> codex_protocol::protocol::ThreadHistoryMode {
        self.inner.default_history_mode()
    }
    fn supports_paginated_history_lists(&self) -> bool {
        self.inner.supports_paginated_history_lists()
    }
    delegate_store! {
        create_thread(CreateThreadParams) -> ();
        resume_thread(ResumeThreadParams) -> Arc<Vec<RolloutItem>>;
        append_items(AppendThreadItemsParams) -> ();
        flush_thread(ThreadId) -> ();
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
        archive_thread(ArchiveThreadParams) -> ();
        unarchive_thread(ArchiveThreadParams) -> StoredThread;
        delete_thread(DeleteThreadParams) -> ();
    }
    fn persist_thread(&self, id: ThreadId, context: PersistContext) -> ThreadStoreFuture<'_, ()> {
        self.inner.persist_thread(id, context)
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

    fn list_turns(&self, params: ListTurnsParams) -> ThreadStoreFuture<'_, TurnPage> {
        // Match paginated_resume_backwards_cursors, not the pre-revert receipt scan.
        if !matches!(
            params,
            ListTurnsParams {
                include_archived: true,
                cursor: None,
                page_size: 1,
                sort_direction: codex_thread_store::SortDirection::Desc,
                items_view: StoredTurnItemsView::NotLoaded,
                ..
            }
        ) {
            return self.inner.list_turns(params);
        }
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            if self.fail.load(Ordering::SeqCst) {
                return Err(ThreadStoreError::Internal {
                    message: "cursor read unavailable".into(),
                });
            }
            self.inner.list_turns(params).await
        })
    }
}

struct Harness {
    _home: TempDir,
    server: MockServer,
    processor: Arc<MessageProcessor>,
    thread_processor: ThreadRequestProcessor,
    outgoing: mpsc::Receiver<OutgoingEnvelope>,
    session: Arc<ConnectionSessionState>,
    barrier: Arc<CursorReadBarrier>,
    thread_id: ThreadId,
    turn_ids: Vec<String>,
}

impl Harness {
    async fn new() -> Result<Self> {
        Self::with_store(None).await
    }

    async fn with_store(
        decorate: Option<crate::message_processor::thread_lifecycle_tests::StoreDecorator>,
    ) -> Result<Self> {
        let server = create_mock_responses_server_repeating_assistant("Done").await;
        let home = TempDir::new()?;
        MockResponsesConfig::new(&server.uri())
            .with_root_config("thread_unload_delay_secs = 1")
            .enable_feature(Feature::SpineJit)
            .write(home.path())?;
        let config = Arc::new(
            ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .build()
                .await?,
        );
        let auth = AuthManager::shared_from_config(
            config.as_ref(),
            /*enable_codex_api_key_env*/ false,
        )
        .await?;
        let (processor, mut thread_processor, outgoing) =
            build_test_thread_processor_with_store(config, auth, decorate).await;
        let barrier = Arc::new(CursorReadBarrier {
            inner: Arc::clone(&thread_processor.thread_store),
            entered: Notify::new(),
            release: Notify::new(),
            fail: AtomicBool::new(false),
            hold_revert: AtomicBool::new(false),
        });
        // Only the handler's cursor reads are faulted; Core keeps its real local store/writer.
        thread_processor.thread_store = barrier.clone();
        let mut harness = Self {
            _home: home,
            server,
            processor,
            thread_processor,
            outgoing,
            session: Arc::new(ConnectionSessionState::new(ConnectionOrigin::Stdio)),
            barrier,
            thread_id: ThreadId::new(),
            turn_ids: Vec::new(),
        };
        harness
            .request(json!({"id":1,"method":"initialize","params":{
                "clientInfo":{"name":"listener-test","version":"1"},
                "capabilities":{"experimentalApi":true}
            }}))
            .await;
        let response = harness.response(1).await?;
        let _: codex_app_server_protocol::InitializeResponse = serde_json::from_value(response)?;
        // Mirror the transport's outbound-ready bookkeeping after initialize.
        harness
            .processor
            .connection_initialized(
                ConnectionId(7),
                harness.session.request_attestation(),
                harness.session.experimental_api_enabled(),
            )
            .await;
        harness
            .request(json!({"id":2,"method":"thread/start","params":{"historyMode":"paginated"}}))
            .await;
        let started: ThreadStartResponse = serde_json::from_value(harness.response(2).await?)?;
        harness.thread_id = ThreadId::from_string(&started.thread.id)?;
        for (id, text) in [(3, "first"), (4, "second")] {
            harness.turn(id, text).await?;
        }
        Ok(harness)
    }

    async fn request(&self, value: serde_json::Value) {
        self.processor
            .process_request(
                ConnectionId(7),
                serde_json::from_value(value).unwrap(),
                &AppServerTransport::Stdio,
                Arc::clone(&self.session),
            )
            .await;
    }

    async fn envelope(&mut self) -> Result<OutgoingEnvelope> {
        let envelope = timeout(Duration::from_secs(/*secs*/ 10), self.outgoing.recv())
            .await
            .context("waiting for outgoing envelope")?
            .context("outgoing closed")?;
        Ok(envelope)
    }

    async fn response(&mut self, id: i64) -> Result<serde_json::Value> {
        loop {
            if let OutgoingEnvelope::ToConnection {
                message: OutgoingMessage::Response(response),
                ..
            } = self.envelope().await?
                && response.id == RequestId::Integer(id)
            {
                return Ok(serde_json::to_value(response.result)?);
            }
        }
    }

    async fn turn(&mut self, id: i64, text: &str) -> Result<()> {
        self.request(json!({"id":id,"method":"turn/start","params":{
            "threadId":self.thread_id.to_string(),"input":[{"type":"text","text":text}]
        }}))
        .await;
        let _: TurnStartResponse = serde_json::from_value(self.response(id).await?)?;
        loop {
            if let OutgoingEnvelope::Broadcast {
                message: OutgoingMessage::AppServerNotification(notification),
            }
            | OutgoingEnvelope::ToConnection {
                message: OutgoingMessage::AppServerNotification(notification),
                ..
            } = self.envelope().await?
                && let ServerNotification::TurnCompleted(completed) = notification.notification
            {
                self.turn_ids.push(completed.turn.id);
                return Ok(());
            }
        }
    }

    fn revert(
        &self,
    ) -> tokio::task::JoinHandle<Result<Option<ClientResponsePayload>, JSONRPCErrorError>> {
        let processor = self.thread_processor.clone();
        let params = ThreadRevertParams {
            thread_id: self.thread_id.to_string(),
            before_turn_id: self.turn_ids[1].clone(),
        };
        tokio::spawn(async move {
            processor
                .thread_revert(
                    ConnectionRequestId {
                        connection_id: ConnectionId(7),
                        request_id: RequestId::Integer(5),
                    },
                    params,
                    None,
                    None,
                )
                .await
        })
    }

    async fn persisted_turns(&self) -> Result<Vec<String>> {
        let page = self
            .barrier
            .inner
            .list_turns(ListTurnsParams {
                thread_id: self.thread_id,
                include_archived: true,
                cursor: None,
                page_size: 100,
                sort_direction: codex_thread_store::SortDirection::Asc,
                items_view: StoredTurnItemsView::NotLoaded,
            })
            .await?;
        Ok(page.turns.into_iter().map(|turn| turn.turn_id).collect())
    }

    async fn shutdown(self) {
        self.processor.shutdown_threads().await;
        self.thread_processor.clear_all_thread_listeners().await;
        self.processor.drain_background_tasks().await;
    }
}

#[tokio::test]
async fn reload_response_failure_keeps_replacement_listener_and_committed_history() -> Result<()> {
    let mut h = Harness::new().await?;
    h.barrier.fail.store(true, Ordering::SeqCst);
    let revert = h.revert();
    timeout(
        Duration::from_secs(/*secs*/ 10),
        h.barrier.entered.notified(),
    )
    .await?;
    let replacement = h
        .thread_processor
        .thread_manager
        .get_thread(h.thread_id)
        .await?;
    let state = h
        .thread_processor
        .thread_state_manager
        .thread_state(h.thread_id)
        .await;
    assert!(state.lock().await.listener_matches(&replacement));
    assert_eq!(h.persisted_turns().await?, h.turn_ids[..1]);
    h.barrier.release.notify_one();
    let error = revert
        .await?
        .expect_err("cursor failure must fail the response");
    assert!(
        error.message.contains("cursor read unavailable"),
        "{error:?}"
    );
    while let Ok(envelope) = h.outgoing.try_recv() {
        assert!(!matches!(envelope,
            OutgoingEnvelope::Broadcast { message: OutgoingMessage::AppServerNotification(notification) }
                | OutgoingEnvelope::ToConnection { message: OutgoingMessage::AppServerNotification(notification), .. }
            if matches!(notification.notification, ServerNotification::ThreadReverted(_))
        ));
    }
    // A live follow-up must complete through the replacement listener, with truncated history.
    h.turn(6, "third").await?;
    assert!(state.lock().await.listener_matches(&replacement));
    assert!(state.lock().await.active_turn_id().is_none());
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
    h.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn closing_prepared_replacement_cancels_without_success_then_error() -> Result<()> {
    let mut h = Harness::new().await?;
    let revert = h.revert();
    timeout(
        Duration::from_secs(/*secs*/ 10),
        h.barrier.entered.notified(),
    )
    .await?;
    let replacement = h
        .thread_processor
        .thread_manager
        .get_thread(h.thread_id)
        .await?;
    let state = h
        .thread_processor
        .thread_state_manager
        .thread_state(h.thread_id)
        .await;
    let command_tx = state
        .lock()
        .await
        .listener_command_tx()
        .context("registered listener commands")?;
    assert!(state.lock().await.listener_matches(&replacement));
    // Public unsubscribe must trigger normal idle unloading while publication is held.
    h.request(
        json!({"id":8,"method":"thread/unsubscribe","params":{"threadId":h.thread_id.to_string()}}),
    )
    .await;
    let _: ThreadUnsubscribeResponse = serde_json::from_value(h.response(8).await?)?;
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
    timeout(Duration::from_secs(/*secs*/ 10), command_tx.closed()).await?;
    h.barrier.release.notify_one();
    assert!(
        revert.await?.is_ok(),
        "committed revert must not return an error after success publication"
    );
    let _: ThreadRevertResponse = serde_json::from_value(h.response(5).await?)?;
    assert!(
        h.thread_processor
            .thread_manager
            .get_thread(h.thread_id)
            .await
            .is_err()
    );
    assert!(state.lock().await.cancel_tx.is_none());
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
async fn closed_listener_cleanup_preserves_cold_resume_registration() -> Result<()> {
    let mut h = Harness::new().await?;
    let revert = h.revert();
    timeout(
        Duration::from_secs(/*secs*/ 10),
        h.barrier.entered.notified(),
    )
    .await?;
    let replacement = h
        .thread_processor
        .thread_manager
        .get_thread(h.thread_id)
        .await?;
    let old_state = h
        .thread_processor
        .thread_state_manager
        .thread_state(h.thread_id)
        .await;
    let old_tx = old_state
        .lock()
        .await
        .listener_command_tx()
        .context("old listener")?;
    h.thread_processor
        .pending_thread_unloads
        .lock()
        .await
        .insert(h.thread_id, tokio::sync::watch::channel(()).0);
    super::super::thread_lifecycle::unload_thread_without_subscribers(
        h.thread_processor.thread_manager.clone(),
        h.thread_processor.outgoing.clone(),
        h.thread_processor.pending_thread_unloads.clone(),
        h.thread_processor.thread_state_manager.clone(),
        h.thread_processor.thread_watch_manager.clone(),
        h.thread_id,
        replacement,
    )
    .await;
    // On this current-thread runtime, take A's lock immediately after teardown. Its
    // cancelled task cannot finish cleanup until the guard is released below.
    let old_guard = old_state.lock().await;
    assert!(
        !old_tx.is_closed(),
        "old task must still be held before cleanup"
    );
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
    h.barrier.release.notify_one();
    assert!(revert.await?.is_ok());
    let _: ThreadRevertResponse = serde_json::from_value(h.response(5).await?)?;
    h.request(
        json!({"id":9,"method":"thread/resume","params":{"threadId":h.thread_id.to_string()}}),
    )
    .await;
    let _: ThreadResumeResponse = serde_json::from_value(h.response(9).await?)?;
    let new_state = h
        .thread_processor
        .thread_state_manager
        .thread_state(h.thread_id)
        .await;
    assert!(!Arc::ptr_eq(&old_state, &new_state));
    let new_tx = h
        .thread_processor
        .thread_state_manager
        .current_listener_command_tx(h.thread_id)
        .context("resumed listener")?;
    assert!(!new_tx.same_channel(&old_tx));
    drop(old_guard);
    timeout(Duration::from_secs(/*secs*/ 10), old_tx.closed()).await?;
    let registered = h
        .thread_processor
        .thread_state_manager
        .current_listener_command_tx(h.thread_id)
        .context("new registration lost to old cleanup")?;
    assert!(registered.same_channel(&new_tx));
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    registered.send(
        crate::thread_state::ThreadListenerCommand::ResolveServerRequest {
            request_id: RequestId::Integer(99),
            completion_tx,
        },
    )?;
    timeout(Duration::from_secs(/*secs*/ 10), completion_rx).await??;
    h.turn(10, "third").await?;
    assert_eq!(
        h.persisted_turns().await?,
        vec![h.turn_ids[0].clone(), h.turn_ids[2].clone()]
    );
    h.shutdown().await;
    Ok(())
}

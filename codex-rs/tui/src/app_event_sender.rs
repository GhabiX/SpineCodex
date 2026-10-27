//! Convenience sender for app events and common outbound TUI commands.
//!
//! This wraps the raw channel so call sites can submit typed `AppCommand`s
//! without duplicating event construction or session logging behavior. Snapshot replay uses local
//! sender clones to stamp history mutations with a thread and generation; live sender clones are
//! unaffected. Replacing or clearing the visible transcript invalidates old queued replay events.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use crate::app_command::AppCommand;
use crate::app_command::UserVerificationResponse;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::FileChangeApprovalDecision;
use codex_app_server_protocol::McpServerElicitationAction;
use codex_app_server_protocol::RequestId as AppServerRequestId;
use codex_app_server_protocol::ReviewTarget;
use codex_app_server_protocol::ToolRequestUserInputResponse;
use codex_protocol::ThreadId;
use codex_protocol::request_permissions::RequestPermissionsResponse;
use tokio::sync::mpsc::UnboundedSender;

use crate::app_event::AppEvent;
use crate::app_event::SpineProjectionEvent;
use crate::session_log;

#[derive(Clone, Debug)]
pub(crate) struct AppEventSender {
    pub app_event_tx: UnboundedSender<AppEvent>,
    pub(crate) voice_only: Arc<AtomicBool>,
    spine_projection_epochs: Arc<Mutex<HashMap<ThreadId, u64>>>,
    history_replay_epoch: Arc<AtomicU64>,
    history_replay_owner: Option<(ThreadId, u64)>,
}

impl AppEventSender {
    pub(crate) fn new(app_event_tx: UnboundedSender<AppEvent>) -> Self {
        Self {
            app_event_tx,
            voice_only: Arc::default(),
            spine_projection_epochs: Arc::new(Mutex::new(HashMap::new())),
            history_replay_epoch: Arc::new(AtomicU64::new(0)),
            history_replay_owner: None,
        }
    }

    /// Send an event to the app event channel. If it fails, we swallow the
    /// error and log it.
    pub(crate) fn send(&self, event: AppEvent) {
        // A parked voice owner keeps processing its session, but its UI and queued
        // typed work must not act on the visible session. Thread-scoped metadata still applies.
        if self.voice_only.load(Ordering::Relaxed)
            && !matches!(
                &event,
                AppEvent::CodexOp(
                    AppCommand::RealtimeConversationStart { .. }
                        | AppCommand::RealtimeConversationStop { .. }
                        | AppCommand::RealtimeConversationSpeech { .. }
                ) | AppEvent::RealtimeWebrtcOfferCreated { .. }
                    | AppEvent::RealtimeWebrtcConnected { .. }
                    | AppEvent::StopRealtimeConversation { .. }
                    | AppEvent::RealtimeConversationStateChanged
                    | AppEvent::BackgroundVoiceError { .. }
                    | AppEvent::SyncThreadGitBranch { .. }
                    | AppEvent::RefreshRateLimits { .. }
            )
        {
            return;
        }
        let event = self.stamp_spine_projection(event);
        // Keep existing history-cell session records; the queue fence is an internal envelope.
        // Avoid double-logging Ops; those are logged at the point of submission.
        if !matches!(event, AppEvent::CodexOp(_)) {
            session_log::log_inbound_app_event(&event);
        }
        let event = match (self.history_replay_owner, &event) {
            (
                Some((thread_id, epoch)),
                AppEvent::BeginInitialHistoryReplayBuffer
                | AppEvent::BeginThreadSwitchHistoryReplayBuffer
                | AppEvent::InsertHistoryCell(_)
                | AppEvent::EndInitialHistoryReplayBuffer
                | AppEvent::ConsolidateAgentMessage { .. }
                | AppEvent::ConsolidateProposedPlan(_),
            ) => AppEvent::ApplyHistoryReplay {
                thread_id,
                epoch,
                event: Box::new(event),
            },
            _ => event,
        };
        if let Err(e) = self.app_event_tx.send(event) {
            tracing::error!("failed to send event: {e}");
        }
    }

    /// Only this sender and its replay-local clones are stamped. Existing live senders remain
    /// unscoped, so asynchronous live events are not captured by a synchronous snapshot replay.
    pub(crate) fn for_history_replay(&self, thread_id: ThreadId) -> Self {
        let epoch = self
            .history_replay_epoch
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let mut sender = self.clone();
        sender.history_replay_owner = Some((thread_id, epoch));
        sender
    }

    pub(crate) fn current_history_replay_epoch(&self) -> u64 {
        self.history_replay_epoch.load(Ordering::Relaxed)
    }

    pub(crate) fn invalidate_history_replay(&self) {
        self.history_replay_epoch.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn current_spine_projection_epoch(&self, thread_id: ThreadId) -> u64 {
        self.spine_projection_epochs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&thread_id)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn stamp_spine_projection(&self, event: AppEvent) -> AppEvent {
        let projection = match event {
            AppEvent::UpsertSpineTreeCell { snapshot } => {
                SpineProjectionEvent::TreeUpdated(snapshot)
            }
            AppEvent::UpsertSpineSpawnProgressCell { notification } => {
                SpineProjectionEvent::SpawnProgressUpdated(notification)
            }
            AppEvent::SpineTreeViewChanged { parent_thread_id } => {
                SpineProjectionEvent::ViewChanged { parent_thread_id }
            }
            AppEvent::ClearIncompleteSpineOverlays {
                parent_thread_id,
                turn_id,
            } => SpineProjectionEvent::ClearIncompleteOverlays {
                parent_thread_id,
                turn_id,
            },
            AppEvent::ClearCompletedTurnSpineOverlays {
                parent_thread_id,
                turn_id,
            } => SpineProjectionEvent::ClearCompletedTurnOverlays {
                parent_thread_id,
                turn_id,
            },
            AppEvent::InvalidateSpineTreeView { thread_id } => {
                SpineProjectionEvent::Invalidate { thread_id }
            }
            other => return other,
        };
        let Some(thread_id) = projection.thread_id() else {
            return AppEvent::ApplySpineProjection {
                epoch: 0,
                event: projection,
            };
        };
        let epoch = if projection.invalidates() {
            let mut epochs = self
                .spine_projection_epochs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let epoch = epochs.entry(thread_id).or_default();
            *epoch = epoch.wrapping_add(1);
            *epoch
        } else {
            self.current_spine_projection_epoch(thread_id)
        };
        AppEvent::ApplySpineProjection {
            epoch,
            event: projection,
        }
    }

    pub(crate) fn interrupt(&self) {
        self.send(AppEvent::CodexOp(AppCommand::interrupt()));
    }

    pub(crate) fn compact(&self) {
        self.send(AppEvent::CodexOp(AppCommand::compact()));
    }

    pub(crate) fn set_thread_name(&self, name: String) {
        self.send(AppEvent::CodexOp(AppCommand::set_thread_name(name)));
    }

    pub(crate) fn review(&self, target: ReviewTarget) {
        self.send(AppEvent::CodexOp(AppCommand::review(target)));
    }

    pub(crate) fn list_skills(&self, cwds: Vec<PathBuf>, force_reload: bool) {
        self.send(AppEvent::CodexOp(AppCommand::list_skills(
            cwds,
            force_reload,
        )));
    }

    pub(crate) fn user_input_answer(
        &self,
        thread_id: ThreadId,
        id: String,
        response: ToolRequestUserInputResponse,
    ) {
        self.send(AppEvent::SubmitThreadOp {
            thread_id,
            op: AppCommand::user_input_answer(id, response),
        });
    }

    pub(crate) fn exec_approval(
        &self,
        thread_id: ThreadId,
        id: String,
        decision: CommandExecutionApprovalDecision,
    ) {
        self.send(AppEvent::SubmitThreadOp {
            thread_id,
            op: AppCommand::exec_approval(id, /*turn_id*/ None, decision),
        });
    }

    pub(crate) fn request_permissions_response(
        &self,
        thread_id: ThreadId,
        id: String,
        response: RequestPermissionsResponse,
    ) {
        self.send(AppEvent::SubmitThreadOp {
            thread_id,
            op: AppCommand::request_permissions_response(id, response),
        });
    }

    pub(crate) fn patch_approval(
        &self,
        thread_id: ThreadId,
        id: String,
        decision: FileChangeApprovalDecision,
    ) {
        self.send(AppEvent::SubmitThreadOp {
            thread_id,
            op: AppCommand::patch_approval(id, decision),
        });
    }

    pub(crate) fn resolve_elicitation(
        &self,
        thread_id: ThreadId,
        server_name: String,
        request_id: AppServerRequestId,
        decision: McpServerElicitationAction,
        content: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
    ) {
        self.send(AppEvent::SubmitThreadOp {
            thread_id,
            op: AppCommand::resolve_elicitation(server_name, request_id, decision, content, meta),
        });
    }

    pub(crate) fn resolve_user_verification(
        &self,
        thread_id: ThreadId,
        server_name: String,
        request_id: AppServerRequestId,
        response: UserVerificationResponse,
    ) {
        self.send(AppEvent::SubmitThreadOp {
            thread_id,
            op: AppCommand::resolve_user_verification(server_name, request_id, response),
        });
    }
}

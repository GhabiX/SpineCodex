use crate::agent::AgentStatus;
use crate::agent::control::SpawnAgentBatchRequest;
use crate::agent::control::SpawnAgentForkMode;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::next_thread_spawn_depth;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::session::MailboxSubmissionCancellation;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::handlers::multi_agents_common::build_agent_spawn_config;
use crate::tools::handlers::multi_agents_common::thread_spawn_source;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::SpineSpawnProgressEvent;
use codex_protocol::protocol::SpineSpawnTaskProgress;
use codex_protocol::user_input::UserInput;
use futures::future::join_all;
use spine_core::host::SPINE_SPAWN_RESULT_SCHEMA;
use spine_core::host::SpawnOutcome;
use spine_core::host::SpawnReceipt;
use spine_core::host::SpawnResult;
use spine_core::host::SpawnTask;
use spine_core::host::SpineTool;
use spine_core::host::ToolValidation;
use spine_core::host::ValidatedTransition;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const CORRECTION_MESSAGE: &str = concat!(
    "This spawned execution branch remains active. Continue exactly the declared\n",
    "assignment and follow its collaboration contract when one is declared. When the\n",
    "assignment is complete or precisely bounded, return exactly one non-empty,\n",
    "tool-free assistant final response containing terminal memory. That response\n",
    "ends this branch execution."
);

#[path = "spawn_recovery.rs"]
mod recovery;

#[derive(Clone, Default)]
pub(crate) struct SpawnLifecycle {
    shared: Arc<SpawnLifecycleShared>,
}

#[derive(Default)]
struct SpawnLifecycleShared {
    state: StdMutex<SpawnLifecycleState>,
    changed: Notify,
}

#[derive(Default)]
struct SpawnLifecycleState {
    active_transaction: Option<ActiveSpawnTransaction>,
    abort_barriers: usize,
}

struct ActiveSpawnTransaction {
    cancellation_token: CancellationToken,
    mailbox_cancellation: Option<MailboxSubmissionCancellation>,
}

pub(crate) struct SpawnTransactionGuard {
    shared: Arc<SpawnLifecycleShared>,
}

pub(crate) struct SpawnAbortBarrier {
    shared: Arc<SpawnLifecycleShared>,
    had_active_transaction: bool,
}

impl SpawnLifecycle {
    pub(crate) fn try_enter(
        &self,
        cancellation_token: CancellationToken,
    ) -> Option<SpawnTransactionGuard> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.abort_barriers > 0 || state.active_transaction.is_some() {
            return None;
        }
        state.active_transaction = Some(ActiveSpawnTransaction {
            cancellation_token,
            mailbox_cancellation: None,
        });
        Some(SpawnTransactionGuard {
            shared: Arc::clone(&self.shared),
        })
    }

    pub(crate) fn begin_abort(&self) -> SpawnAbortBarrier {
        let active_transaction = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.abort_barriers += 1;
            state.active_transaction.as_ref().map(|transaction| {
                (
                    transaction.cancellation_token.clone(),
                    transaction.mailbox_cancellation.clone(),
                )
            })
        };
        let had_active_transaction = active_transaction.is_some();
        if let Some((cancellation_token, mailbox_cancellation)) = active_transaction {
            cancellation_token.cancel();
            if let Some(cancellation) = mailbox_cancellation {
                cancellation.activate();
            }
        }
        SpawnAbortBarrier {
            shared: Arc::clone(&self.shared),
            had_active_transaction,
        }
    }
}

impl SpawnAbortBarrier {
    pub(crate) fn had_active_transactions(&self) -> bool {
        self.had_active_transaction
    }

    pub(crate) async fn wait_for_quiescence(&self) {
        loop {
            let changed = self.shared.changed.notified();
            if self
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active_transaction
                .is_none()
            {
                return;
            }
            changed.await;
        }
    }
}

impl Drop for SpawnTransactionGuard {
    fn drop(&mut self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_transaction
            .take();
        self.shared.changed.notify_waiters();
    }
}

impl SpawnTransactionGuard {
    fn install_mailbox_cancellation(&self, cancellation: MailboxSubmissionCancellation) {
        let should_activate = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let should_activate = state.abort_barriers > 0;
            if let Some(transaction) = state.active_transaction.as_mut() {
                transaction.mailbox_cancellation = Some(cancellation.clone());
            }
            should_activate
        };
        if should_activate {
            cancellation.activate();
        }
    }
}

impl Drop for SpawnAbortBarrier {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.abort_barriers = state.abort_barriers.saturating_sub(1);
    }
}

pub(crate) async fn execute(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    call_id: String,
    arguments: String,
    cancellation_token: CancellationToken,
) -> Result<(Vec<SpawnTask>, SpawnReceipt), String> {
    let tasks = parse_tasks(&arguments)?;
    let max_tasks = turn
        .config
        .spine_spawn
        .max_concurrent_threads_per_session
        .saturating_sub(1)
        .min(spine_core::host::MAX_SPAWN_TASKS);
    if tasks.len() > max_tasks {
        return Err(format!("spine.spawn accepts at most {max_tasks} tasks"));
    }

    let transaction_tasks = tasks.clone();
    let receipt = tokio::spawn(execute_transaction(
        session,
        turn,
        call_id,
        transaction_tasks,
        cancellation_token,
    ))
    .await
    .map_err(|error| format!("spine.spawn transaction task failed: {error}"))??;
    Ok((tasks, receipt))
}

pub(crate) fn parse_tasks(arguments: &str) -> Result<Vec<SpawnTask>, String> {
    let ToolValidation::Transition(ValidatedTransition::Spawn { tasks }) =
        spine_core::host::validate_tool(SpineTool::Spawn, arguments)
            .map_err(|error| error.to_string())?
    else {
        return Err("spine.spawn validation returned an unexpected result".to_string());
    };
    let mut summaries = HashSet::with_capacity(tasks.len());
    for (ordinal, task) in tasks.iter().enumerate() {
        let summary = task.summary.trim();
        if !summaries.insert(summary) {
            return Err(format!(
                "spine.spawn task {ordinal} has duplicate summary `{summary}`"
            ));
        }
    }
    validate_complete_child_inputs(&tasks)?;
    Ok(tasks)
}

/// Rejects a Spawn batch before reservation when any one child would receive a
/// model-visible input larger than the SDK's final provider-item boundary.
///
/// The child sees the rendered task envelope, not only the task's `prompt`
/// field. This check therefore includes its assignment, identity, peer roster,
/// the complete `ResponseItem`, JSON escaping, and the provider input-array
/// framing used by the normal child request path.
fn validate_complete_child_inputs(tasks: &[SpawnTask]) -> Result<(), String> {
    for (ordinal, task) in tasks.iter().enumerate() {
        let input = task_envelope(task, tasks);
        let item = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: input }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        crate::context::validate_spine_model_item(&item).map_err(|error| {
            format!("spine.spawn task {ordinal} produces an oversized child initial input: {error}")
        })?;
    }
    Ok(())
}

async fn execute_transaction(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    call_id: String,
    tasks: Vec<SpawnTask>,
    cancellation_token: CancellationToken,
) -> Result<SpawnReceipt, String> {
    let transaction_guard = session
        .spine_spawn_lifecycle
        .try_enter(cancellation_token.clone())
        .ok_or_else(|| {
            "spine.spawn cannot start while another transaction is active or aborting".to_string()
        })?;
    if cancellation_token.is_cancelled() {
        return Err("spine.spawn was cancelled before child creation".to_string());
    }

    let config = build_agent_spawn_config(
        &session.get_base_instructions().await,
        turn.as_ref(),
        turn.environments.primary(),
    )
    .map_err(|error| error.to_string())?;
    let child_depth = next_thread_spawn_depth(&turn.session_source);
    let parent_path = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let mut child_paths = Vec::with_capacity(tasks.len());
    let mut requests = Vec::with_capacity(tasks.len());
    for (ordinal, _) in tasks.iter().enumerate() {
        let source = thread_spawn_source(
            session.thread_id,
            &turn.session_source,
            child_depth,
            /*agent_role*/ None,
            Some(transaction_task_name(&call_id, ordinal)),
        )
        .map_err(|error| error.to_string())?;
        let child_path = source
            .get_agent_path()
            .ok_or_else(|| "spine.spawn child is missing an agent path".to_string())?;
        child_paths.push(child_path);
        requests.push(
            SpawnAgentBatchRequest::new(
                source,
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: Some(call_id.clone()),
                    fork_mode: Some(SpawnAgentForkMode::FullHistoryAtSamplingStart),
                    parent_thread_id: Some(session.thread_id),
                    parent_turn_id: Some(turn.sub_id.clone()),
                    environments: Some(turn.environments.to_selections()),
                },
            )
            .suppress_parent_completion_notification(),
        );
    }

    let prepared = match session
        .services
        .agent_control
        .prepare_agent_spawn_batch(config.clone(), requests)
        .await
    {
        Ok(prepared) => prepared,
        Err(error) => match error.details() {
            CodexErrorDetails::AgentLimitReached { max_threads } => {
                return capacity_rejection_receipt(&tasks, *max_threads);
            }
            _ => return Err(format!("spine.spawn admission failed: {error}")),
        },
    };
    if prepared.len() != tasks.len() {
        return Err(format!(
            "spine.spawn admission prepared {} of {} children",
            prepared.len(),
            tasks.len()
        ));
    }
    if cancellation_token.is_cancelled() {
        drop(prepared);
        return Err("spine.spawn was cancelled before child creation".to_string());
    }

    let starts = prepared.into_iter().zip(&tasks).map(|(prepared, task)| {
        session
            .services
            .agent_control
            .spawn_prepared_agent_with_metadata(
                prepared,
                vec![UserInput::Text {
                    text: task_envelope(task, &tasks),
                    text_elements: Vec::new(),
                }],
            )
    });
    let start_results = join_all(starts)
        .await
        .into_iter()
        .map(|result| result.map(|agent| agent.thread_id));
    let StartPhase {
        live,
        mut results,
        failed,
    } = classify_start_results(&child_paths, start_results);
    let child_thread_ids = live
        .iter()
        .map(|(_, thread_id, _)| *thread_id)
        .collect::<Vec<_>>();
    let child_by_path = live
        .iter()
        .map(|(_, thread_id, path)| (path.clone(), *thread_id))
        .collect::<HashMap<_, _>>();
    let mailbox_cancellation = session
        .input_queue
        .mailbox_submission_cancellation(&child_paths);
    transaction_guard.install_mailbox_cancellation(mailbox_cancellation.clone());
    if cancellation_token.is_cancelled() {
        mailbox_cancellation.activate();
    }

    if failed {
        let mut corrected_ids = HashSet::new();
        let teardown_result = teardown_transaction_children_with_correction(
            &session,
            &parent_path,
            &child_thread_ids,
            &child_paths,
            &child_by_path,
            &mut corrected_ids,
        )
        .await;
        quiesce_transaction_messages(
            &session,
            &parent_path,
            &child_paths,
            &child_by_path,
            &mut corrected_ids,
        )
        .await;
        teardown_result?;
        for (ordinal, thread_id, _) in &live {
            results[*ordinal] = Some(error_result(
                *ordinal,
                SpawnOutcome::Aborted,
                "child aborted because another transaction child failed to start".to_string(),
                Some(thread_id.to_string()),
            ));
        }
        return finish_receipt(&tasks, results);
    }

    let progress_tasks = Arc::new(tasks.clone());
    let progress_thread_ids = Arc::new(tokio::sync::Mutex::new(
        live.iter()
            .map(|(_, thread_id, _)| *thread_id)
            .collect::<Vec<_>>(),
    ));
    let progress_paths = Arc::new(child_paths.clone());
    let initial_statuses = join_all(
        live.iter()
            .map(|(_, thread_id, _)| session.services.agent_control.get_status(*thread_id)),
    )
    .await;
    let progress_statuses = Arc::new(tokio::sync::Mutex::new(
        live.iter()
            .zip(initial_statuses)
            .map(|((ordinal, thread_id, _), status)| {
                normalized_progress_status(*ordinal, *thread_id, status)
            })
            .collect::<Vec<_>>(),
    ));
    session
        .emit_spine_spawn_progress(
            turn.as_ref(),
            spawn_progress_event(
                &call_id,
                progress_tasks.as_ref(),
                &progress_thread_ids.lock().await,
                progress_paths.as_ref(),
                &progress_statuses.lock().await,
            ),
        )
        .await;

    recovery::finish_transaction(
        session,
        turn,
        call_id,
        tasks,
        cancellation_token,
        config,
        child_depth,
        parent_path,
        child_paths,
        live,
        results,
        mailbox_cancellation,
        progress_thread_ids,
        progress_statuses,
    )
    .await
}

async fn teardown_transaction_children(
    session: &Session,
    transaction_root_thread_ids: &[ThreadId],
) -> Result<(), String> {
    if transaction_root_thread_ids.is_empty() {
        return Ok(());
    }
    session
        .services
        .agent_control
        .shutdown_spine_spawn_subtrees(transaction_root_thread_ids)
        .await
        .map_err(|error| format!("spine.spawn child teardown failed: {error}"))
}

async fn teardown_transaction_children_with_correction(
    session: &Arc<Session>,
    parent_path: &AgentPath,
    transaction_root_thread_ids: &[ThreadId],
    transaction_roots: &[AgentPath],
    child_by_path: &HashMap<AgentPath, ThreadId>,
    corrected_ids: &mut HashSet<String>,
) -> Result<(), String> {
    let teardown = teardown_transaction_children(session.as_ref(), transaction_root_thread_ids);
    tokio::pin!(teardown);
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(25));
    loop {
        tokio::select! {
            result = &mut teardown => break result,
            _ = interval.tick() => {
                correct_intermediate_messages(
                    session,
                    parent_path,
                    transaction_roots,
                    child_by_path,
                    corrected_ids,
                ).await;
            }
        }
    }
}

async fn quiesce_transaction_messages(
    session: &Arc<Session>,
    parent_path: &AgentPath,
    transaction_roots: &[AgentPath],
    child_by_path: &HashMap<AgentPath, ThreadId>,
    corrected_ids: &mut HashSet<String>,
) {
    let quiescence = session.input_queue.wait_for_mailbox_submissions(|author| {
        author_is_in_transaction_subtree(author, transaction_roots)
    });
    tokio::pin!(quiescence);
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(25));
    loop {
        tokio::select! {
            _ = &mut quiescence => break,
            _ = interval.tick() => {
                correct_intermediate_messages(
                    session,
                    parent_path,
                    transaction_roots,
                    child_by_path,
                    corrected_ids,
                ).await;
            }
        }
    }
    correct_intermediate_messages(
        session,
        parent_path,
        transaction_roots,
        child_by_path,
        corrected_ids,
    )
    .await;
}

async fn correct_intermediate_messages(
    session: &Session,
    parent_path: &AgentPath,
    transaction_roots: &[AgentPath],
    child_by_path: &HashMap<AgentPath, ThreadId>,
    corrected_ids: &mut HashSet<String>,
) {
    let messages = session
        .input_queue
        .extract_mailbox_communications(|mail| {
            author_is_in_transaction_subtree(&mail.author, transaction_roots)
        })
        .await;
    for message in messages {
        if message
            .id
            .as_ref()
            .is_some_and(|identity| !corrected_ids.insert(identity.to_string()))
        {
            continue;
        }
        let Some(thread_id) = child_by_path.get(&message.author).copied().or_else(|| {
            session
                .services
                .agent_control
                .agent_id_for_path(&message.author)
        }) else {
            continue;
        };
        let correction = InterAgentCommunication::new(
            parent_path.clone(),
            message.author,
            Vec::new(),
            CORRECTION_MESSAGE.to_string(),
            /*trigger_turn*/ false,
        );
        let context =
            AgentCommunicationContext::new(AgentCommunicationKind::Message, session.thread_id);
        let _ = session
            .services
            .agent_control
            .send_inter_agent_communication(
                thread_id, correction, context, /*parent_turn_id*/ None,
            )
            .await;
    }
}

fn author_is_in_transaction_subtree(author: &AgentPath, transaction_roots: &[AgentPath]) -> bool {
    transaction_roots
        .iter()
        .any(|root| path_is_in_subtree(author, root))
}

fn path_is_in_subtree(candidate: &AgentPath, root: &AgentPath) -> bool {
    candidate == root
        || candidate
            .as_str()
            .strip_prefix(root.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
}

struct StartPhase {
    live: Vec<(usize, ThreadId, AgentPath)>,
    results: Vec<Option<SpawnResult>>,
    failed: bool,
}

fn classify_start_results<E: Display>(
    child_paths: &[AgentPath],
    start_results: impl IntoIterator<Item = Result<ThreadId, E>>,
) -> StartPhase {
    let mut live = Vec::with_capacity(child_paths.len());
    let mut results = vec![None; child_paths.len()];
    let mut failed = false;
    for (ordinal, start_result) in start_results.into_iter().enumerate() {
        match start_result {
            Ok(thread_id) => live.push((ordinal, thread_id, child_paths[ordinal].clone())),
            Err(error) => {
                failed = true;
                results[ordinal] = Some(error_result(
                    ordinal,
                    SpawnOutcome::Errored,
                    format!("child failed to start: {error}"),
                    /*execution_ref*/ None,
                ));
            }
        }
    }
    StartPhase {
        live,
        results,
        failed,
    }
}

fn transaction_task_name(call_id: &str, ordinal: usize) -> String {
    let fragment = call_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .take(20)
        .collect::<String>();
    let fragment = if fragment.is_empty() {
        "call"
    } else {
        &fragment
    };
    format!("spawn_{fragment}_{ordinal}")
}

fn task_envelope(task: &SpawnTask, call_tasks: &[SpawnTask]) -> String {
    let identity = task.summary.trim();
    let peers = call_tasks
        .iter()
        .map(|peer| peer.summary.trim())
        .filter(|summary| *summary != identity)
        .map(|summary| format!("- {summary}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        concat!(
            "You are a spawned execution branch. Your role is to complete exactly the assignment ",
            "below and return bounded terminal memory to the spawning continuation.\n\n",
            "You are: {}\n\nPeer branches in this spawn:\n{}\n\n",
            "The assignment is already an active branch scope. Begin the assigned work directly. ",
            "Use spine.open, spine.close, and spine.next only to manage genuine descendant work ",
            "within this assignment.\n\n",
            "Executable work is defined by the assignment. Inherited context supplies constraints ",
            "and evidence for that work.\n\n",
            "When the assignment declares a collaboration contract, follow its named root, peer ",
            "roles, artifact format, update/read protocol, synchronization points, and bounded ",
            "fallback. Inspect the coordination root before substantive work. Coordinate through it ",
            "to minimize unnecessary duplicate work: respect assigned scopes, share reusable ",
            "evidence early, and independently verify load-bearing or disputed claims. Within the ",
            "coordination root, write only your declared single-writer artifact and read peer ",
            "artifacts through the declared protocol, even when the investigated source and evidence ",
            "are otherwise read-only. Unless the contract provides locking or atomic append, preserve ",
            "your artifact append-only. Publish findings, conflicts, or requests useful to peers at ",
            "the declared synchronization points. Before returning your final response, perform the ",
            "declared final peer read and state which peer deltas you incorporated. Never write a ",
            "peer artifact, let collaboration expand the assignment, make completion depend on peer ",
            "state, or treat collaboration artifacts as correctness-critical evidence. If the root ",
            "or peer state is unavailable or incomplete, use the declared bounded fallback; do not ",
            "invent another coordination path.\n\n",
            "Other shared-workspace changes remain context for the assignment and do not add ",
            "executable work. Production-file ownership and any integration responsibility remain ",
            "exactly as declared in the assignment.\n\n",
            "Treat each <spine_tran_status> update as task-tree parser telemetry for this branch ",
            "session. Across status updates, executable work remains defined by the assignment.\n\n",
            "Complete this branch by returning exactly one non-empty, tool-free assistant final ",
            "response containing terminal memory. After returning it, execution ends.\n\n",
            "Assignment:\n{}"
        ),
        identity, peers, task.prompt
    )
}

async fn wait_for_terminal(
    control: &crate::agent::AgentControl,
    parent_path: &AgentPath,
    child_path: &AgentPath,
    parent_thread_id: ThreadId,
    parent_turn_id: String,
    thread_id: ThreadId,
) -> AgentStatus {
    let Ok(mut status_rx) = control.subscribe_status(thread_id).await else {
        return control.get_status(thread_id).await;
    };
    let mut final_message_reminded = false;
    loop {
        let status = status_rx.borrow_and_update().clone();
        if is_spawn_terminal(&status) {
            if is_missing_final_message(&status) && !final_message_reminded {
                final_message_reminded = true;
                let correction = InterAgentCommunication::new(
                    parent_path.clone(),
                    child_path.clone(),
                    Vec::new(),
                    CORRECTION_MESSAGE.to_string(),
                    /*trigger_turn*/ true,
                );
                let context = AgentCommunicationContext::new(
                    AgentCommunicationKind::Message,
                    parent_thread_id,
                );
                if let Err(error) = control
                    .send_inter_agent_communication(
                        thread_id,
                        correction,
                        context,
                        Some(parent_turn_id.clone()),
                    )
                    .await
                {
                    return AgentStatus::Errored(format!(
                        "failed to request missing final memory: {error}"
                    ));
                }
                if status_rx.changed().await.is_err() {
                    return control.get_status(thread_id).await;
                }
                continue;
            }
            return status;
        }
        if status_rx.changed().await.is_err() {
            return control.get_status(thread_id).await;
        }
    }
}

fn is_missing_final_message(status: &AgentStatus) -> bool {
    matches!(status, AgentStatus::Completed(None))
        || matches!(status, AgentStatus::Completed(Some(memory)) if memory.trim().is_empty())
}

fn is_spawn_terminal(status: &AgentStatus) -> bool {
    !matches!(status, AgentStatus::PendingInit | AgentStatus::Running)
}

fn result_from_status(ordinal: usize, thread_id: ThreadId, status: AgentStatus) -> SpawnResult {
    match status {
        AgentStatus::Completed(Some(memory)) if !memory.trim().is_empty() => SpawnResult {
            ordinal: ordinal as u32,
            outcome: SpawnOutcome::Completed,
            memory_body: memory,
            diagnostic: None,
            execution_ref: Some(thread_id.to_string()),
        },
        AgentStatus::Completed(_) => error_result(
            ordinal,
            SpawnOutcome::Errored,
            "child completed without a non-empty final memory".to_string(),
            Some(thread_id.to_string()),
        ),
        AgentStatus::Errored(error) => error_result(
            ordinal,
            SpawnOutcome::Errored,
            format!("child errored: {error}"),
            Some(thread_id.to_string()),
        ),
        AgentStatus::Shutdown => error_result(
            ordinal,
            SpawnOutcome::Aborted,
            "child shut down before returning final memory".to_string(),
            Some(thread_id.to_string()),
        ),
        AgentStatus::NotFound => error_result(
            ordinal,
            SpawnOutcome::Errored,
            "child was not found before returning final memory".to_string(),
            Some(thread_id.to_string()),
        ),
        AgentStatus::PendingInit | AgentStatus::Running | AgentStatus::Interrupted => error_result(
            ordinal,
            SpawnOutcome::Aborted,
            format!("child did not reach a terminal status: {status:?}"),
            Some(thread_id.to_string()),
        ),
    }
}

fn error_result(
    ordinal: usize,
    outcome: SpawnOutcome,
    diagnostic: String,
    execution_ref: Option<String>,
) -> SpawnResult {
    SpawnResult {
        ordinal: ordinal as u32,
        outcome,
        memory_body: diagnostic.clone(),
        diagnostic: Some(diagnostic),
        execution_ref,
    }
}

fn spawn_progress_event(
    call_id: &str,
    tasks: &[SpawnTask],
    thread_ids: &[ThreadId],
    paths: &[AgentPath],
    statuses: &[AgentStatus],
) -> SpineSpawnProgressEvent {
    SpineSpawnProgressEvent {
        call_id: call_id.to_string(),
        tasks: tasks
            .iter()
            .zip(thread_ids)
            .zip(paths)
            .zip(statuses)
            .enumerate()
            .map(
                |(ordinal, (((task, thread_id), path), status))| SpineSpawnTaskProgress {
                    ordinal: ordinal as u32,
                    summary: task.summary.clone(),
                    thread_id: *thread_id,
                    agent_path: Some(path.clone()),
                    status: status.clone(),
                },
            )
            .collect(),
    }
}

fn result_status(result: &SpawnResult) -> AgentStatus {
    match result.outcome {
        SpawnOutcome::Completed => AgentStatus::Completed(None),
        SpawnOutcome::Errored => AgentStatus::Errored(
            result
                .diagnostic
                .clone()
                .unwrap_or_else(|| result.memory_body.clone()),
        ),
        SpawnOutcome::Aborted => AgentStatus::Shutdown,
    }
}

fn normalized_progress_status(
    ordinal: usize,
    thread_id: ThreadId,
    status: AgentStatus,
) -> AgentStatus {
    if is_spawn_terminal(&status) {
        result_status(&result_from_status(ordinal, thread_id, status))
    } else {
        status
    }
}

async fn wait_for_terminal_after_resume(
    control: &crate::agent::AgentControl,
    parent_path: &AgentPath,
    child_path: &AgentPath,
    parent_thread_id: ThreadId,
    parent_turn_id: String,
    thread_id: ThreadId,
    mut status_rx: tokio::sync::watch::Receiver<AgentStatus>,
) -> AgentStatus {
    if status_rx.changed().await.is_err() {
        return control.get_status(thread_id).await;
    }
    let mut final_message_reminded = false;
    loop {
        let status = status_rx.borrow_and_update().clone();
        if is_spawn_terminal(&status) {
            if is_missing_final_message(&status) && !final_message_reminded {
                final_message_reminded = true;
                let correction = InterAgentCommunication::new(
                    parent_path.clone(),
                    child_path.clone(),
                    Vec::new(),
                    CORRECTION_MESSAGE.to_string(),
                    /*trigger_turn*/ true,
                );
                let context = AgentCommunicationContext::new(
                    AgentCommunicationKind::Message,
                    parent_thread_id,
                );
                if let Err(error) = control
                    .send_inter_agent_communication(
                        thread_id,
                        correction,
                        context,
                        Some(parent_turn_id.clone()),
                    )
                    .await
                {
                    return AgentStatus::Errored(format!(
                        "failed to request missing final memory: {error}"
                    ));
                }
                if status_rx.changed().await.is_err() {
                    return control.get_status(thread_id).await;
                }
                continue;
            }
            return status;
        }
        if status_rx.changed().await.is_err() {
            return control.get_status(thread_id).await;
        }
    }
}

fn capacity_rejection_receipt(
    tasks: &[SpawnTask],
    max_threads: usize,
) -> Result<SpawnReceipt, String> {
    let task_count = tasks.len();
    let results = tasks
        .iter()
        .enumerate()
        .map(|(ordinal, task)| {
            let task_number = ordinal.saturating_add(1);
            let diagnostic = format!(
                "spine.spawn task {task_number}/{task_count} (`{}`) was not started: aggregate \
                 admission requested {task_count} child agents, but shared capacity was unavailable \
                 under the configured limit of {max_threads} concurrent child agents. Admission is \
                 all-or-nothing, so no child agents from this batch were created.",
                task.summary
            );
            Some(error_result(
                ordinal,
                SpawnOutcome::Errored,
                diagnostic,
                /*execution_ref*/ None,
            ))
        })
        .collect();
    finish_receipt(tasks, results)
}

fn finish_receipt(
    tasks: &[SpawnTask],
    results: Vec<Option<SpawnResult>>,
) -> Result<SpawnReceipt, String> {
    let receipt = SpawnReceipt {
        schema: SPINE_SPAWN_RESULT_SCHEMA.to_string(),
        results: results
            .into_iter()
            .enumerate()
            .map(|(ordinal, result)| {
                result.unwrap_or_else(|| {
                    error_result(
                        ordinal,
                        SpawnOutcome::Errored,
                        "coordinator lost the child terminal result".to_string(),
                        /*execution_ref*/ None,
                    )
                })
            })
            .collect(),
    };
    receipt
        .validate_for(tasks)
        .map_err(|error| format!("spine.spawn produced an invalid receipt: {error}"))?;
    Ok(receipt)
}

#[cfg(test)]
#[path = "spawn_tests.rs"]
mod tests;

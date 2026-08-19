use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::user_input::UserInput;
use futures::future::join_all;
use spine_core::host::SpawnOutcome;
use spine_core::host::SpawnReceipt;
use spine_core::host::SpawnResult;
use spine_core::host::SpawnTask;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentStatus;
use crate::agent::control::SpawnAgentBatchRequest;
use crate::agent::control::SpawnAgentForkMode;
use crate::agent::control::SpawnAgentOptions;
use crate::config::Config;
use crate::session::MailboxSubmissionCancellation;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::spine::spawn_gate::SpawnFailureAction;
use crate::spine::spawn_gate::request_spawn_failure_action;
use crate::tools::handlers::multi_agents_common::thread_spawn_source;

use super::correct_intermediate_messages;
use super::error_result;
use super::finish_receipt;
use super::is_spawn_terminal;
use super::normalized_progress_status;
use super::quiesce_transaction_messages;
use super::result_from_status;
use super::result_status;
use super::spawn_progress_event;
use super::task_envelope;
use super::teardown_transaction_children_with_correction;
use super::transaction_task_name;
use super::wait_for_terminal;
use super::wait_for_terminal_after_resume;

const CONTINUE_AFTER_FAILURE_MESSAGE: &str = concat!(
    "Continue the same assignment from this branch's existing context. Preserve useful progress ",
    "from the failed turn, finish the remaining work, and return the required terminal memory."
);

struct AttemptWait {
    ordinal: usize,
    thread_id: ThreadId,
    resume_status: Option<watch::Receiver<AgentStatus>>,
}

impl AttemptWait {
    fn initial(ordinal: usize, thread_id: ThreadId) -> Self {
        Self {
            ordinal,
            thread_id,
            resume_status: None,
        }
    }

    fn resumed(
        ordinal: usize,
        thread_id: ThreadId,
        resume_status: watch::Receiver<AgentStatus>,
    ) -> Self {
        Self {
            ordinal,
            thread_id,
            resume_status: Some(resume_status),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_transaction(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    call_id: String,
    tasks: Vec<SpawnTask>,
    cancellation_token: CancellationToken,
    config: Config,
    child_depth: i32,
    parent_path: AgentPath,
    child_paths: Vec<AgentPath>,
    live: Vec<(usize, ThreadId, AgentPath)>,
    mut results: Vec<Option<SpawnResult>>,
    mailbox_cancellation: MailboxSubmissionCancellation,
    progress_thread_ids: Arc<tokio::sync::Mutex<Vec<ThreadId>>>,
    progress_statuses: Arc<tokio::sync::Mutex<Vec<AgentStatus>>>,
) -> Result<SpawnReceipt, String> {
    let failure_gate_enabled = parent_path == AgentPath::root();
    let progress_tasks = Arc::new(tasks.clone());
    let progress_paths = Arc::new(child_paths.clone());
    let mut child_by_path = live
        .iter()
        .map(|(_, thread_id, path)| (path.clone(), *thread_id))
        .collect::<HashMap<_, _>>();
    let mut current_thread_ids = vec![None; tasks.len()];
    for (ordinal, thread_id, _) in &live {
        current_thread_ids[*ordinal] = Some(*thread_id);
    }

    let mut corrected_ids = HashSet::new();
    let mut waits = live
        .iter()
        .map(|(ordinal, thread_id, _)| AttemptWait::initial(*ordinal, *thread_id))
        .collect::<Vec<_>>();
    let mut cancelled = false;
    let mut gate_cancelled = false;
    let mut gate_round = 1_u32;
    let mut fatal_error = None;

    'attempts: loop {
        let attempted = waits
            .iter()
            .map(|wait| (wait.ordinal, wait.thread_id))
            .collect::<Vec<_>>();
        let terminal = wait_for_attempts(
            &session,
            &turn,
            &cancellation_token,
            &parent_path,
            &child_paths,
            &child_by_path,
            &mut corrected_ids,
            &call_id,
            &progress_tasks,
            &progress_thread_ids,
            &progress_paths,
            &progress_statuses,
            waits,
        )
        .await;
        let Some(completed_results) = terminal else {
            cancelled = true;
            for (ordinal, thread_id) in attempted {
                let status = session.services.agent_control.get_status(thread_id).await;
                results[ordinal] = Some(if is_spawn_terminal(&status) {
                    result_from_status(ordinal, thread_id, status)
                } else {
                    aborted_result(ordinal, Some(thread_id))
                });
            }
            break;
        };
        for (ordinal, result) in completed_results {
            results[ordinal] = Some(result);
        }

        let failed_ordinals = results
            .iter()
            .enumerate()
            .filter_map(|(ordinal, result)| {
                result
                    .as_ref()
                    .is_some_and(|result| result.outcome != SpawnOutcome::Completed)
                    .then_some(ordinal)
            })
            .collect::<Vec<_>>();
        if failed_ordinals.is_empty() || !failure_gate_enabled {
            break;
        }

        let gate_call_id = format!("{call_id}:failure_gate:{gate_round}");
        let decision = tokio::select! {
            biased;
            _ = cancellation_token.cancelled() => {
                cancelled = true;
                None
            }
            decision = request_spawn_failure_action(
                &session,
                &turn,
                &gate_call_id,
                failed_ordinals.len(),
                tasks.len(),
            ) => decision,
        };
        let Some(decision) = decision else {
            if !cancelled {
                gate_cancelled = true;
            }
            break;
        };
        gate_round = gate_round.saturating_add(1);

        match decision.action {
            SpawnFailureAction::Abandon => break,
            SpawnFailureAction::Continue => {
                waits = continue_failed_branches(
                    &session,
                    &turn,
                    &failed_ordinals,
                    &current_thread_ids,
                    &child_paths,
                    &mut results,
                    &call_id,
                    &progress_tasks,
                    &progress_thread_ids,
                    &progress_paths,
                    &progress_statuses,
                    decision.note.as_deref(),
                )
                .await;
            }
            SpawnFailureAction::Retry => {
                let retry_thread_ids = failed_ordinals
                    .iter()
                    .filter_map(|ordinal| current_thread_ids[*ordinal])
                    .collect::<Vec<_>>();
                let retry_paths = failed_ordinals
                    .iter()
                    .map(|ordinal| child_paths[*ordinal].clone())
                    .collect::<Vec<_>>();
                let teardown_result = teardown_transaction_children_with_correction(
                    &session,
                    &parent_path,
                    &retry_thread_ids,
                    &retry_paths,
                    &child_by_path,
                    &mut corrected_ids,
                )
                .await;
                quiesce_transaction_messages(
                    &session,
                    &parent_path,
                    &retry_paths,
                    &child_by_path,
                    &mut corrected_ids,
                )
                .await;
                if let Err(error) = teardown_result {
                    fatal_error = Some(error);
                    break 'attempts;
                }
                for ordinal in &failed_ordinals {
                    current_thread_ids[*ordinal] = None;
                    child_by_path.remove(&child_paths[*ordinal]);
                }
                if cancellation_token.is_cancelled() {
                    cancelled = true;
                    break;
                }

                let retry_requests = (|| -> Result<Vec<_>, String> {
                    let mut requests = Vec::with_capacity(failed_ordinals.len());
                    for ordinal in &failed_ordinals {
                        let source = thread_spawn_source(
                            session.thread_id,
                            &turn.session_source,
                            child_depth,
                            /*agent_role*/ None,
                            Some(transaction_task_name(&call_id, *ordinal)),
                        )
                        .map_err(|error| error.to_string())?;
                        let retry_path = source.get_agent_path().ok_or_else(|| {
                            "spine.spawn retry child is missing an agent path".to_string()
                        })?;
                        if retry_path != child_paths[*ordinal] {
                            return Err("spine.spawn retry changed a child agent path".to_string());
                        }
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
                    Ok(requests)
                })();
                let retry_requests = match retry_requests {
                    Ok(requests) => requests,
                    Err(error) => {
                        fatal_error = Some(error);
                        break 'attempts;
                    }
                };

                let prepared = match session
                    .services
                    .agent_control
                    .prepare_agent_spawn_batch(config.clone(), retry_requests)
                    .await
                {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let diagnostic = format!("child retry admission failed: {error}");
                        for ordinal in &failed_ordinals {
                            results[*ordinal] = Some(error_result(
                                *ordinal,
                                SpawnOutcome::Errored,
                                diagnostic.clone(),
                                /*execution_ref*/ None,
                            ));
                        }
                        sync_progress_results(&progress_statuses, &results).await;
                        emit_progress(
                            &session,
                            &turn,
                            &call_id,
                            &progress_tasks,
                            &progress_thread_ids,
                            &progress_paths,
                            &progress_statuses,
                        )
                        .await;
                        waits = Vec::new();
                        continue;
                    }
                };
                if cancellation_token.is_cancelled() {
                    drop(prepared);
                    cancelled = true;
                    break;
                }

                let starts =
                    prepared
                        .into_iter()
                        .zip(&failed_ordinals)
                        .map(|(prepared, ordinal)| {
                            session
                                .services
                                .agent_control
                                .spawn_prepared_agent_with_metadata(
                                    prepared,
                                    vec![UserInput::Text {
                                        text: append_failure_guidance(
                                            task_envelope(&tasks[*ordinal], &tasks),
                                            decision.note.as_deref(),
                                        ),
                                        text_elements: Vec::new(),
                                    }],
                                )
                        });
                let start_results = join_all(starts).await;
                let mut retry_live = Vec::with_capacity(failed_ordinals.len());
                let mut retry_start_failed = false;
                for ((ordinal, path), start_result) in failed_ordinals
                    .iter()
                    .copied()
                    .zip(retry_paths)
                    .zip(start_results)
                {
                    match start_result {
                        Ok(agent) => retry_live.push((ordinal, agent.thread_id, path)),
                        Err(error) => {
                            retry_start_failed = true;
                            results[ordinal] = Some(error_result(
                                ordinal,
                                SpawnOutcome::Errored,
                                format!("child retry failed to start: {error}"),
                                /*execution_ref*/ None,
                            ));
                        }
                    }
                }
                for (ordinal, thread_id, path) in &retry_live {
                    current_thread_ids[*ordinal] = Some(*thread_id);
                    child_by_path.insert(path.clone(), *thread_id);
                    progress_thread_ids.lock().await[*ordinal] = *thread_id;
                }

                if retry_start_failed {
                    let retry_live_ids = retry_live
                        .iter()
                        .map(|(_, thread_id, _)| *thread_id)
                        .collect::<Vec<_>>();
                    let retry_live_paths = retry_live
                        .iter()
                        .map(|(_, _, path)| path.clone())
                        .collect::<Vec<_>>();
                    let teardown_result = teardown_transaction_children_with_correction(
                        &session,
                        &parent_path,
                        &retry_live_ids,
                        &retry_live_paths,
                        &child_by_path,
                        &mut corrected_ids,
                    )
                    .await;
                    quiesce_transaction_messages(
                        &session,
                        &parent_path,
                        &retry_live_paths,
                        &child_by_path,
                        &mut corrected_ids,
                    )
                    .await;
                    if let Err(error) = teardown_result {
                        fatal_error = Some(error);
                        break 'attempts;
                    }
                    for (ordinal, thread_id, path) in retry_live {
                        results[ordinal] = Some(error_result(
                            ordinal,
                            SpawnOutcome::Aborted,
                            "child retry aborted because another retry child failed to start"
                                .to_string(),
                            Some(thread_id.to_string()),
                        ));
                        current_thread_ids[ordinal] = None;
                        child_by_path.remove(&path);
                    }
                    sync_progress_results(&progress_statuses, &results).await;
                    emit_progress(
                        &session,
                        &turn,
                        &call_id,
                        &progress_tasks,
                        &progress_thread_ids,
                        &progress_paths,
                        &progress_statuses,
                    )
                    .await;
                    waits = Vec::new();
                    continue;
                }

                let retry_statuses = join_all(retry_live.iter().map(|(_, thread_id, _)| {
                    session.services.agent_control.get_status(*thread_id)
                }))
                .await;
                {
                    let mut statuses = progress_statuses.lock().await;
                    for ((ordinal, thread_id, _), status) in retry_live.iter().zip(retry_statuses) {
                        results[*ordinal] = None;
                        statuses[*ordinal] =
                            normalized_progress_status(*ordinal, *thread_id, status);
                    }
                }
                emit_progress(
                    &session,
                    &turn,
                    &call_id,
                    &progress_tasks,
                    &progress_thread_ids,
                    &progress_paths,
                    &progress_statuses,
                )
                .await;
                waits = retry_live
                    .into_iter()
                    .map(|(ordinal, thread_id, _)| AttemptWait::initial(ordinal, thread_id))
                    .collect();
            }
        }
    }

    if cancelled {
        mailbox_cancellation.activate();
        for (ordinal, thread_id) in current_thread_ids.iter().enumerate() {
            if results[ordinal].is_none() {
                results[ordinal] = Some(aborted_result(ordinal, *thread_id));
            }
        }
        sync_progress_results(&progress_statuses, &results).await;
        emit_progress(
            &session,
            &turn,
            &call_id,
            &progress_tasks,
            &progress_thread_ids,
            &progress_paths,
            &progress_statuses,
        )
        .await;
    }

    let child_thread_ids = current_thread_ids
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
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
    if let Some(error) = fatal_error {
        return match teardown_result {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!("{error}; cleanup failed: {cleanup_error}")),
        };
    }
    teardown_result?;

    if gate_cancelled {
        return Err("spine.spawn failure gate was cancelled without a selection".to_string());
    }
    finish_receipt(&tasks, results)
}

#[allow(clippy::too_many_arguments)]
async fn continue_failed_branches(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    failed_ordinals: &[usize],
    current_thread_ids: &[Option<ThreadId>],
    child_paths: &[AgentPath],
    results: &mut [Option<SpawnResult>],
    call_id: &str,
    progress_tasks: &Arc<Vec<SpawnTask>>,
    progress_thread_ids: &Arc<tokio::sync::Mutex<Vec<ThreadId>>>,
    progress_paths: &Arc<Vec<AgentPath>>,
    progress_statuses: &Arc<tokio::sync::Mutex<Vec<AgentStatus>>>,
    additional_guidance: Option<&str>,
) -> Vec<AttemptWait> {
    let control = &session.services.agent_control;
    let mut pending = Vec::with_capacity(failed_ordinals.len());
    for ordinal in failed_ordinals {
        let Some(thread_id) = current_thread_ids[*ordinal] else {
            results[*ordinal] = Some(error_result(
                *ordinal,
                SpawnOutcome::Errored,
                "child cannot continue because its thread is no longer available".to_string(),
                /*execution_ref*/ None,
            ));
            continue;
        };
        match control.subscribe_status(thread_id).await {
            Ok(mut status_rx) => {
                status_rx.borrow_and_update();
                pending.push((*ordinal, thread_id, status_rx));
            }
            Err(error) => {
                results[*ordinal] = Some(error_result(
                    *ordinal,
                    SpawnOutcome::Errored,
                    format!("child cannot continue: {error}"),
                    Some(thread_id.to_string()),
                ));
            }
        }
    }

    let reservations = match control.reserve_spine_spawn_slots(pending.len()) {
        Ok(reservations) => reservations,
        Err(error) => {
            let diagnostic = format!("child continuation admission failed: {error}");
            for (ordinal, thread_id, _) in &pending {
                results[*ordinal] = Some(error_result(
                    *ordinal,
                    SpawnOutcome::Errored,
                    diagnostic.clone(),
                    Some(thread_id.to_string()),
                ));
            }
            sync_progress_results(progress_statuses, results).await;
            emit_progress(
                session,
                turn,
                call_id,
                progress_tasks,
                progress_thread_ids,
                progress_paths,
                progress_statuses,
            )
            .await;
            return Vec::new();
        }
    };
    for (reservation, (ordinal, _, _)) in reservations.into_iter().zip(&pending) {
        reservation.commit(&child_paths[*ordinal]);
    }

    let mut waits = Vec::with_capacity(pending.len());
    for (ordinal, thread_id, status_rx) in pending {
        let send_result = control
            .send_spine_spawn_continuation(
                thread_id,
                &child_paths[ordinal],
                vec![UserInput::Text {
                    text: append_failure_guidance(
                        CONTINUE_AFTER_FAILURE_MESSAGE.to_string(),
                        additional_guidance,
                    ),
                    text_elements: Vec::new(),
                }],
                Some(turn.sub_id.clone()),
            )
            .await;
        match send_result {
            Ok(_) => {
                results[ordinal] = None;
                waits.push(AttemptWait::resumed(ordinal, thread_id, status_rx));
            }
            Err(error) => {
                results[ordinal] = Some(error_result(
                    ordinal,
                    SpawnOutcome::Errored,
                    format!("child continuation failed to start: {error}"),
                    Some(thread_id.to_string()),
                ));
            }
        }
    }

    {
        let mut statuses = progress_statuses.lock().await;
        for ordinal in failed_ordinals {
            statuses[*ordinal] = results[*ordinal]
                .as_ref()
                .map_or(AgentStatus::Running, result_status);
        }
    }
    emit_progress(
        session,
        turn,
        call_id,
        progress_tasks,
        progress_thread_ids,
        progress_paths,
        progress_statuses,
    )
    .await;
    waits
}

fn append_failure_guidance(mut message: String, additional_guidance: Option<&str>) -> String {
    if let Some(additional_guidance) = additional_guidance {
        message.push_str("\n\nAdditional user guidance:\n");
        message.push_str(additional_guidance);
    }
    message
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_attempts(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    cancellation_token: &CancellationToken,
    parent_path: &AgentPath,
    child_paths: &[AgentPath],
    child_by_path: &HashMap<AgentPath, ThreadId>,
    corrected_ids: &mut HashSet<String>,
    call_id: &str,
    progress_tasks: &Arc<Vec<SpawnTask>>,
    progress_thread_ids: &Arc<tokio::sync::Mutex<Vec<ThreadId>>>,
    progress_paths: &Arc<Vec<AgentPath>>,
    progress_statuses: &Arc<tokio::sync::Mutex<Vec<AgentStatus>>>,
    waits: Vec<AttemptWait>,
) -> Option<Vec<(usize, SpawnResult)>> {
    let waits = waits.into_iter().map(|wait| {
        let control = session.services.agent_control.clone();
        let session = Arc::clone(session);
        let turn = Arc::clone(turn);
        let call_id = call_id.to_string();
        let progress_tasks = Arc::clone(progress_tasks);
        let progress_thread_ids = Arc::clone(progress_thread_ids);
        let progress_paths = Arc::clone(progress_paths);
        let progress_statuses = Arc::clone(progress_statuses);
        let parent_path = parent_path.clone();
        let child_path = child_paths[wait.ordinal].clone();
        let parent_thread_id = session.thread_id;
        let parent_turn_id = turn.sub_id.clone();
        async move {
            let mut status = match wait.resume_status {
                Some(status_rx) => {
                    wait_for_terminal_after_resume(
                        &control,
                        &parent_path,
                        &child_path,
                        parent_thread_id,
                        parent_turn_id,
                        wait.thread_id,
                        status_rx,
                    )
                    .await
                }
                None => {
                    wait_for_terminal(
                        &control,
                        &parent_path,
                        &child_path,
                        parent_thread_id,
                        parent_turn_id,
                        wait.thread_id,
                    )
                    .await
                }
            };
            if control
                .wait_for_spine_spawn_turn_idle(wait.thread_id)
                .await
                .is_ok()
            {
                status = control.get_status(wait.thread_id).await;
            }
            let result = result_from_status(wait.ordinal, wait.thread_id, status);
            let event = {
                let thread_ids = progress_thread_ids.lock().await;
                let mut statuses = progress_statuses.lock().await;
                statuses[wait.ordinal] = result_status(&result);
                spawn_progress_event(
                    &call_id,
                    progress_tasks.as_ref(),
                    &thread_ids,
                    progress_paths.as_ref(),
                    &statuses,
                )
            };
            session
                .emit_spine_spawn_progress(turn.as_ref(), event)
                .await;
            (wait.ordinal, result)
        }
    });
    let wait_all = join_all(waits);
    tokio::pin!(wait_all);
    let mut interval = tokio::time::interval(Duration::from_millis(25));
    let terminal = loop {
        tokio::select! {
            terminal = &mut wait_all => break Some(terminal),
            _ = cancellation_token.cancelled() => break None,
            _ = interval.tick() => {
                correct_intermediate_messages(
                    session,
                    parent_path,
                    child_paths,
                    child_by_path,
                    corrected_ids,
                ).await;
            }
        }
    };
    correct_intermediate_messages(
        session,
        parent_path,
        child_paths,
        child_by_path,
        corrected_ids,
    )
    .await;
    terminal
}

async fn sync_progress_results(
    progress_statuses: &tokio::sync::Mutex<Vec<AgentStatus>>,
    results: &[Option<SpawnResult>],
) {
    let mut statuses = progress_statuses.lock().await;
    for (ordinal, result) in results.iter().enumerate() {
        if let Some(result) = result {
            statuses[ordinal] = result_status(result);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn emit_progress(
    session: &Session,
    turn: &TurnContext,
    call_id: &str,
    tasks: &[SpawnTask],
    thread_ids: &tokio::sync::Mutex<Vec<ThreadId>>,
    paths: &[AgentPath],
    statuses: &tokio::sync::Mutex<Vec<AgentStatus>>,
) {
    let event = {
        let thread_ids = thread_ids.lock().await;
        let statuses = statuses.lock().await;
        spawn_progress_event(call_id, tasks, &thread_ids, paths, &statuses)
    };
    session.emit_spine_spawn_progress(turn, event).await;
}

fn aborted_result(ordinal: usize, thread_id: Option<ThreadId>) -> SpawnResult {
    error_result(
        ordinal,
        SpawnOutcome::Aborted,
        "branch aborted because the originating spine.spawn transaction was cancelled".to_string(),
        thread_id.map(|thread_id| thread_id.to_string()),
    )
}

use crate::agent::AgentStatus;
use crate::agent::control::SpawnAgentBatchRequest;
use crate::agent::control::SpawnAgentForkMode;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::next_thread_spawn_depth;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::handlers::multi_agents_common::build_agent_spawn_config;
use crate::tools::handlers::multi_agents_common::thread_spawn_source;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SpineSpawnProgressEvent;
use codex_protocol::protocol::SpineSpawnTaskProgress;
use codex_protocol::user_input::UserInput;
use codex_spine_core::SPINE_SPAWN_RESULT_SCHEMA;
use codex_spine_core::SpawnOutcome;
use codex_spine_core::SpawnReceipt;
use codex_spine_core::SpawnResult;
use codex_spine_core::SpawnTask;
use futures::future::join_all;
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CORRECTION_MESSAGE: &str = "你是自依赖的Agent，除了final memory之外不要发送我任何消息。";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    tasks: Vec<SpawnTask>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpawnBatchCall {
    pub(crate) call_id: String,
    pub(crate) tasks: Vec<SpawnTask>,
}

#[derive(Default)]
pub(crate) struct SpawnBatchCoordinator {
    completed: HashMap<String, Result<SpawnReceipt, String>>,
}

pub(crate) fn parse_tasks(arguments: &str) -> Result<Vec<SpawnTask>, String> {
    let tasks = serde_json::from_str::<SpawnArgs>(arguments)
        .map_err(|error| format!("invalid spine.spawn arguments: {error}"))?
        .tasks;
    if tasks.len() < 2 {
        return Err("spine.spawn requires at least two tasks".to_string());
    }
    for (ordinal, task) in tasks.iter().enumerate() {
        if task.summary.trim().is_empty() {
            return Err(format!(
                "spine.spawn task {ordinal} requires a non-empty summary"
            ));
        }
        if task.prompt.trim().is_empty() {
            return Err(format!(
                "spine.spawn task {ordinal} requires a non-empty prompt"
            ));
        }
    }
    Ok(tasks)
}

pub(crate) fn encode_receipt(receipt: &SpawnReceipt) -> Result<String, serde_json::Error> {
    serde_json::to_string(receipt)
}

pub(crate) fn decode_receipt(body: &str) -> Result<SpawnReceipt, serde_json::Error> {
    serde_json::from_str(body)
}

pub(crate) fn calls_in_response_group(
    rollout: &[RolloutItem],
    call_id: &str,
) -> Result<Vec<SpawnBatchCall>, String> {
    let effective = super::effective_rollout(rollout);
    let mut index = 0;
    while index < effective.len() {
        let Some((group, consumed)) = super::completed_tool_group(&effective, index, true) else {
            index += 1;
            continue;
        };
        if group.calls.iter().any(|call| call.call_id == call_id) {
            if group.calls.iter().any(|call| {
                matches!(
                    call.name.as_str(),
                    "spine.open" | "spine.close" | "spine.next"
                )
            }) {
                return Err(
                    "spine.spawn cannot be mixed with spine.open, spine.close, or spine.next"
                        .to_string(),
                );
            }

            let mut calls = Vec::new();
            for call in group.calls.iter().filter(|call| call.name == "spine.spawn") {
                match parse_tasks(&call.arguments) {
                    Ok(tasks) => calls.push(SpawnBatchCall {
                        call_id: call.call_id.clone(),
                        tasks,
                    }),
                    Err(error) if call.call_id == call_id => return Err(error),
                    Err(_) => {}
                }
            }
            if calls.iter().any(|call| call.call_id == call_id) {
                return Ok(calls);
            }
            return Err(format!(
                "spine.spawn call `{call_id}` is missing valid tasks from its response group"
            ));
        }
        index += consumed;
    }
    Err(format!(
        "spine.spawn call `{call_id}` is missing from the current rollout"
    ))
}

pub(crate) async fn execute(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    call_id: String,
    cancellation_token: CancellationToken,
    tasks: Vec<SpawnTask>,
) -> Result<SpawnReceipt, String> {
    let mut coordinator = session.spine_spawn_batch_coordinator.lock().await;
    if let Some(result) = coordinator.completed.remove(&call_id) {
        return result;
    }

    let calls = session
        .spine_spawn_calls_in_response_group(&call_id)
        .await?;
    let Some(current) = calls.iter().find(|call| call.call_id == call_id) else {
        return Err(format!(
            "spine.spawn call `{call_id}` is missing from its response group"
        ));
    };
    if current.tasks != tasks {
        return Err(format!(
            "spine.spawn call `{call_id}` arguments changed during group admission"
        ));
    }

    let batch_result = execute_batch(Arc::clone(&session), turn, cancellation_token, &calls).await;
    match batch_result {
        Ok(receipts) => coordinator.completed.extend(
            receipts
                .into_iter()
                .map(|(call_id, receipt)| (call_id, Ok(receipt))),
        ),
        Err(error) => coordinator.completed.extend(
            calls
                .iter()
                .map(|call| (call.call_id.clone(), Err(error.clone()))),
        ),
    }
    coordinator.completed.remove(&call_id).unwrap_or_else(|| {
        Err(format!(
            "spine.spawn batch did not produce a result for call `{call_id}`"
        ))
    })
}

async fn execute_batch(
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    cancellation_token: CancellationToken,
    calls: &[SpawnBatchCall],
) -> Result<HashMap<String, SpawnReceipt>, String> {
    if cancellation_token.is_cancelled() {
        return Err("spine.spawn was cancelled before child creation".to_string());
    }

    let config = build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())
        .map_err(|error| error.to_string())?;
    let child_depth = next_thread_spawn_depth(&turn.session_source);
    let parent_path = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let task_count = calls.iter().map(|call| call.tasks.len()).sum();
    let mut child_paths = Vec::with_capacity(task_count);
    let mut requests = Vec::with_capacity(task_count);
    let mut flat_tasks = Vec::with_capacity(task_count);
    for (call_ordinal, call) in calls.iter().enumerate() {
        for (task_ordinal, task) in call.tasks.iter().enumerate() {
            let task_name = transaction_task_name(&call.call_id, task_ordinal);
            let source = thread_spawn_source(
                session.thread_id,
                &turn.session_source,
                child_depth,
                /*agent_role*/ None,
                Some(task_name),
            )
            .map_err(|error| error.to_string())?;
            let child_path = source
                .get_agent_path()
                .ok_or_else(|| "spine.spawn child is missing an agent path".to_string())?;
            child_paths.push(child_path);
            flat_tasks.push((call_ordinal, task_ordinal, task.clone()));
            // TODO(spine-spawn-context): Verify complete effective parent-context inheritance for
            // spawned children using native fork_turns="all". Compare the parent pre-spawn
            // effective context with each child's first model request, including inherited Spine
            // memory and first-turn cached_tokens, before strengthening this contract.
            requests.push(
                SpawnAgentBatchRequest::new(
                    source,
                    SpawnAgentOptions {
                        fork_parent_spawn_call_id: Some(call.call_id.clone()),
                        fork_mode: Some(SpawnAgentForkMode::FullHistoryTrimToolCallSuffix),
                        parent_thread_id: Some(session.thread_id),
                        environments: Some(turn.environments.to_selections()),
                    },
                )
                .suppress_parent_completion_notification(),
            );
        }
    }

    let prepared = session
        .services
        .agent_control
        .prepare_agent_spawn_batch(config, requests)
        .await
        .map_err(|error| format!("spine.spawn admission failed: {error}"))?;
    if cancellation_token.is_cancelled() {
        drop(prepared);
        return Err("spine.spawn was cancelled before child creation".to_string());
    }

    let starts = prepared
        .into_iter()
        .zip(flat_tasks.iter())
        .map(|(prepared, (_, _, task))| {
            session
                .services
                .agent_control
                .spawn_prepared_agent_with_metadata(
                    prepared,
                    vec![UserInput::Text {
                        text: task_envelope(task),
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
        failed: start_failed,
    } = classify_start_results(&child_paths, start_results);

    if start_failed {
        teardown_transaction_children(session.as_ref(), &live).await?;
        for (ordinal, thread_id, _) in &live {
            let diagnostic =
                "child aborted because another transaction child failed to start".to_string();
            results[*ordinal] = Some(error_result(
                *ordinal,
                SpawnOutcome::Aborted,
                diagnostic,
                Some(thread_id.to_string()),
            ));
        }
        return finish_batch_receipts(calls, results);
    }

    let child_by_path = live
        .iter()
        .map(|(_, thread_id, path)| (path.clone(), *thread_id))
        .collect::<HashMap<_, _>>();
    let progress_calls = Arc::new(calls.to_vec());
    let progress_paths = Arc::new(child_paths.clone());
    let initial_statuses = join_all(
        live.iter()
            .map(|(_, thread_id, _)| session.services.agent_control.get_status(*thread_id)),
    )
    .await;
    let mut progress_statuses = vec![AgentStatus::PendingInit; task_count];
    for ((ordinal, _, _), status) in live.iter().zip(initial_statuses) {
        progress_statuses[*ordinal] = status;
    }
    let progress_statuses = Arc::new(tokio::sync::Mutex::new(progress_statuses));
    for call_ordinal in 0..progress_calls.len() {
        session
            .emit_spine_spawn_progress(
                turn.as_ref(),
                batch_progress_event(
                    progress_calls.as_ref(),
                    call_ordinal,
                    progress_paths.as_ref(),
                    &progress_statuses.lock().await,
                ),
            )
            .await;
    }
    let waits = live.iter().map(|(ordinal, thread_id, _)| {
        let control = session.services.agent_control.clone();
        let session = session.clone();
        let turn = turn.clone();
        let progress_calls = progress_calls.clone();
        let progress_paths = progress_paths.clone();
        let progress_statuses = progress_statuses.clone();
        let ordinal = *ordinal;
        let call_ordinal = flat_tasks[ordinal].0;
        let thread_id = *thread_id;
        async move {
            let status = wait_for_terminal(&control, thread_id).await;
            let event = {
                let mut statuses = progress_statuses.lock().await;
                statuses[ordinal] = status.clone();
                batch_progress_event(
                    progress_calls.as_ref(),
                    call_ordinal,
                    progress_paths.as_ref(),
                    &statuses,
                )
            };
            session
                .emit_spine_spawn_progress(turn.as_ref(), event)
                .await;
            (ordinal, thread_id, status)
        }
    });
    let wait_all = join_all(waits);
    tokio::pin!(wait_all);
    let mut corrected_ids = HashSet::new();
    let mut interval = tokio::time::interval(Duration::from_millis(25));
    let terminal = loop {
        tokio::select! {
            statuses = &mut wait_all => break Some(statuses),
            _ = cancellation_token.cancelled() => break None,
            _ = interval.tick() => {
                correct_intermediate_messages(
                    &session,
                    &parent_path,
                    &child_by_path,
                    &mut corrected_ids,
                ).await;
            }
        }
    };
    correct_intermediate_messages(&session, &parent_path, &child_by_path, &mut corrected_ids).await;

    let cancelled = match terminal {
        Some(statuses) => {
            for (ordinal, thread_id, status) in statuses {
                results[ordinal] = Some(result_from_status(ordinal, thread_id, status));
            }
            false
        }
        None => {
            for (ordinal, thread_id, _) in &live {
                let status = session.services.agent_control.get_status(*thread_id).await;
                if is_spawn_terminal(&status) {
                    results[*ordinal] = Some(result_from_status(*ordinal, *thread_id, status));
                }
            }
            true
        }
    };

    teardown_transaction_children(session.as_ref(), &live).await?;

    if cancelled {
        for (ordinal, thread_id, _) in &live {
            if results[*ordinal].is_none() {
                results[*ordinal] = Some(error_result(
                    *ordinal,
                    SpawnOutcome::Aborted,
                    "child aborted because the parent spine.spawn was cancelled".to_string(),
                    Some(thread_id.to_string()),
                ));
            }
        }
        let events = {
            let mut statuses = progress_statuses.lock().await;
            for (ordinal, result) in results.iter().enumerate() {
                if let Some(result) = result {
                    statuses[ordinal] = result_status(result);
                }
            }
            (0..progress_calls.len())
                .map(|call_ordinal| {
                    batch_progress_event(
                        progress_calls.as_ref(),
                        call_ordinal,
                        progress_paths.as_ref(),
                        &statuses,
                    )
                })
                .collect::<Vec<_>>()
        };
        for event in events {
            session
                .emit_spine_spawn_progress(turn.as_ref(), event)
                .await;
        }
    }

    finish_batch_receipts(calls, results)
}

async fn teardown_transaction_children(
    session: &Session,
    live: &[(usize, ThreadId, AgentPath)],
) -> Result<(), String> {
    let teardown = live.iter().map(|(_, thread_id, _)| async move {
        let thread_id = *thread_id;
        (
            thread_id,
            session
                .services
                .agent_control
                .shutdown_live_agent(thread_id)
                .await,
        )
    });
    let failures = join_all(teardown)
        .await
        .into_iter()
        .filter_map(|(thread_id, result)| result.err().map(|error| format!("{thread_id}: {error}")))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "spine.spawn child teardown failed: {}",
            failures.join("; ")
        ))
    }
}

fn batch_progress_event(
    calls: &[SpawnBatchCall],
    call_ordinal: usize,
    paths: &[AgentPath],
    statuses: &[AgentStatus],
) -> SpineSpawnProgressEvent {
    let start = calls
        .iter()
        .take(call_ordinal)
        .map(|call| call.tasks.len())
        .sum::<usize>();
    let call = &calls[call_ordinal];
    let end = start + call.tasks.len();
    spawn_progress_event(
        &call.call_id,
        &call.tasks,
        &paths[start..end],
        &statuses[start..end],
    )
}

fn finish_batch_receipts(
    calls: &[SpawnBatchCall],
    results: Vec<Option<SpawnResult>>,
) -> Result<HashMap<String, SpawnReceipt>, String> {
    let mut receipts = HashMap::with_capacity(calls.len());
    let mut results = results.into_iter();
    for call in calls {
        let call_results = (0..call.tasks.len())
            .map(|task_ordinal| {
                results.next().flatten().map(|mut result| {
                    result.ordinal = u32::try_from(task_ordinal).unwrap_or(u32::MAX);
                    result
                })
            })
            .collect();
        receipts.insert(
            call.call_id.clone(),
            finish_receipt(&call.tasks, call_results)?,
        );
    }
    debug_assert!(results.next().is_none());
    Ok(receipts)
}

fn spawn_progress_event(
    call_id: &str,
    tasks: &[SpawnTask],
    paths: &[AgentPath],
    statuses: &[AgentStatus],
) -> SpineSpawnProgressEvent {
    SpineSpawnProgressEvent {
        call_id: call_id.to_string(),
        tasks: tasks
            .iter()
            .zip(paths)
            .zip(statuses)
            .enumerate()
            .map(|(ordinal, ((task, path), status))| SpineSpawnTaskProgress {
                ordinal: ordinal as u32,
                summary: task.summary.clone(),
                agent_path: Some(path.clone()),
                status: status.clone(),
            })
            .collect(),
    }
}

fn result_status(result: &SpawnResult) -> AgentStatus {
    match result.outcome {
        SpawnOutcome::Completed => AgentStatus::Completed(Some(result.memory_body.clone())),
        SpawnOutcome::Errored => AgentStatus::Errored(
            result
                .diagnostic
                .clone()
                .unwrap_or_else(|| result.memory_body.clone()),
        ),
        SpawnOutcome::Aborted => AgentStatus::Shutdown,
    }
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
        .filter(|character| character.is_ascii_alphanumeric())
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

fn task_envelope(task: &SpawnTask) -> String {
    format!(
        "You are a self-contained spine.spawn child agent. Work only on the task below using the inherited context. Do not ask the parent for information and do not send questions, progress, commentary, or partial results to the parent. When finished, return exactly one final message containing the terminal memory for this task.\n\nTask summary: {}\n\nTask:\n{}",
        task.summary, task.prompt
    )
}

async fn wait_for_terminal(
    control: &crate::agent::AgentControl,
    thread_id: ThreadId,
) -> AgentStatus {
    let Ok(mut status_rx) = control.subscribe_status(thread_id).await else {
        return control.get_status(thread_id).await;
    };
    loop {
        let status = status_rx.borrow_and_update().clone();
        if is_spawn_terminal(&status) {
            return status;
        }
        if status_rx.changed().await.is_err() {
            return control.get_status(thread_id).await;
        }
    }
}

fn is_spawn_terminal(status: &AgentStatus) -> bool {
    !matches!(status, AgentStatus::PendingInit | AgentStatus::Running)
}

async fn correct_intermediate_messages(
    session: &Session,
    parent_path: &AgentPath,
    child_by_path: &HashMap<AgentPath, ThreadId>,
    corrected_ids: &mut HashSet<String>,
) {
    let messages = session
        .input_queue
        .extract_mailbox_communications(|mail| child_by_path.contains_key(&mail.author))
        .await;
    for message in messages {
        if message
            .id
            .as_ref()
            .is_some_and(|identity| !corrected_ids.insert(identity.clone()))
        {
            continue;
        }
        let Some(thread_id) = child_by_path.get(&message.author).copied() else {
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
            .send_inter_agent_communication(thread_id, correction, context)
            .await;
    }
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
                        None,
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

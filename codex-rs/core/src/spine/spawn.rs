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

pub(crate) fn validate_call_only(rollout: &[RolloutItem], call_id: &str) -> Result<(), String> {
    let effective = super::effective_rollout(rollout);
    let mut index = 0;
    while index < effective.len() {
        let Some((group, consumed)) = super::completed_tool_group(&effective, index, true) else {
            index += 1;
            continue;
        };
        if group.calls.iter().any(|call| call.call_id == call_id) {
            let is_exclusive_spawn = group.calls.len() == 1
                && group.calls[0].call_id == call_id
                && group.calls[0].name == "spine.spawn"
                && group
                    .leading_assistant_messages
                    .iter()
                    .all(|message| message.content.trim().is_empty());
            return is_exclusive_spawn.then_some(()).ok_or_else(|| {
                "spine.spawn must be the only call and non-empty assistant output in its model response"
                    .to_string()
            });
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
    session.validate_spine_spawn_call_only(&call_id).await?;
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
    let mut child_paths = Vec::with_capacity(tasks.len());
    let mut requests = Vec::with_capacity(tasks.len());
    for ordinal in 0..tasks.len() {
        let task_name = transaction_task_name(&call_id, ordinal);
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
        // TODO(spine-spawn-context): Verify complete effective parent-context inheritance for
        // spawned children using native fork_turns="all". Compare the parent pre-spawn
        // effective context with each child's first model request, including inherited Spine
        // memory and first-turn cached_tokens, before strengthening this contract.
        requests.push(
            SpawnAgentBatchRequest::new(
                source,
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: Some(call_id.clone()),
                    fork_mode: Some(SpawnAgentForkMode::FullHistory),
                    parent_thread_id: Some(session.thread_id),
                    environments: Some(turn.environments.to_selections()),
                },
            )
            .suppress_parent_completion_notification(),
        );
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
        .zip(tasks.iter())
        .map(|(prepared, task)| {
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
        let teardown = live.iter().map(|(_, thread_id, _)| {
            session
                .services
                .agent_control
                .shutdown_live_agent(*thread_id)
        });
        let _ = join_all(teardown).await;
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
        return finish_receipt(&tasks, results);
    }

    let child_by_path = live
        .iter()
        .map(|(_, thread_id, path)| (path.clone(), *thread_id))
        .collect::<HashMap<_, _>>();
    let waits = live.iter().map(|(ordinal, thread_id, _)| {
        let control = session.services.agent_control.clone();
        let ordinal = *ordinal;
        let thread_id = *thread_id;
        async move {
            (
                ordinal,
                thread_id,
                wait_for_terminal(&control, thread_id).await,
            )
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

    match terminal {
        Some(statuses) => {
            for (ordinal, thread_id, status) in statuses {
                results[ordinal] = Some(result_from_status(ordinal, thread_id, status));
            }
        }
        None => {
            for (ordinal, thread_id, _) in &live {
                let status = session.services.agent_control.get_status(*thread_id).await;
                if is_spawn_terminal(&status) {
                    results[*ordinal] = Some(result_from_status(*ordinal, *thread_id, status));
                }
            }
            let teardown = live.iter().filter_map(|(ordinal, thread_id, _)| {
                results[*ordinal].is_none().then(|| {
                    session
                        .services
                        .agent_control
                        .shutdown_live_agent(*thread_id)
                })
            });
            let _ = join_all(teardown).await;
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
        }
    }

    finish_receipt(&tasks, results)
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

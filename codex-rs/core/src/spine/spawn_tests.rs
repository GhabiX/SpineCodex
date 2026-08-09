use super::*;
use pretty_assertions::assert_eq;

fn tasks() -> Vec<SpawnTask> {
    vec![
        SpawnTask {
            summary: "parser".to_string(),
            prompt: "Implement the parser.".to_string(),
        },
        SpawnTask {
            summary: "compatibility".to_string(),
            prompt: "Test compatibility.".to_string(),
        },
    ]
}

#[test]
fn parse_tasks_validates_complete_batch_before_admission() {
    let parsed = parse_tasks(
        r#"{"tasks":[{"summary":"parser","prompt":"first"},{"summary":"tests","prompt":"second"}]}"#,
    )
    .unwrap();
    assert_eq!(
        parsed,
        vec![
            SpawnTask {
                summary: "parser".to_string(),
                prompt: "first".to_string(),
            },
            SpawnTask {
                summary: "tests".to_string(),
                prompt: "second".to_string(),
            },
        ]
    );

    for invalid in [
        r#"{"tasks":[]}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first"}]}"#,
        r#"{"tasks":[{"summary":"same","prompt":"first"},{"summary":" same ","prompt":"second"}]}"#,
        r#"{"tasks":[{"summary":"one","prompt":""},{"summary":"two","prompt":"second"}]}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first"},{"summary":"two","prompt":"second"}],"extra":true}"#,
    ] {
        assert!(parse_tasks(invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn task_envelope_injects_identity_and_peer_roster() {
    let tasks = tasks();
    let envelope = task_envelope(&tasks[0], &tasks);

    assert!(envelope.contains("You are: parser"));
    assert!(envelope.contains("Peer branches in this spawn:\n- compatibility"));
    assert!(envelope.ends_with("Assignment:\nImplement the parser."));
}

fn child_input_token_upper_bound(tasks: &[SpawnTask], ordinal: usize) -> usize {
    let item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: task_envelope(&tasks[ordinal], tasks),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    crate::context::spine_model_item_wire_bytes(&item).unwrap()
}

fn task_pair_with_first_input_tokens(target_tokens: usize) -> Vec<SpawnTask> {
    let mut tasks = vec![
        SpawnTask {
            summary: "parser".to_string(),
            prompt: String::new(),
        },
        SpawnTask {
            summary: "compatibility".to_string(),
            prompt: "Test compatibility.".to_string(),
        },
    ];
    let mut remaining = target_tokens.saturating_sub(child_input_token_upper_bound(&tasks, 0));
    let prompt_tokens = remaining.min(spine_core::MAX_SPAWN_PROMPT_BYTES);
    tasks[0].prompt = "a".repeat(prompt_tokens);
    remaining = target_tokens.saturating_sub(child_input_token_upper_bound(&tasks, 0));
    tasks[1].summary.push_str(&"b".repeat(remaining));
    assert!(tasks[1].summary.len() <= spine_core::MAX_SUMMARY_BYTES);
    assert_eq!(child_input_token_upper_bound(&tasks, 0), target_tokens);
    tasks
}

#[test]
fn parse_tasks_accepts_a_complete_child_input_at_the_hard_boundary() {
    let tasks = task_pair_with_first_input_tokens(crate::context::MAX_SPINE_MODEL_ITEM_WIRE_BYTES);
    let arguments = serde_json::json!({ "tasks": tasks }).to_string();

    let parsed = parse_tasks(&arguments).expect("input at the model boundary is valid");
    assert_eq!(
        child_input_token_upper_bound(&parsed, 0),
        crate::context::MAX_SPINE_MODEL_ITEM_WIRE_BYTES
    );
}

#[test]
fn parse_tasks_rejects_one_safety_token_over_the_complete_child_input_boundary() {
    let tasks =
        task_pair_with_first_input_tokens(crate::context::MAX_SPINE_MODEL_ITEM_WIRE_BYTES + 1);
    let arguments = serde_json::json!({ "tasks": tasks }).to_string();

    let error = parse_tasks(&arguments).expect_err("oversized child input must be rejected");
    assert!(error.contains("child initial input"));
    assert!(error.contains("task 0"));
}

#[test]
fn parse_tasks_counts_unicode_escaping_and_peer_rosters_in_child_input() {
    let tasks = vec![
        SpawnTask {
            summary: "parser".to_string(),
            prompt: "界\"\\\n".repeat(700),
        },
        SpawnTask {
            summary: "peer".repeat(spine_core::MAX_SUMMARY_BYTES / 4),
            prompt: "Test compatibility.".to_string(),
        },
    ];
    let arguments = serde_json::json!({ "tasks": tasks }).to_string();

    let error = parse_tasks(&arguments).expect_err("peer roster must be included in the limit");
    assert!(error.contains("child initial input"));
}

#[test]
fn spawn_progress_event_preserves_task_identity_and_status() {
    let tasks = tasks();
    let thread_ids = [ThreadId::new(), ThreadId::new()];
    let paths = [
        AgentPath::try_from("/root/parser").unwrap(),
        AgentPath::try_from("/root/compatibility").unwrap(),
    ];
    let statuses = [
        AgentStatus::Running,
        AgentStatus::Errored("provider failure".to_string()),
    ];

    assert_eq!(
        spawn_progress_event("spawn-call", &tasks, &thread_ids, &paths, &statuses),
        SpineSpawnProgressEvent {
            call_id: "spawn-call".to_string(),
            tasks: vec![
                SpineSpawnTaskProgress {
                    ordinal: 0,
                    summary: "parser".to_string(),
                    thread_id: thread_ids[0],
                    agent_path: Some(paths[0].clone()),
                    status: AgentStatus::Running,
                },
                SpineSpawnTaskProgress {
                    ordinal: 1,
                    summary: "compatibility".to_string(),
                    thread_id: thread_ids[1],
                    agent_path: Some(paths[1].clone()),
                    status: AgentStatus::Errored("provider failure".to_string()),
                },
            ],
        }
    );
}

#[test]
fn transaction_task_names_are_path_safe_and_stable() {
    assert_eq!(transaction_task_name("Call-ID.42", 3), "spawn_callid42_3");
    assert_eq!(transaction_task_name("!!!", 0), "spawn_call_0");
}

#[tokio::test]
async fn abort_barrier_cancels_active_transaction_and_blocks_new_admission() {
    let lifecycle = SpawnLifecycle::default();
    let cancellation = CancellationToken::new();
    let transaction = lifecycle
        .try_enter(cancellation.clone())
        .expect("first transaction enters");
    let barrier = lifecycle.begin_abort();

    assert!(barrier.had_active_transactions());
    assert!(cancellation.is_cancelled());
    assert!(lifecycle.try_enter(CancellationToken::new()).is_none());
    drop(transaction);
    barrier.wait_for_quiescence().await;
    assert!(lifecycle.try_enter(CancellationToken::new()).is_none());
    drop(barrier);
    assert!(lifecycle.try_enter(CancellationToken::new()).is_some());
}

#[test]
fn start_failure_is_total_and_keeps_input_ordinals() {
    let paths = vec![
        AgentPath::try_from("/root/spawn_0").unwrap(),
        AgentPath::try_from("/root/spawn_1").unwrap(),
        AgentPath::try_from("/root/spawn_2").unwrap(),
    ];
    let first = ThreadId::new();
    let third = ThreadId::new();
    let StartPhase {
        live,
        mut results,
        failed,
    } = classify_start_results(
        &paths,
        [Ok(first), Err("injected start failure"), Ok(third)],
    );

    assert!(failed);
    assert_eq!(
        live.iter()
            .map(|(ordinal, thread_id, _)| (*ordinal, *thread_id))
            .collect::<Vec<_>>(),
        vec![(0, first), (2, third)]
    );
    for (ordinal, thread_id, _) in live {
        results[ordinal] = Some(error_result(
            ordinal,
            SpawnOutcome::Aborted,
            "sibling start failed".to_string(),
            Some(thread_id.to_string()),
        ));
    }
    let receipt = finish_receipt(
        &[
            SpawnTask {
                summary: "zero".to_string(),
                prompt: "zero task".to_string(),
            },
            SpawnTask {
                summary: "one".to_string(),
                prompt: "one task".to_string(),
            },
            SpawnTask {
                summary: "two".to_string(),
                prompt: "two task".to_string(),
            },
        ],
        results,
    )
    .unwrap();
    assert_eq!(
        receipt
            .results
            .iter()
            .map(|result| (result.ordinal, result.outcome))
            .collect::<Vec<_>>(),
        vec![
            (0, SpawnOutcome::Aborted),
            (1, SpawnOutcome::Errored),
            (2, SpawnOutcome::Aborted),
        ]
    );
}

#[test]
fn terminal_statuses_produce_truthful_total_receipt() {
    let tasks = vec![
        SpawnTask {
            summary: "completed".to_string(),
            prompt: "a".to_string(),
        },
        SpawnTask {
            summary: "missing".to_string(),
            prompt: "b".to_string(),
        },
        SpawnTask {
            summary: "failed".to_string(),
            prompt: "c".to_string(),
        },
        SpawnTask {
            summary: "aborted".to_string(),
            prompt: "d".to_string(),
        },
    ];
    let statuses = [
        AgentStatus::Completed(Some("memory".to_string())),
        AgentStatus::Completed(None),
        AgentStatus::Errored("provider failure".to_string()),
        AgentStatus::Shutdown,
    ];
    let results = statuses
        .into_iter()
        .enumerate()
        .map(|(ordinal, status)| Some(result_from_status(ordinal, ThreadId::new(), status, None)))
        .collect();

    let receipt = finish_receipt(&tasks, results).unwrap();
    assert_eq!(
        receipt
            .results
            .iter()
            .map(|result| (result.ordinal, result.outcome))
            .collect::<Vec<_>>(),
        vec![
            (0, SpawnOutcome::Completed),
            (1, SpawnOutcome::Errored),
            (2, SpawnOutcome::Errored),
            (3, SpawnOutcome::Aborted),
        ]
    );
    assert_eq!(receipt.results[0].memory_body, "memory");
    assert!(receipt.results[1..].iter().all(|result| {
        !result.memory_body.trim().is_empty()
            && result
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| !diagnostic.trim().is_empty())
    }));
}

#[test]
fn salvaged_failure_preserves_memory_without_changing_outcome() {
    let thread_id = ThreadId::new();
    let result = result_from_status(
        0,
        thread_id,
        AgentStatus::Completed(None),
        Some(crate::spine::spawn_salvage::SpawnFailureRecord {
            diagnostic: "upstream 503".to_string(),
            salvaged_memory: Some("confirmed progress".to_string()),
        }),
    );
    assert_eq!(
        result,
        SpawnResult {
            ordinal: 0,
            outcome: SpawnOutcome::Errored,
            memory_body: "confirmed progress".to_string(),
            diagnostic: Some("child errored: upstream 503".to_string()),
            execution_ref: Some(thread_id.to_string()),
        }
    );
}

#[test]
fn capacity_rejection_is_ordered_and_creates_no_execution_refs() {
    let tasks = tasks();
    let receipt = capacity_rejection_receipt(&tasks, /*max_threads*/ 1).unwrap();

    assert_eq!(receipt.results.len(), tasks.len());
    for (ordinal, result) in receipt.results.iter().enumerate() {
        assert_eq!(result.ordinal, ordinal as u32);
        assert_eq!(result.outcome, SpawnOutcome::Errored);
        assert_eq!(result.execution_ref, None);
        assert!(result.memory_body.contains("all-or-nothing"));
    }
}

#[test]
fn missing_internal_result_fails_closed_as_an_errored_result() {
    let tasks = tasks();
    let completed = result_from_status(
        0,
        ThreadId::new(),
        AgentStatus::Completed(Some("memory".to_string())),
        None,
    );
    let receipt = finish_receipt(&tasks, vec![Some(completed), None]).unwrap();

    assert_eq!(receipt.results[0].outcome, SpawnOutcome::Completed);
    assert_eq!(receipt.results[1].outcome, SpawnOutcome::Errored);
    assert!(
        receipt.results[1]
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("lost"))
    );
}

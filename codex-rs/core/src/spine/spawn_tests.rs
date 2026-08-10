use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_spine_core::SPINE_SPAWN_RESULT_SCHEMA;
use codex_spine_core::SpawnOutcome;
use codex_spine_core::SpawnResult;
use pretty_assertions::assert_eq;

#[test]
fn task_arguments_require_two_exact_non_empty_tasks() {
    let tasks = parse_tasks(
        r#"{"tasks":[{"summary":"one","prompt":"first"},{"summary":" two ","prompt":" second "}]}"#,
    )
    .unwrap();
    assert_eq!(
        tasks,
        vec![
            codex_spine_core::SpawnTask {
                summary: "one".to_string(),
                prompt: "first".to_string(),
            },
            codex_spine_core::SpawnTask {
                summary: " two ".to_string(),
                prompt: " second ".to_string(),
            },
        ]
    );

    for arguments in [
        r#"{"tasks":[]}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first"}]}"#,
        r#"{"tasks":[{"summary":" ","prompt":"first"},{"summary":"two","prompt":"second"}]}"#,
        r#"{"tasks":[{"summary":"one","prompt":""},{"summary":"two","prompt":"second"}]}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first","extra":true},{"summary":"two","prompt":"second"}]}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first"},{"summary":"two","prompt":"second"}],"extra":true}"#,
        r#"{"tasks":[{"summary":"one","prompt":"first"},{"summary":" one ","prompt":"second"}]}"#,
    ] {
        assert!(parse_tasks(arguments).is_err(), "accepted {arguments}");
    }
}

#[test]
fn task_envelope_injects_identity_and_same_call_peer_roster() {
    let tasks = vec![
        SpawnTask {
            summary: "parser".to_string(),
            prompt: concat!(
                "Shared blackboard: tasks/trial/blackboard\n",
                "Implement parser."
            )
            .to_string(),
        },
        SpawnTask {
            summary: "compatibility tests".to_string(),
            prompt: concat!(
                "Shared blackboard: tasks/trial/blackboard\n",
                "Test compatibility."
            )
            .to_string(),
        },
        SpawnTask {
            summary: "interface review".to_string(),
            prompt: concat!(
                "Shared blackboard: tasks/trial/blackboard\n",
                "Review the interface."
            )
            .to_string(),
        },
    ];

    let envelope = task_envelope(&tasks[0], &tasks);

    assert!(envelope.contains("You are: parser"));
    assert!(envelope.contains("- compatibility tests\n- interface review"));
    assert!(envelope.contains("Shared blackboard: tasks/trial/blackboard"));
    assert!(envelope.ends_with(&format!("Assignment:\n{}", tasks[0].prompt)));
}

fn call(call_id: &str, namespace: Option<&str>, name: &str) -> RolloutItem {
    let is_spawn = name == "spine.spawn" || (namespace == Some("spine") && name == "spawn");
    RolloutItem::ResponseItem(ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: namespace.map(str::to_string),
        arguments: if is_spawn {
            r#"{"tasks":[{"summary":"one","prompt":"first"},{"summary":"two","prompt":"second"}]}"#
                .to_string()
        } else {
            "{}".to_string()
        },
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    })
}

fn message(role: &str, text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

fn output(call_id: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput {
        id: None,
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("done".to_string()),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    })
}

fn reasoning() -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Reasoning {
        id: None,
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "thinking".to_string(),
        }],
        content: None,
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

#[test]
fn exact_receipt_codec_preserves_all_semantic_fields() {
    let receipt = SpawnReceipt {
        schema: SPINE_SPAWN_RESULT_SCHEMA.to_string(),
        results: vec![SpawnResult {
            ordinal: 0,
            outcome: SpawnOutcome::Errored,
            memory_body: "truthful memory".to_string(),
            diagnostic: Some("child error".to_string()),
            execution_ref: Some("child-ref".to_string()),
        }],
    };

    assert_eq!(
        decode_receipt(&encode_receipt(&receipt).unwrap()).unwrap(),
        receipt
    );
    assert!(
        decode_receipt(r#"{"schema":"spine.spawn.result.v1","results":[],"extra":true}"#).is_err()
    );
}

#[test]
fn coordinator_helpers_keep_safe_names_and_truthful_terminal_results() {
    assert_eq!(transaction_task_name("Call-ID.42", 3), "spawn_callid42_3");

    let thread_id = codex_protocol::ThreadId::new();
    let completed = result_from_status(
        0,
        thread_id,
        AgentStatus::Completed(Some("final memory".to_string())),
        None,
    );
    assert_eq!(completed.outcome, SpawnOutcome::Completed);
    assert_eq!(completed.memory_body, "final memory");
    assert_eq!(completed.diagnostic, None);

    let missing = result_from_status(1, thread_id, AgentStatus::Completed(None), None);
    assert_eq!(missing.outcome, SpawnOutcome::Errored);
    assert!(missing.diagnostic.is_some());
    assert!(!missing.memory_body.trim().is_empty());

    assert!(is_spawn_terminal(&AgentStatus::Interrupted));
    let interrupted = result_from_status(2, thread_id, AgentStatus::Interrupted, None);
    assert_eq!(interrupted.outcome, SpawnOutcome::Aborted);

    let salvaged = result_from_status(
        3,
        thread_id,
        AgentStatus::Completed(None),
        Some(crate::spine::spawn_salvage::SpawnFailureRecord {
            diagnostic: "upstream 503".to_string(),
            salvaged_memory: Some("progress before failure".to_string()),
        }),
    );
    assert_eq!(salvaged.outcome, SpawnOutcome::Errored);
    assert_eq!(salvaged.memory_body, "progress before failure");
    assert_eq!(
        salvaged.diagnostic.as_deref(),
        Some("child errored: upstream 503")
    );

    let salvage_failed = result_from_status(
        4,
        thread_id,
        AgentStatus::Errored("upstream 503".to_string()),
        Some(crate::spine::spawn_salvage::SpawnFailureRecord {
            diagnostic: "upstream 503".to_string(),
            salvaged_memory: None,
        }),
    );
    assert_eq!(salvage_failed.outcome, SpawnOutcome::Errored);
    assert_eq!(
        salvage_failed.memory_body,
        salvage_failed.diagnostic.expect("original diagnostic")
    );
}

#[test]
fn subtree_membership_uses_agent_path_segment_boundaries() {
    let root = AgentPath::try_from("/root/spawn_a").unwrap();
    assert!(path_is_in_subtree(&root, &root));
    assert!(path_is_in_subtree(
        &AgentPath::try_from("/root/spawn_a/worker").unwrap(),
        &root,
    ));
    assert!(path_is_in_subtree(
        &AgentPath::try_from("/root/spawn_a/worker/deep").unwrap(),
        &root,
    ));
    assert!(!path_is_in_subtree(
        &AgentPath::try_from("/root/spawn_a2").unwrap(),
        &root,
    ));
    assert!(!path_is_in_subtree(
        &AgentPath::try_from("/root/other/spawn_a").unwrap(),
        &root,
    ));
}

#[test]
fn abort_barrier_blocks_new_admission_without_owning_transaction_cleanup() {
    let lifecycle = SpawnLifecycle::default();
    let transaction = lifecycle.try_enter().expect("first Spawn may enter");
    let abort_barrier = lifecycle.begin_abort();

    assert!(abort_barrier.had_active_transactions());
    assert!(lifecycle.try_enter().is_none());
    drop(transaction);
    assert!(lifecycle.try_enter().is_none());

    drop(abort_barrier);
    assert!(lifecycle.try_enter().is_some());
}

#[tokio::test]
async fn terminal_status_watch_reports_running_before_terminal() {
    let (status_tx, status_rx) = tokio::sync::watch::channel(AgentStatus::PendingInit);
    let (running_tx, running_rx) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        let mut running_tx = Some(running_tx);
        wait_for_terminal_status(status_rx, move |status| {
            let running_tx = (status == AgentStatus::Running)
                .then(|| running_tx.take())
                .flatten();
            async move {
                if let Some(running_tx) = running_tx {
                    let _ = running_tx.send(());
                }
            }
        })
        .await
    });

    status_tx
        .send(AgentStatus::Running)
        .expect("status watcher should still be active");
    tokio::time::timeout(Duration::from_secs(1), running_rx)
        .await
        .expect("Running progress was not reported")
        .expect("Running progress observer was dropped");
    status_tx
        .send(AgentStatus::Completed(Some("memory".to_string())))
        .expect("status watcher should still be active");

    assert_eq!(
        waiter.await.expect("status watcher task should not panic"),
        Some(AgentStatus::Completed(Some("memory".to_string())))
    );
}

#[tokio::test]
async fn terminal_status_watch_does_not_invent_running_for_fast_terminal() {
    let (_status_tx, status_rx) =
        tokio::sync::watch::channel(AgentStatus::Completed(Some("memory".to_string())));
    let mut observed = Vec::new();

    let terminal = wait_for_terminal_status(status_rx, |status| {
        observed.push(status);
        std::future::ready(())
    })
    .await;

    assert_eq!(
        (terminal, observed),
        (
            Some(AgentStatus::Completed(Some("memory".to_string()))),
            Vec::new()
        )
    );
}

#[test]
fn terminal_status_matrix_produces_one_total_ordered_receipt() {
    let tasks = (0..4)
        .map(|ordinal| codex_spine_core::SpawnTask {
            summary: format!("task {ordinal}"),
            prompt: format!("prompt {ordinal}"),
        })
        .collect::<Vec<_>>();
    let statuses = [
        AgentStatus::Completed(Some("completed memory".to_string())),
        AgentStatus::Completed(None),
        AgentStatus::Errored("provider failure".to_string()),
        AgentStatus::Shutdown,
    ];
    let normalized = statuses
        .into_iter()
        .enumerate()
        .map(|(ordinal, status)| {
            let result = result_from_status(
                ordinal,
                codex_protocol::ThreadId::new(),
                status.clone(),
                None,
            );
            let progress_status = result_status(&result, Some(&status));
            (Some(result), progress_status)
        })
        .collect::<Vec<_>>();
    let progress_statuses = normalized
        .iter()
        .map(|(_, status)| status.clone())
        .collect::<Vec<_>>();
    let results = normalized.into_iter().map(|(result, _)| result).collect();

    let receipt = finish_receipt(&tasks, results).expect("terminal matrix must be total");
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
    assert_eq!(receipt.results[0].diagnostic, None);
    assert!(matches!(
        progress_statuses.as_slice(),
        [
            AgentStatus::Completed(None),
            AgentStatus::Errored(missing),
            AgentStatus::Errored(error),
            AgentStatus::Shutdown,
        ] if missing.contains("non-empty final memory")
            && error.contains("provider failure")
    ));
    assert!(receipt.results[1..].iter().all(|result| {
        !result.memory_body.trim().is_empty()
            && result
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| !diagnostic.trim().is_empty())
    }));
}

#[test]
fn ambiguous_aborted_result_defaults_to_interrupted_without_observed_status() {
    let result = error_result(
        0,
        SpawnOutcome::Aborted,
        "spawn transaction was aborted".to_string(),
        None,
    );

    assert_eq!(result_status(&result, None), AgentStatus::Interrupted);
    assert_eq!(
        result_status(&result, Some(&AgentStatus::Interrupted)),
        AgentStatus::Interrupted
    );
    assert_eq!(
        result_status(&result, Some(&AgentStatus::Shutdown)),
        AgentStatus::Shutdown
    );
}

#[test]
fn partial_start_failure_is_total_and_keeps_input_ordinals() {
    let paths = vec![
        codex_protocol::AgentPath::try_from("/root/spawn_0").unwrap(),
        codex_protocol::AgentPath::try_from("/root/spawn_1").unwrap(),
        codex_protocol::AgentPath::try_from("/root/spawn_2").unwrap(),
    ];
    let first = codex_protocol::ThreadId::new();
    let third = codex_protocol::ThreadId::new();
    let StartPhase {
        live,
        mut results,
        failed,
    } = classify_start_results(
        &paths,
        [
            Ok((first, AgentStatus::PendingInit)),
            Err("injected start failure"),
            Ok((
                third,
                AgentStatus::Completed(Some("fast terminal".to_string())),
            )),
        ],
    );

    assert!(failed);
    assert_eq!(
        live.iter()
            .map(|(ordinal, thread_id, _, status)| (*ordinal, *thread_id, status.clone()))
            .collect::<Vec<_>>(),
        vec![
            (0, first, AgentStatus::PendingInit),
            (
                2,
                third,
                AgentStatus::Completed(Some("fast terminal".to_string())),
            ),
        ]
    );
    for (ordinal, thread_id, _, _) in live {
        results[ordinal] = Some(error_result(
            ordinal,
            SpawnOutcome::Aborted,
            "child aborted because another transaction child failed to start".to_string(),
            Some(thread_id.to_string()),
        ));
    }
    let tasks = vec![
        codex_spine_core::SpawnTask {
            summary: "zero".to_string(),
            prompt: "zero task".to_string(),
        },
        codex_spine_core::SpawnTask {
            summary: "one".to_string(),
            prompt: "one task".to_string(),
        },
        codex_spine_core::SpawnTask {
            summary: "two".to_string(),
            prompt: "two task".to_string(),
        },
    ];
    let receipt = finish_receipt(&tasks, results).unwrap();
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
    assert!(
        receipt.results[1]
            .diagnostic
            .as_deref()
            .is_some_and(|text| text.contains("injected start failure"))
    );
}

#[test]
fn batch_receipts_partition_flat_results_and_restore_task_ordinals() {
    let calls = vec![
        SpawnBatchCall {
            call_id: "spawn-1".to_string(),
            fork_parent_call_id: "spawn-1".to_string(),
            tasks: parse_tasks(
                r#"{"tasks":[{"summary":"a","prompt":"pa"},{"summary":"b","prompt":"pb"}]}"#,
            )
            .unwrap(),
        },
        SpawnBatchCall {
            call_id: "spawn-2".to_string(),
            fork_parent_call_id: "spawn-2".to_string(),
            tasks: parse_tasks(
                r#"{"tasks":[{"summary":"c","prompt":"pc"},{"summary":"d","prompt":"pd"}]}"#,
            )
            .unwrap(),
        },
    ];
    let results = (0..4)
        .map(|ordinal| {
            Some(SpawnResult {
                ordinal,
                outcome: SpawnOutcome::Completed,
                memory_body: format!("memory-{ordinal}"),
                diagnostic: None,
                execution_ref: None,
            })
        })
        .collect();

    let receipts = finish_batch_receipts(&calls, results).unwrap();
    assert_eq!(
        receipts["spawn-1"]
            .results
            .iter()
            .map(|result| result.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert_eq!(
        receipts["spawn-2"]
            .results
            .iter()
            .map(|result| (result.ordinal, result.memory_body.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "memory-2"), (1, "memory-3")]
    );
}

#[test]
fn capacity_rejection_partitions_multiple_calls_without_losing_task_identity() {
    let calls = vec![
        SpawnBatchCall {
            call_id: "spawn-1".to_string(),
            fork_parent_call_id: "spawn-1".to_string(),
            tasks: parse_tasks(
                r#"{"tasks":[{"summary":"a","prompt":"pa"},{"summary":"b","prompt":"pb"}]}"#,
            )
            .unwrap(),
        },
        SpawnBatchCall {
            call_id: "spawn-2".to_string(),
            fork_parent_call_id: "spawn-2".to_string(),
            tasks: parse_tasks(
                r#"{"tasks":[{"summary":"c","prompt":"pc"},{"summary":"d","prompt":"pd"}]}"#,
            )
            .unwrap(),
        },
    ];

    let receipts =
        capacity_rejection_receipts(&calls, /*task_count*/ 4, /*max_threads*/ 3)
            .expect("capacity rejection must produce complete receipts");

    for (call_ordinal, call) in calls.iter().enumerate() {
        let receipt = &receipts[&call.call_id];
        assert_eq!(receipt.results.len(), call.tasks.len());
        for (task_ordinal, (result, task)) in receipt.results.iter().zip(&call.tasks).enumerate() {
            let batch_ordinal = call_ordinal * call.tasks.len() + task_ordinal + 1;
            assert_eq!(result.ordinal, task_ordinal as u32);
            assert_eq!(result.outcome, SpawnOutcome::Errored);
            assert_eq!(result.execution_ref, None);
            let diagnostic = result.diagnostic.as_deref().unwrap();
            assert_eq!(result.memory_body, diagnostic);
            assert!(diagnostic.contains(&format!("task {batch_ordinal}/4")));
            assert!(diagnostic.contains(&format!("(`{}`)", task.summary)));
            assert!(diagnostic.contains("configured limit of 3"));
        }
    }
}

#[test]
fn response_group_admission_accepts_flat_and_namespaced_spawn_calls() {
    for rollout in [
        vec![call("spawn", None, "spine.spawn")],
        vec![call("spawn", Some("spine"), "spawn")],
    ] {
        let calls = calls_in_response_group(&rollout, "spawn").unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tasks.len(), 2);
    }
}

#[test]
fn response_group_admission_uses_native_response_group_boundaries() {
    let rollout = [
        message("user", "first turn"),
        call("previous", None, "shell"),
        output("previous"),
        message("user", "spawn now"),
        call("spawn", Some("spine"), "spawn"),
        output("later"),
    ];
    let calls = calls_in_response_group(&rollout, "spawn").unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["spawn"]
    );
}

#[test]
fn response_group_admission_accepts_text_reasoning_and_ordinary_sibling_calls() {
    for rollout in [
        vec![
            message("assistant", "extra"),
            call("spawn", None, "spine.spawn"),
        ],
        vec![reasoning(), call("spawn", None, "spine.spawn")],
        vec![
            call("spawn", None, "spine.spawn"),
            call("shell", None, "shell"),
        ],
    ] {
        assert_eq!(calls_in_response_group(&rollout, "spawn").unwrap().len(), 1);
    }
}

#[test]
fn response_group_admission_rejects_multiple_spawn_calls() {
    let multiple = vec![
        call("spawn-1", None, "spine.spawn"),
        call("spawn-2", Some("spine"), "spawn"),
    ];
    let error = calls_in_response_group(&multiple, "spawn-2")
        .expect_err("multiple spine.spawn calls must be rejected before execution");
    assert_eq!(
        error,
        "spine.spawn may be called at most once in one model response"
    );
}

#[test]
fn response_group_admission_rejects_conflicting_spine_controls() {
    for control in ["spine.open", "spine.close", "spine.next"] {
        let rollout = vec![
            call("spawn", None, "spine.spawn"),
            call("control", None, control),
        ];
        assert!(calls_in_response_group(&rollout, "spawn").is_err());
    }
}

#[test]
fn progress_event_carries_the_exact_child_thread_id_for_each_task() {
    let tasks = vec![
        SpawnTask {
            summary: "first".to_string(),
            prompt: "one".to_string(),
        },
        SpawnTask {
            summary: "second".to_string(),
            prompt: "two".to_string(),
        },
    ];
    let thread_ids = [
        codex_protocol::ThreadId::new(),
        codex_protocol::ThreadId::new(),
    ];
    let paths = [
        AgentPath::root().join("first").unwrap(),
        AgentPath::root().join("second").unwrap(),
    ];
    let statuses = [AgentStatus::Running, AgentStatus::PendingInit];

    let event = spawn_progress_event("spawn-1", &tasks, &thread_ids, &paths, &statuses);

    assert_eq!(
        event
            .tasks
            .iter()
            .map(|task| task.thread_id)
            .collect::<Vec<_>>(),
        thread_ids
    );
}

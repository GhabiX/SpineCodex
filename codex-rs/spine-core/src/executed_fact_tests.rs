use super::*;
use crate::MAX_SPAWN_BATCH_BYTES;
use crate::executed_fact::MAX_EXECUTION_ORIGIN_BYTES;
use crate::identity::ContextEpoch;
use crate::identity::ThreadNamespace;
use crate::model::SpawnOutcome;
use pretty_assertions::assert_eq;
use serde_json::json;

fn namespace() -> ThreadNamespace {
    ThreadNamespace::parse("thread-a").expect("valid namespace")
}

fn execution_id() -> ExecutionId {
    ExecutionId::parse(namespace(), "execution-7").expect("valid execution ID")
}

fn direct_fact(operation: SpineOperationFact) -> ExecutedSpineFact {
    ExecutedSpineFact {
        execution_id: execution_id(),
        ordinal: AdmissionOrdinal::new(3),
        origin: ExecutionOrigin::Direct {
            call_id: "call-3".to_string(),
        },
        operation,
    }
}

fn spawn_tasks() -> Vec<SpawnTask> {
    vec![
        SpawnTask {
            summary: "first".to_string(),
            prompt: "do first".to_string(),
        },
        SpawnTask {
            summary: "second".to_string(),
            prompt: "do second".to_string(),
        },
    ]
}

fn terminal_results() -> Vec<SpawnResult> {
    vec![
        SpawnResult {
            ordinal: 0,
            outcome: SpawnOutcome::Completed,
            memory_body: "first memory".to_string(),
            diagnostic: None,
            execution_ref: Some("exec-0".to_string()),
        },
        SpawnResult {
            ordinal: 1,
            outcome: SpawnOutcome::Errored,
            memory_body: "second error memory".to_string(),
            diagnostic: Some("failed".to_string()),
            execution_ref: Some("exec-1".to_string()),
        },
    ]
}

fn stable_trim() -> (TrimTicket, StableToolOutputId) {
    let thread = namespace();
    let epoch = ContextEpoch::new(2);
    let request = SourceCellId::new(thread.clone(), epoch, 10);
    let response = SourceCellId::new(thread.clone(), epoch, 11);
    let ticket = TrimTicket::parse(thread, epoch, "trim-ticket-1").expect("valid trim ticket");
    (
        ticket,
        StableToolOutputId {
            request,
            response,
            call_id: "tool-call".to_string(),
        },
    )
}

#[test]
fn executed_spine_fact_validates_all_five_variants() {
    let (ticket, target) = stable_trim();
    let facts = [
        direct_fact(SpineOperationFact::Open {
            summary: "open scope".to_string(),
        }),
        direct_fact(SpineOperationFact::Close {
            memory: "closed memory".to_string(),
        }),
        direct_fact(SpineOperationFact::Next {
            closed_memory: "closed memory".to_string(),
            next_summary: "next scope".to_string(),
        }),
        direct_fact(SpineOperationFact::Spawn {
            tasks: spawn_tasks(),
            terminal_results: terminal_results(),
        }),
        direct_fact(SpineOperationFact::Trim {
            ticket,
            target,
            validated_edit: TrimEdit::Snipped,
            source_digest: "source-digest".to_string(),
        }),
    ];

    assert_eq!(
        facts.map(|fact| fact.validate()),
        [Ok(()), Ok(()), Ok(()), Ok(()), Ok(())]
    );
}

#[test]
fn executed_spine_fact_next_is_one_atomic_serialized_operation() {
    let fact = direct_fact(SpineOperationFact::Next {
        closed_memory: "done".to_string(),
        next_summary: "next".to_string(),
    });

    let encoded = serde_json::to_value(&fact).expect("serialize next fact");
    assert_eq!(
        encoded["operation"],
        json!({
            "type": "next",
            "closed_memory": "done",
            "next_summary": "next",
        })
    );
    assert_eq!(
        serde_json::from_value::<ExecutedSpineFact>(encoded).expect("deserialize next fact"),
        fact
    );
}

#[test]
fn executed_spine_fact_spawn_requires_terminal_results_in_task_order() {
    let valid = direct_fact(SpineOperationFact::Spawn {
        tasks: spawn_tasks(),
        terminal_results: terminal_results(),
    });
    assert_eq!(valid.validate(), Ok(()));

    let mut reversed_results = terminal_results();
    reversed_results.reverse();
    let reversed = direct_fact(SpineOperationFact::Spawn {
        tasks: spawn_tasks(),
        terminal_results: reversed_results,
    });
    assert!(matches!(
        reversed.validate(),
        Err(ExecutedFactError::InvalidSpawn(
            SpawnValidationError::ResultOrdinal {
                expected: 0,
                actual: 1
            }
        ))
    ));

    let missing = direct_fact(SpineOperationFact::Spawn {
        tasks: spawn_tasks(),
        terminal_results: terminal_results().into_iter().take(1).collect(),
    });
    assert!(matches!(
        missing.validate(),
        Err(ExecutedFactError::InvalidSpawn(
            SpawnValidationError::ResultCount {
                expected: 2,
                actual: 1
            }
        ))
    ));
}

#[test]
fn executed_spine_fact_rejects_field_origin_and_total_payload_bounds() {
    let summary = direct_fact(SpineOperationFact::Open {
        summary: "s".repeat(MAX_SUMMARY_BYTES + 1),
    });
    assert!(matches!(
        summary.validate(),
        Err(ExecutedFactError::FieldTooLarge {
            field: "summary",
            ..
        })
    ));

    let memory = direct_fact(SpineOperationFact::Close {
        memory: "m".repeat(MAX_MEMORY_BYTES + 1),
    });
    assert!(matches!(
        memory.validate(),
        Err(ExecutedFactError::FieldTooLarge {
            field: "memory",
            ..
        })
    ));

    let mut origin = direct_fact(SpineOperationFact::Open {
        summary: "scope".to_string(),
    });
    origin.origin = ExecutionOrigin::Direct {
        call_id: "c".repeat(MAX_EXECUTION_ORIGIN_BYTES + 1),
    };
    assert!(matches!(
        origin.validate(),
        Err(ExecutedFactError::FieldTooLarge {
            field: "origin.call_id",
            ..
        })
    ));

    let nul_prompt = "\0".repeat((MAX_SPAWN_BATCH_BYTES - 2) / 2);
    let nul_memory = "\0".repeat(MAX_MEMORY_BYTES);
    let payload = direct_fact(SpineOperationFact::Spawn {
        tasks: vec![
            SpawnTask {
                summary: "a".to_string(),
                prompt: nul_prompt.clone(),
            },
            SpawnTask {
                summary: "b".to_string(),
                prompt: nul_prompt,
            },
        ],
        terminal_results: vec![
            SpawnResult {
                ordinal: 0,
                outcome: SpawnOutcome::Completed,
                memory_body: nul_memory.clone(),
                diagnostic: None,
                execution_ref: None,
            },
            SpawnResult {
                ordinal: 1,
                outcome: SpawnOutcome::Completed,
                memory_body: nul_memory,
                diagnostic: None,
                execution_ref: None,
            },
        ],
    });
    assert!(matches!(
        payload.validate(),
        Err(ExecutedFactError::PayloadTooLarge { .. })
    ));
}

#[test]
fn executed_spine_fact_trim_requires_a_stable_epoch_bound_ticket() {
    let (ticket, target) = stable_trim();
    let valid = direct_fact(SpineOperationFact::Trim {
        ticket: ticket.clone(),
        target: target.clone(),
        validated_edit: TrimEdit::Sliced("retained".to_string()),
        source_digest: "source-digest".to_string(),
    });
    assert_eq!(valid.validate(), Ok(()));

    let wrong_response = SourceCellId::new(namespace(), ContextEpoch::new(3), 13);
    let invalid = direct_fact(SpineOperationFact::Trim {
        ticket,
        target: StableToolOutputId {
            response: wrong_response,
            ..target
        },
        validated_edit: TrimEdit::Snipped,
        source_digest: "source-digest".to_string(),
    });
    assert!(matches!(
        invalid.validate(),
        Err(ExecutedFactError::InvalidTrimTicket(
            "request and response must belong to the same epoch"
        ))
    ));
}

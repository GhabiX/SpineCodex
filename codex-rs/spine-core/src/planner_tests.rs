use crate::AdmissionOrdinal;
use crate::BoundaryId;
use crate::ContextEpoch;
use crate::ContextLabel;
use crate::ExecutionId;
use crate::ExecutionOrigin;
use crate::Feature;
use crate::Message;
use crate::MessageRole;
use crate::RawBoundary;
use crate::SamplingAttemptId;
use crate::SamplingCommitId;
use crate::SourceCellId;
use crate::SpineChar;
use crate::SpineConfig;
use crate::SpineFactKind;
use crate::SpineFactReservation;
use crate::SpineOperationFact;
use crate::ThreadNamespace;
use crate::ToolOutcome;
use crate::ToolRequestChar;
use crate::ToolResponseChar;
use crate::archive::MAX_ARCHIVE_RECORD_BYTES;
use pretty_assertions::assert_eq;

use super::*;
use crate::planner::SamplingCommitOutput;

fn namespace() -> ThreadNamespace {
    ThreadNamespace::parse("thread-planner").expect("namespace")
}

fn config() -> SpineConfig {
    SpineConfig::default()
        .with_feature(Feature::Jit)
        .expect("JIT config")
}

fn trim_config() -> SpineConfig {
    SpineConfig::default()
        .with_features([Feature::Jit, Feature::Trim])
        .expect("trim config")
}

fn attempt_id(value: &str) -> SamplingAttemptId {
    SamplingAttemptId::parse(namespace(), value).expect("attempt")
}

fn commit_id(value: &str) -> SamplingCommitId {
    SamplingCommitId::parse(namespace(), value).expect("commit")
}

fn user(boundary: u64, body: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role: MessageRole::User,
        content: body.to_string(),
    })
}

fn assistant(boundary: u64, body: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role: MessageRole::Assistant,
        content: body.to_string(),
    })
}

fn tool_group(
    request_boundary: u64,
    response_boundary: u64,
    call_id: &str,
    name: &str,
) -> [SpineChar; 2] {
    [
        SpineChar::ToolRequest(ToolRequestChar {
            boundary: RawBoundary(request_boundary),
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
        }),
        SpineChar::ToolResponse(ToolResponseChar {
            boundary: RawBoundary(response_boundary),
            call_id: call_id.to_string(),
            outcome: ToolOutcome::Succeeded,
            output: "ok".to_string(),
        }),
    ]
}

fn complete_fact(
    handle: &crate::SamplingHandle,
    execution: &str,
    ordinal: u64,
    kind: SpineFactKind,
    call_id: &str,
    operation: SpineOperationFact,
) {
    let reservation = SpineFactReservation::new(
        handle.attempt(),
        ExecutionId::parse(namespace(), execution).expect("execution"),
        AdmissionOrdinal::new(ordinal),
        kind,
    )
    .expect("reservation");
    let sink = handle.fact_sink();
    let permit = sink.reserve(reservation.clone()).expect("permit");
    sink.complete(
        permit,
        crate::ExecutedSpineFact {
            execution_id: reservation.execution_id().clone(),
            ordinal: reservation.ordinal(),
            origin: ExecutionOrigin::Direct {
                call_id: call_id.to_string(),
            },
            operation,
        },
    )
    .expect("complete");
}

fn finish_sampling(
    planner: &mut SamplingPlanner,
    handle: &crate::SamplingHandle,
    post_boundary: u64,
    commit: &str,
) -> SamplingCommitOutput {
    finish_sampling_with_input_tokens(
        planner,
        handle,
        post_boundary,
        commit,
        /*input_tokens*/ None,
    )
}

fn finish_sampling_with_input_tokens(
    planner: &mut SamplingPlanner,
    handle: &crate::SamplingHandle,
    post_boundary: u64,
    commit: &str,
    input_tokens: Option<u64>,
) -> SamplingCommitOutput {
    let sealed = handle.seal(ContextEpoch::ZERO).expect("seal");
    let prepared = planner
        .prepare_sampling_with_input_tokens(
            sealed,
            BoundaryId::new(namespace(), ContextEpoch::ZERO, post_boundary),
            commit_id(commit),
            RecordDigest::digest(b"sampling-started"),
            input_tokens,
        )
        .expect("prepare");
    planner.install_prepared(prepared).expect("install")
}

#[test]
fn sampling_events_remain_bounded_across_large_tool_outputs() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, trim_config()).expect("planner");
    planner.observe_source([user(1, "request")]).expect("user");

    for ordinal in 0..10_u64 {
        let handle = planner
            .begin_sampling(attempt_id(&format!("attempt-large-{ordinal}")))
            .expect("begin");
        let started = planner
            .sampling_started_record(&handle, RecordDigest::digest(b"prompt"))
            .expect("sampling started");
        assert!(matches!(started, SamplingArchiveRecord::SamplingStarted(_)));

        let request_boundary = ordinal * 2 + 2;
        let response_boundary = request_boundary + 1;
        planner
            .observe_source([
                SpineChar::ToolRequest(ToolRequestChar {
                    boundary: RawBoundary(request_boundary),
                    call_id: format!("large-{ordinal}"),
                    name: "shell".to_string(),
                    arguments: "{}".to_string(),
                }),
                SpineChar::ToolResponse(ToolResponseChar {
                    boundary: RawBoundary(response_boundary),
                    call_id: format!("large-{ordinal}"),
                    outcome: ToolOutcome::Succeeded,
                    output: "x".repeat(40 * 1024),
                }),
            ])
            .expect("large output");

        let output = finish_sampling(
            &mut planner,
            &handle,
            response_boundary,
            &format!("commit-large-{ordinal}"),
        );
        let encoded = SamplingArchiveRecord::SamplingCommit(output.record)
            .encode()
            .expect("bounded event");
        assert!(encoded.len() < MAX_ARCHIVE_RECORD_BYTES);
    }
}

#[test]
fn sampling_planner_prepares_without_mutating_and_installs_once() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    planner.observe_source([user(1, "request")]).expect("user");
    let handle = planner
        .begin_sampling(attempt_id("attempt-open"))
        .expect("begin");
    planner
        .observe_source(tool_group(2, 3, "open", "spine.open"))
        .expect("tool source");
    complete_fact(
        &handle,
        "execution-open",
        1,
        SpineFactKind::Open,
        "open",
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    );
    let sealed = handle.seal(ContextEpoch::ZERO).expect("seal");
    let before = planner.projection().clone();
    let prepared = planner
        .prepare_sampling(
            sealed,
            BoundaryId::new(namespace(), ContextEpoch::ZERO, 3),
            commit_id("commit-open"),
            RecordDigest::digest(b"sampling-started"),
        )
        .expect("prepare");

    assert_eq!(planner.projection(), &before);
    let [execution] = prepared.durable_record().executions.as_slice() else {
        panic!("open sampling must archive exactly one execution");
    };
    assert_eq!(
        execution.operation,
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        }
    );
    assert_ne!(execution.source_span.start, execution.source_span.end);
    assert_eq!(
        prepared.context_plan().source_snapshot_digest,
        prepared.durable_record().source_digest
    );
    let output = planner.install_prepared(prepared).expect("install");
    assert_eq!(planner.projection(), &output.projection);
    assert_eq!(planner.projection().nodes.len(), 2);
}

#[test]
fn sampling_planner_does_not_infer_transition_from_source_text() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    planner.observe_source([user(1, "request")]).expect("user");
    let handle = planner
        .begin_sampling(attempt_id("attempt-source-only"))
        .expect("begin");
    planner
        .observe_source(tool_group(2, 3, "looks-structural", "spine.open"))
        .expect("source");
    let sealed = handle.seal(ContextEpoch::ZERO).expect("seal");
    let prepared = planner
        .prepare_sampling(
            sealed,
            BoundaryId::new(namespace(), ContextEpoch::ZERO, 3),
            commit_id("commit-source-only"),
            RecordDigest::digest(b"sampling-started"),
        )
        .expect("prepare");
    let output = planner.install_prepared(prepared).expect("install");

    assert_eq!(output.record.executions, Vec::new());
    assert_eq!(output.projection.nodes.len(), 1);
}

#[test]
fn sampling_planner_preserves_source_identity_and_user_anchor() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    let user_id = planner.observe_source([user(1, "request")]).expect("user")[0].clone();
    let handle = planner
        .begin_sampling(attempt_id("attempt-empty"))
        .expect("begin");
    planner
        .observe_source([SpineChar::Message(Message {
            boundary: RawBoundary(2),
            role: MessageRole::Assistant,
            content: "answer".to_string(),
        })])
        .expect("assistant");
    let sealed = handle.seal(ContextEpoch::ZERO).expect("seal");
    let prepared = planner
        .prepare_sampling(
            sealed,
            BoundaryId::new(namespace(), ContextEpoch::ZERO, 2),
            commit_id("commit-empty"),
            RecordDigest::digest(b"sampling-started"),
        )
        .expect("prepare");
    let output = planner.install_prepared(prepared).expect("install");

    assert!(output.plan.cells.iter().any(|cell| {
        matches!(
            cell,
            crate::ContextPlanCell::Source { source_id, labels }
                if source_id == &user_id && labels == &[ContextLabel::UserAnchor(1)]
        )
    }));
}

#[test]
fn sampling_planner_flushes_pre_sampling_assistant_source() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    let source = planner
        .observe_source([user(1, "request"), assistant(2, "prefill")])
        .expect("pre-sampling source");
    let handle = planner
        .begin_sampling(attempt_id("attempt-pre-sampling-assistant"))
        .expect("begin");
    let output = finish_sampling(&mut planner, &handle, 2, "commit-pre-sampling-assistant");

    assert_eq!(
        output
            .plan
            .cells
            .iter()
            .filter_map(|cell| match cell {
                crate::ContextPlanCell::Source { source_id, .. } => Some(source_id),
                crate::ContextPlanCell::Projection { .. } => None,
            })
            .collect::<Vec<_>>(),
        source.iter().collect::<Vec<_>>()
    );
    assert!(matches!(
        output.projection.visible_context.as_slice(),
        [
            crate::ContextItem::Message {
                message: Message {
                    role: MessageRole::User,
                    ..
                },
                ..
            },
            crate::ContextItem::Message {
                message: Message {
                    role: MessageRole::Assistant,
                    content,
                    ..
                },
                ..
            }
        ] if content == "prefill"
    ));
}

#[test]
fn spawn_labels_are_scoped_to_response_boundaries_when_call_ids_repeat() {
    let spawn_config = SpineConfig::default()
        .with_features([Feature::Jit, Feature::Spawn])
        .expect("spawn config");
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, spawn_config).expect("planner");

    let first = planner
        .begin_sampling(attempt_id("attempt-reused-spawn-success"))
        .expect("begin first");
    let first_source = planner
        .observe_source(tool_group(1, 2, "reused-spawn", "spine.spawn"))
        .expect("first spawn source");
    finish_sampling(&mut planner, &first, 2, "commit-reused-spawn-success");

    let second = planner
        .begin_sampling(attempt_id("attempt-reused-spawn-failure"))
        .expect("begin second");
    let second_source = planner
        .observe_source([
            SpineChar::ToolRequest(ToolRequestChar {
                boundary: RawBoundary(3),
                call_id: "reused-spawn".to_string(),
                name: "spine.spawn".to_string(),
                arguments: "{}".to_string(),
            }),
            SpineChar::ToolResponse(ToolResponseChar {
                boundary: RawBoundary(4),
                call_id: "reused-spawn".to_string(),
                outcome: ToolOutcome::Failed,
                output: "failed".to_string(),
            }),
        ])
        .expect("second spawn source");
    let output = finish_sampling(&mut planner, &second, 4, "commit-reused-spawn-failure");

    let labels = output
        .plan
        .cells
        .iter()
        .filter_map(|cell| match cell {
            crate::ContextPlanCell::Source { source_id, labels }
                if source_id == &first_source[1] || source_id == &second_source[1] =>
            {
                Some((source_id, labels.as_slice()))
            }
            crate::ContextPlanCell::Source { .. } | crate::ContextPlanCell::Projection { .. } => {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec![
            (
                &first_source[1],
                &[ContextLabel::SpawnOutput { succeeded: true }][..],
            ),
            (
                &second_source[1],
                &[ContextLabel::SpawnOutput { succeeded: false }][..],
            ),
        ]
    );
}

#[test]
fn sampling_planner_rejects_fact_without_matching_source_group() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    planner.observe_source([user(1, "request")]).expect("user");
    let handle = planner
        .begin_sampling(attempt_id("attempt-unmatched"))
        .expect("begin");
    planner
        .observe_source(tool_group(2, 3, "actual", "tool"))
        .expect("source");
    complete_fact(
        &handle,
        "execution-unmatched",
        1,
        SpineFactKind::Open,
        "missing",
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    );
    let sealed = handle.seal(ContextEpoch::ZERO).expect("seal");

    assert!(matches!(
        planner.prepare_sampling(
            sealed,
            BoundaryId::new(namespace(), ContextEpoch::ZERO, 3),
            commit_id("commit-unmatched"),
            RecordDigest::digest(b"sampling-started"),
        ),
        Err(PlannerError::FactHasNoSourceGroup(_))
    ));
}

#[test]
fn sampling_planner_rejects_post_boundary_race() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    planner.observe_source([user(1, "request")]).expect("user");
    let handle = planner
        .begin_sampling(attempt_id("attempt-race"))
        .expect("begin");
    planner
        .observe_source([user(2, "new source")])
        .expect("source");
    let sealed = handle.seal(ContextEpoch::ZERO).expect("seal");

    assert!(matches!(
        planner.prepare_sampling(
            sealed,
            BoundaryId::new(namespace(), ContextEpoch::ZERO, 1),
            commit_id("commit-race"),
            RecordDigest::digest(b"sampling-started"),
        ),
        Err(PlannerError::PostBoundaryIsNotSourceTail)
    ));
}

#[test]
fn sampling_planner_rejects_stale_trim_source_identity() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, trim_config()).expect("planner");
    planner.observe_source([user(1, "request")]).expect("user");
    let handle = planner
        .begin_sampling(attempt_id("attempt-trim"))
        .expect("begin");
    planner
        .observe_source(tool_group(2, 3, "trim", "spine.trim"))
        .expect("source");
    let target = crate::StableToolOutputId {
        request: SourceCellId::new(namespace(), ContextEpoch::ZERO, 99),
        response: SourceCellId::new(namespace(), ContextEpoch::ZERO, 100),
        call_id: "missing".to_string(),
    };
    complete_fact(
        &handle,
        "execution-trim",
        1,
        SpineFactKind::Trim,
        "trim",
        SpineOperationFact::Trim {
            ticket: crate::TrimTicket::parse(namespace(), ContextEpoch::ZERO, "ticket")
                .expect("ticket"),
            target,
            validated_edit: crate::TrimEdit::Snipped,
            source_digest: "digest".to_string(),
        },
    );
    let sealed = handle.seal(ContextEpoch::ZERO).expect("seal");

    assert!(matches!(
        planner.prepare_sampling(
            sealed,
            BoundaryId::new(namespace(), ContextEpoch::ZERO, 3),
            commit_id("commit-trim"),
            RecordDigest::digest(b"sampling-started"),
        ),
        Err(PlannerError::MissingTrimSource(_))
    ));
}

#[test]
fn sampling_planner_next_preserves_memory_and_rebases_context_cost() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    planner
        .observe_source([user(1, "root request")])
        .expect("user");
    let open = planner
        .begin_sampling(attempt_id("attempt-open-memory"))
        .expect("begin open");
    planner
        .observe_source(tool_group(2, 3, "open-memory", "spine.open"))
        .expect("open source");
    complete_fact(
        &open,
        "execution-open-memory",
        1,
        SpineFactKind::Open,
        "open-memory",
        SpineOperationFact::Open {
            summary: "task".to_string(),
        },
    );
    finish_sampling_with_input_tokens(
        &mut planner,
        &open,
        3,
        "commit-open-memory",
        /*input_tokens*/ Some(10_001),
    );

    let child_user = planner
        .observe_source([user(4, "child request")])
        .expect("child user")[0]
        .clone();
    let next = planner
        .begin_sampling(attempt_id("attempt-next-memory"))
        .expect("begin next");
    planner
        .observe_source(tool_group(5, 6, "next-memory", "spine.next"))
        .expect("next source");
    complete_fact(
        &next,
        "execution-next-memory",
        1,
        SpineFactKind::Next,
        "next-memory",
        SpineOperationFact::Next {
            closed_memory: "finished first task".to_string(),
            next_summary: "second task".to_string(),
        },
    );
    let output = finish_sampling_with_input_tokens(
        &mut planner,
        &next,
        6,
        "commit-next-memory",
        /*input_tokens*/ Some(70_000),
    );

    assert_eq!(output.projection.nodes.len(), 3);
    assert_eq!(
        planner
            .node_context_costs(&[
                crate::ContextWindowSample {
                    boundary: RawBoundary(6),
                    model_context_window: 80_000,
                },
                crate::ContextWindowSample {
                    boundary: RawBoundary(7),
                    model_context_window: 40_000,
                },
            ])
            .get(&crate::NodeId::root_epoch(1).child(2)),
        Some(&crate::NodeContextCost::Percentage(13))
    );
    assert_eq!(
        output.plan.memory_slots,
        output
            .projection
            .nodes
            .iter()
            .flat_map(|node| node.memory.iter().flatten())
            .cloned()
            .collect::<Vec<_>>()
    );
    assert!(output.plan.memory_slots.iter().any(|slot| {
        matches!(
            slot,
            crate::MemorySlot::User { message, .. }
                if message.content == "child request"
        )
    }));
    assert!(output.plan.memory_slots.iter().any(|slot| {
        matches!(
            slot,
            crate::MemorySlot::Summary { body, .. }
                if body == "finished first task"
        )
    }));
    assert!(output.plan.cells.iter().any(|cell| {
        matches!(
            cell,
            crate::ContextPlanCell::Source { source_id, labels }
                if *source_id == child_user && labels == &[ContextLabel::UserAnchor(2)]
        )
    }));

    let close = planner
        .begin_sampling(attempt_id("attempt-close-memory"))
        .expect("begin close");
    let close_source = planner
        .observe_source(tool_group(7, 8, "close-memory", "spine.close"))
        .expect("close source");
    complete_fact(
        &close,
        "execution-close-memory",
        1,
        SpineFactKind::Close,
        "close-memory",
        SpineOperationFact::Close {
            memory: "finished second task".to_string(),
        },
    );
    let output = finish_sampling(&mut planner, &close, 8, "commit-close-memory");
    let [execution] = output.record.executions.as_slice() else {
        panic!("close commit must contain one execution");
    };
    assert_eq!(execution.source_span.start, close_source[0]);
    assert_eq!(execution.source_span.end, close_source[1]);
    assert!(output.plan.cells.iter().any(|cell| {
        matches!(
            cell,
            crate::ContextPlanCell::Projection {
                item: crate::ContextItem::MemorySlot(crate::MemorySlot::Summary { body, .. }),
                ..
            } if body == "finished second task"
        )
    }));
    for source_id in close_source {
        assert!(output.plan.cells.iter().any(|cell| {
            matches!(
                cell,
                crate::ContextPlanCell::Source {
                    source_id: planned,
                    ..
                } if *planned == source_id
            )
        }));
    }
}

#[test]
fn sampling_planner_applies_terminal_spawn_and_stable_trim_facts() {
    let spawn_config = SpineConfig::default()
        .with_features([Feature::Jit, Feature::Spawn])
        .expect("spawn config");
    let mut spawn_planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, spawn_config).expect("planner");
    spawn_planner
        .observe_source([user(1, "spawn request")])
        .expect("user");
    let spawn = spawn_planner
        .begin_sampling(attempt_id("attempt-spawn"))
        .expect("begin");
    let spawn_source = spawn_planner
        .observe_source(tool_group(2, 3, "spawn", "spine.spawn"))
        .expect("spawn source");
    complete_fact(
        &spawn,
        "execution-spawn",
        1,
        SpineFactKind::Spawn,
        "spawn",
        SpineOperationFact::Spawn {
            tasks: vec![
                crate::SpawnTask {
                    summary: "first".to_string(),
                    prompt: "inspect first".to_string(),
                },
                crate::SpawnTask {
                    summary: "second".to_string(),
                    prompt: "inspect second".to_string(),
                },
            ],
            terminal_results: vec![
                crate::SpawnResult {
                    ordinal: 0,
                    outcome: crate::SpawnOutcome::Completed,
                    memory_body: "first memory".to_string(),
                    diagnostic: None,
                    execution_ref: Some("exec-first".to_string()),
                },
                crate::SpawnResult {
                    ordinal: 1,
                    outcome: crate::SpawnOutcome::Errored,
                    memory_body: "second memory".to_string(),
                    diagnostic: Some("failed".to_string()),
                    execution_ref: Some("exec-second".to_string()),
                },
            ],
        },
    );
    let spawn_output = finish_sampling(&mut spawn_planner, &spawn, 3, "commit-spawn");
    assert_eq!(
        spawn_output
            .plan
            .memory_slots
            .iter()
            .filter(|slot| matches!(slot, crate::MemorySlot::SpawnEvidence { .. }))
            .count(),
        2
    );
    assert!(spawn_output.plan.cells.iter().any(|cell| {
        matches!(
            cell,
            crate::ContextPlanCell::Source { source_id, labels }
                if *source_id == spawn_source[1]
                    && labels == &[ContextLabel::SpawnOutput { succeeded: true }]
        )
    }));

    let followup = spawn_planner
        .begin_sampling(attempt_id("attempt-spawn-followup"))
        .expect("begin followup");
    spawn_planner
        .observe_source([SpineChar::Message(Message {
            boundary: RawBoundary(4),
            role: MessageRole::Assistant,
            content: "continued".to_string(),
        })])
        .expect("followup source");
    let followup_output =
        finish_sampling(&mut spawn_planner, &followup, 4, "commit-spawn-followup");
    assert!(followup_output.plan.cells.iter().any(|cell| {
        matches!(
            cell,
            crate::ContextPlanCell::Source { source_id, labels }
                if *source_id == spawn_source[1]
                    && labels == &[ContextLabel::SpawnOutput { succeeded: true }]
        )
    }));

    let mut trim_planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, trim_config()).expect("planner");
    trim_planner
        .observe_source([user(1, "trim request")])
        .expect("user");
    let source_sampling = trim_planner
        .begin_sampling(attempt_id("attempt-trim-source"))
        .expect("begin source");
    let source_ids = trim_planner
        .observe_source([
            SpineChar::ToolRequest(ToolRequestChar {
                boundary: RawBoundary(2),
                call_id: "large-output".to_string(),
                name: "shell".to_string(),
                arguments: "{}".to_string(),
            }),
            SpineChar::ToolResponse(ToolResponseChar {
                boundary: RawBoundary(3),
                call_id: "large-output".to_string(),
                outcome: ToolOutcome::Succeeded,
                output: "x".repeat(crate::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES + 1),
            }),
        ])
        .expect("large output");
    finish_sampling(&mut trim_planner, &source_sampling, 3, "commit-trim-source");

    let trim = trim_planner
        .begin_sampling(attempt_id("attempt-trim-apply"))
        .expect("begin trim");
    trim_planner
        .observe_source(tool_group(4, 5, "trim", "spine.trim"))
        .expect("trim source");
    complete_fact(
        &trim,
        "execution-trim-apply",
        1,
        SpineFactKind::Trim,
        "trim",
        SpineOperationFact::Trim {
            ticket: crate::TrimTicket::parse(namespace(), ContextEpoch::ZERO, "ticket")
                .expect("ticket"),
            target: crate::StableToolOutputId {
                request: source_ids[0].clone(),
                response: source_ids[1].clone(),
                call_id: "large-output".to_string(),
            },
            validated_edit: crate::TrimEdit::Snipped,
            source_digest: trim_planner.source_snapshot().digest().as_str().to_string(),
        },
    );
    let trim_output = finish_sampling(&mut trim_planner, &trim, 5, "commit-trim-apply");
    assert!(trim_output.plan.cells.iter().any(|cell| {
        matches!(
            cell,
            crate::ContextPlanCell::Source { source_id, labels }
                if *source_id == source_ids[1]
                    && labels == &[ContextLabel::ToolOutput(crate::TrimEdit::Snipped)]
        )
    }));
}

#[test]
fn sampling_planner_builds_trim_fact_from_stable_source_ids() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, trim_config()).expect("planner");
    planner
        .observe_source([user(1, "trim request")])
        .expect("user");
    let source_sampling = planner
        .begin_sampling(attempt_id("attempt-trim-source-ids"))
        .expect("begin source");
    let source_ids = planner
        .observe_source([
            SpineChar::ToolRequest(ToolRequestChar {
                boundary: RawBoundary(2),
                call_id: "large-output".to_string(),
                name: "shell".to_string(),
                arguments: "{}".to_string(),
            }),
            SpineChar::ToolResponse(ToolResponseChar {
                boundary: RawBoundary(3),
                call_id: "large-output".to_string(),
                outcome: ToolOutcome::Succeeded,
                output: "x".repeat(crate::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES + 1),
            }),
        ])
        .expect("large output");
    finish_sampling(&mut planner, &source_sampling, 3, "commit-trim-source-ids");

    let request =
        crate::TrimRequest::parse(r#"{"TRIM_ID":"trim_3","op":"snip"}"#).expect("trim request");
    let operation = planner
        .validated_trim_fact(&request)
        .expect("stable trim fact");
    let SpineOperationFact::Trim {
        ticket,
        target,
        validated_edit,
        source_digest,
    } = operation
    else {
        panic!("validated trim must produce a trim fact");
    };

    assert_eq!(target.request, source_ids[0]);
    assert_eq!(target.response, source_ids[1]);
    assert_eq!(target.call_id, "large-output");
    assert_eq!(ticket.epoch(), ContextEpoch::ZERO);
    assert_eq!(validated_edit, crate::TrimEdit::Snipped);
    assert_eq!(
        source_digest,
        planner.source_snapshot().digest().as_str().to_string()
    );
}

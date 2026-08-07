use crate::AdmissionOrdinal;
use crate::BoundaryId;
use crate::CanonicalReplay;
use crate::ContextEpoch;
use crate::ExecutionId;
use crate::ExecutionOrigin;
use crate::Feature;
use crate::Message;
use crate::MessageRole;
use crate::NativeItemRef;
use crate::RawBoundary;
use crate::RecordDigest;
use crate::ReplayInput;
use crate::SamplingArchiveRecord;
use crate::SamplingAttemptId;
use crate::SamplingCommitId;
use crate::SamplingHandle;
use crate::SamplingPlanner;
use crate::SpineChar;
use crate::SpineCompactBarrierV1;
use crate::SpineConfig;
use crate::SpineFactKind;
use crate::SpineFactReservation;
use crate::SpineOperationFact;
use crate::ThreadNamespace;
use crate::TokenUsageSample;
use crate::ToolOutcome;
use crate::ToolRequestChar;
use crate::ToolResponseChar;
use crate::context_plan::ContextPlanSource;
use crate::planner::SamplingCommitOutput;
use crate::replay::ReplayError;
use pretty_assertions::assert_eq;

fn namespace() -> ThreadNamespace {
    ThreadNamespace::parse("thread-replay").expect("thread")
}

fn config() -> SpineConfig {
    SpineConfig::default()
        .with_features([Feature::Jit, Feature::Trim, Feature::Spawn])
        .expect("config")
}

fn user(boundary: u64, body: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role: MessageRole::User,
        content: body.to_string(),
    })
}

fn tool_group(
    request_boundary: u64,
    response_boundary: u64,
    call_id: &str,
    name: &str,
    arguments: &str,
    output: &str,
) -> [SpineChar; 2] {
    [
        SpineChar::ToolRequest(ToolRequestChar {
            boundary: RawBoundary(request_boundary),
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }),
        SpineChar::ToolResponse(ToolResponseChar {
            boundary: RawBoundary(response_boundary),
            call_id: call_id.to_string(),
            outcome: ToolOutcome::Succeeded,
            output: output.to_string(),
        }),
    ]
}

fn started(planner: &SamplingPlanner, handle: &SamplingHandle) -> SamplingArchiveRecord {
    planner
        .sampling_started_record(handle, RecordDigest::digest(b"prompt"))
        .expect("sampling started")
}

fn started_digest(record: &SamplingArchiveRecord) -> RecordDigest {
    record.record_digest().clone()
}

fn attempt(value: &str) -> SamplingAttemptId {
    SamplingAttemptId::parse(namespace(), value).expect("attempt")
}

fn complete(
    handle: &SamplingHandle,
    execution: &str,
    ordinal: u64,
    kind: SpineFactKind,
    call_id: &str,
    operation: SpineOperationFact,
) {
    complete_in_namespace(
        handle,
        &namespace(),
        execution,
        ordinal,
        kind,
        call_id,
        operation,
    );
}

fn complete_in_namespace(
    handle: &SamplingHandle,
    namespace: &ThreadNamespace,
    execution: &str,
    ordinal: u64,
    kind: SpineFactKind,
    call_id: &str,
    operation: SpineOperationFact,
) {
    let reservation = SpineFactReservation::new(
        handle.attempt(),
        ExecutionId::parse(namespace.clone(), execution).expect("execution"),
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

fn finish(
    planner: &mut SamplingPlanner,
    handle: &SamplingHandle,
    started: &SamplingArchiveRecord,
    post_boundary: u64,
    commit_id: &str,
) -> SamplingCommitOutput {
    finish_at_epoch(
        planner,
        handle,
        started,
        ContextEpoch::ZERO,
        post_boundary,
        commit_id,
    )
}

fn finish_with_input_tokens(
    planner: &mut SamplingPlanner,
    handle: &SamplingHandle,
    started: &SamplingArchiveRecord,
    post_boundary: u64,
    commit_id: &str,
    input_tokens: u64,
) -> SamplingCommitOutput {
    let sealed = handle.seal(ContextEpoch::ZERO).expect("seal");
    let prepared = planner
        .prepare_sampling_with_input_tokens(
            sealed,
            BoundaryId::new(namespace(), ContextEpoch::ZERO, post_boundary),
            SamplingCommitId::parse(namespace(), commit_id).expect("commit"),
            started_digest(started),
            Some(input_tokens),
        )
        .expect("prepare");
    planner.install_prepared(prepared).expect("install")
}

fn finish_at_epoch(
    planner: &mut SamplingPlanner,
    handle: &SamplingHandle,
    started: &SamplingArchiveRecord,
    epoch: ContextEpoch,
    post_boundary: u64,
    commit_id: &str,
) -> SamplingCommitOutput {
    finish_in_namespace(
        planner,
        handle,
        started,
        &namespace(),
        epoch,
        post_boundary,
        commit_id,
    )
}

fn finish_in_namespace(
    planner: &mut SamplingPlanner,
    handle: &SamplingHandle,
    started: &SamplingArchiveRecord,
    namespace: &ThreadNamespace,
    epoch: ContextEpoch,
    post_boundary: u64,
    commit_id: &str,
) -> SamplingCommitOutput {
    let sealed = handle.seal(epoch).expect("seal");
    let prepared = planner
        .prepare_sampling(
            sealed,
            BoundaryId::new(namespace.clone(), epoch, post_boundary),
            SamplingCommitId::parse(namespace.clone(), commit_id).expect("commit"),
            started_digest(started),
        )
        .expect("prepare");
    planner.install_prepared(prepared).expect("install")
}

fn open_trace(
    name: &str,
    arguments: &str,
    output: &str,
) -> (SamplingCommitOutput, Vec<ReplayInput>) {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    let user = user(1, "request");
    planner.observe_source([user.clone()]).expect("user");
    let handle = planner
        .begin_sampling(attempt("attempt-open"))
        .expect("begin");
    let started = started(&planner, &handle);
    let group = tool_group(2, 3, "call-open", name, arguments, output);
    planner.observe_source(group.clone()).expect("tool source");
    complete(
        &handle,
        "execution-open",
        1,
        SpineFactKind::Open,
        "call-open",
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    );
    let output =
        finish_with_input_tokens(&mut planner, &handle, &started, 3, "commit-open", 10_000);
    let input = vec![
        ReplayInput::Source(user),
        ReplayInput::Archive(started),
        ReplayInput::Source(group[0].clone()),
        ReplayInput::Source(group[1].clone()),
        ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(output.record.clone())),
    ];
    (output, input)
}

#[test]
fn jit_replay_equivalence_preserves_tree_context_and_memory() {
    let (open, mut input) = open_trace("spine.open", "{}", "ok");
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    planner.observe_source([user(1, "request")]).expect("user");
    let open_handle = planner
        .begin_sampling(attempt("attempt-open"))
        .expect("begin");
    let open_started = started(&planner, &open_handle);
    let open_group = tool_group(2, 3, "call-open", "spine.open", "{}", "ok");
    planner.observe_source(open_group).expect("open source");
    complete(
        &open_handle,
        "execution-open",
        1,
        SpineFactKind::Open,
        "call-open",
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    );
    finish_with_input_tokens(
        &mut planner,
        &open_handle,
        &open_started,
        3,
        "commit-open",
        10_000,
    );

    let second_user = user(4, "follow-up");
    planner
        .observe_source([second_user.clone()])
        .expect("second user");
    let close_handle = planner
        .begin_sampling(attempt("attempt-close"))
        .expect("begin close");
    let close_started = started(&planner, &close_handle);
    let close_group = tool_group(5, 6, "call-close", "anything", "not-json", "arbitrary");
    planner
        .observe_source(close_group.clone())
        .expect("close source");
    complete(
        &close_handle,
        "execution-close",
        1,
        SpineFactKind::Close,
        "call-close",
        SpineOperationFact::Close {
            memory: "finished".to_string(),
        },
    );
    let close = finish_with_input_tokens(
        &mut planner,
        &close_handle,
        &close_started,
        6,
        "commit-close",
        80_000,
    );

    input.extend([
        ReplayInput::Source(second_user),
        ReplayInput::Archive(close_started),
        ReplayInput::Source(close_group[0].clone()),
        ReplayInput::Source(close_group[1].clone()),
        ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(close.record.clone())),
        ReplayInput::Usage(TokenUsageSample {
            boundary: RawBoundary(7),
            input_tokens: 100,
        }),
    ]);
    let replay = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare(input)
        .expect("replay");

    assert_eq!(replay.projection, close.projection);
    assert_eq!(replay.live_plan, Some(close.plan.clone()));
    assert_eq!(
        replay.live_context,
        close
            .plan
            .resolve(&planner.source_snapshot())
            .expect("resolved JIT plan")
            .cells
            .into_iter()
            .map(|cell| cell.item)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        replay.applied_commits,
        vec![open.record.commit_id, close.record.commit_id]
    );
    assert_eq!(replay.tree.cursor, replay.projection.cursor);
    assert_eq!(planner.current_input_tokens(), Some(10_000));
    assert_eq!(replay.into_runtime().current_input_tokens(), Some(10_000));
}

#[test]
fn replay_function_text_mutation_does_not_change_transition_tree() {
    let (first_jit, first_input) =
        open_trace("spine.open", r#"{"summary":"source"}"#, "carrier text");
    let (mutated_jit, mutated_input) =
        open_trace("not-spine", "malformed text", "different carrier");
    let first = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare(first_input)
        .expect("first replay");
    let mutated = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare(mutated_input)
        .expect("mutated replay");

    assert_eq!(first.projection.nodes, first_jit.projection.nodes);
    assert_eq!(mutated.projection.nodes, mutated_jit.projection.nodes);
    assert_eq!(first.projection.nodes, mutated.projection.nodes);
    assert_eq!(first.projection.cursor, mutated.projection.cursor);
    assert_eq!(first.live_context, mutated.live_context);
}

#[test]
fn replay_zero_fact_commit_does_not_infer_from_tool_text() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    let user = user(1, "request");
    planner.observe_source([user.clone()]).expect("user");
    let handle = planner
        .begin_sampling(attempt("attempt-empty"))
        .expect("begin");
    let started = started(&planner, &handle);
    let group = tool_group(
        2,
        3,
        "call-looking-structural",
        "spine.open",
        r#"{"summary":"must not open"}"#,
        "ok",
    );
    planner.observe_source(group.clone()).expect("source");
    let output = finish(&mut planner, &handle, &started, 3, "commit-empty");
    let replay = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare([
            ReplayInput::Source(user),
            ReplayInput::Archive(started),
            ReplayInput::Source(group[0].clone()),
            ReplayInput::Source(group[1].clone()),
            ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(output.record)),
        ])
        .expect("replay");

    assert_eq!(replay.projection.nodes.len(), 1);
    assert_eq!(replay.projection.cursor, crate::NodeId::root_epoch(1));
}

#[test]
fn replay_duplicate_is_noop_and_conflicting_duplicate_fails_closed() {
    let (_, mut input) = open_trace("tool", "{}", "ok");
    let record = match input.last().expect("commit") {
        ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(record)) => record.clone(),
        ReplayInput::Source(_)
        | ReplayInput::Archive(_)
        | ReplayInput::Compact(_)
        | ReplayInput::Usage(_) => panic!("expected commit"),
    };
    input.push(ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(
        record.clone(),
    )));
    let replay = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare(input.clone())
        .expect("duplicate replay");
    assert_eq!(replay.applied_commits, vec![record.commit_id.clone()]);

    let mut conflicting = record;
    conflicting.executions[0].operation = SpineOperationFact::Open {
        summary: "conflict".to_string(),
    };
    let conflicting = match SamplingArchiveRecord::SamplingCommit(conflicting)
        .finalize_digest()
        .expect("conflicting record")
    {
        SamplingArchiveRecord::SamplingCommit(record) => record,
        SamplingArchiveRecord::SamplingStarted(_) => unreachable!(),
    };
    input.pop();
    input.push(ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(
        conflicting,
    )));
    assert!(matches!(
        CanonicalReplay::new(namespace())
            .expect("replay runtime")
            .prepare(input),
        Err(ReplayError::Archive(
            crate::archive::ArchiveError::ConflictingCommit { .. }
        ))
    ));
}

#[test]
fn replay_compact_barrier_retains_tree_and_projects_only_final_epoch() {
    let (_, mut input) = open_trace("tool", "{}", "ok");
    let first_replacement = vec![RawBoundary(11)];
    input.push(ReplayInput::Compact(
        SpineCompactBarrierV1::new(
            namespace(),
            ContextEpoch::ZERO,
            ContextEpoch::new(1),
            RawBoundary(10),
            first_replacement,
        )
        .expect("first compact"),
    ));
    input.push(ReplayInput::Compact(
        SpineCompactBarrierV1::new(
            namespace(),
            ContextEpoch::new(1),
            ContextEpoch::new(2),
            RawBoundary(20),
            vec![RawBoundary(21)],
        )
        .expect("second compact"),
    ));
    let replay = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare(input)
        .expect("replay");

    assert_eq!(replay.final_epoch, ContextEpoch::new(2));
    assert!(replay.live_plan.is_some());
    assert_eq!(
        replay.live_context,
        vec![crate::ContextItem::Native {
            source: NativeItemRef::Rollout {
                ordinal: RawBoundary(21),
            },
        }]
    );
    assert_eq!(
        replay
            .projection
            .nodes
            .iter()
            .filter(|node| node.kind == crate::NodeKind::RootEpoch)
            .count(),
        3
    );
    assert_eq!(
        replay.projection.nodes[1].status,
        crate::NodeStatus::Compacted
    );
}

#[test]
fn replay_rejects_commit_without_matching_started() {
    let (_, mut input) = open_trace("tool", "{}", "ok");
    input.retain(|item| {
        !matches!(
            item,
            ReplayInput::Archive(SamplingArchiveRecord::SamplingStarted(_))
        )
    });
    assert!(matches!(
        CanonicalReplay::new(namespace())
            .expect("replay runtime")
            .prepare(input),
        Err(ReplayError::CommitWithoutStarted)
    ));
}

#[test]
fn replay_rejects_commit_bound_to_different_started_record() {
    let (_, mut input) = open_trace("tool", "{}", "ok");
    let started_index = input
        .iter()
        .position(|item| {
            matches!(
                item,
                ReplayInput::Archive(SamplingArchiveRecord::SamplingStarted(_))
            )
        })
        .expect("sampling started");
    let mut started = match &input[started_index] {
        ReplayInput::Archive(SamplingArchiveRecord::SamplingStarted(started)) => started.clone(),
        ReplayInput::Source(_)
        | ReplayInput::Archive(_)
        | ReplayInput::Compact(_)
        | ReplayInput::Usage(_) => unreachable!(),
    };
    started.prompt_digest = RecordDigest::digest(b"different prompt");
    input[started_index] = ReplayInput::Archive(
        SamplingArchiveRecord::SamplingStarted(started)
            .finalize_digest()
            .expect("tampered sampling started"),
    );

    assert!(matches!(
        CanonicalReplay::new(namespace())
            .expect("replay runtime")
            .prepare(input),
        Err(ReplayError::SamplingStartedMismatch)
    ));
}

#[test]
fn replay_rejects_fact_binding_to_a_different_tool_group() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    let source = user(1, "request");
    planner
        .observe_source([source.clone()])
        .expect("observe source");
    let handle = planner
        .begin_sampling(attempt("attempt-swapped-binding"))
        .expect("begin");
    let started = started(&planner, &handle);
    let open_group = tool_group(2, 3, "call-open", "spine.open", "{}", "ok");
    let other_group = tool_group(4, 5, "call-other", "shell", "{}", "ok");
    planner
        .observe_source(open_group.clone().into_iter().chain(other_group.clone()))
        .expect("observe tool groups");
    complete(
        &handle,
        "execution-open",
        1,
        SpineFactKind::Open,
        "call-open",
        SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
    );
    let snapshot = planner.source_snapshot();
    let other_start = snapshot
        .source_at_raw_boundary(RawBoundary(4))
        .expect("other request source")
        .id
        .clone();
    let other_end = snapshot
        .source_at_raw_boundary(RawBoundary(5))
        .expect("other response source")
        .id
        .clone();
    let mut commit = finish(&mut planner, &handle, &started, 5, "commit-swapped-binding").record;
    commit.executions[0].source_span.start = other_start;
    commit.executions[0].source_span.end = other_end;
    let commit = SamplingArchiveRecord::SamplingCommit(commit)
        .finalize_digest()
        .expect("recompute commit digest");

    let input = [
        vec![ReplayInput::Source(source), ReplayInput::Archive(started)],
        open_group.map(ReplayInput::Source).to_vec(),
        other_group.map(ReplayInput::Source).to_vec(),
        vec![ReplayInput::Archive(commit)],
    ]
    .concat();
    assert!(matches!(
        CanonicalReplay::new(namespace())
            .expect("replay runtime")
            .prepare(input),
        Err(ReplayError::FactSourceMissing)
    ));
}

#[test]
fn replay_orphan_started_produces_no_transition() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    let source = user(1, "request");
    planner
        .observe_source([source.clone()])
        .expect("observe source");
    let handle = planner
        .begin_sampling(attempt("attempt-orphan"))
        .expect("begin");
    let replay = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare([
            ReplayInput::Source(source),
            ReplayInput::Archive(started(&planner, &handle)),
        ])
        .expect("orphan started replay");

    assert_eq!(replay.applied_commits, Vec::new());
    assert_eq!(replay.projection.nodes.len(), 1);
    assert_eq!(replay.projection.cursor, crate::NodeId::root_epoch(1));
}

#[test]
fn replay_orphan_started_after_commit_previews_pending_source() {
    let (open, mut input) = open_trace("spine.open", "{}", "ok");
    let prepared = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare(input.clone())
        .expect("replay committed prefix");
    let mut planner = prepared.into_planner();
    let pending_user = user(4, "pending request");
    planner
        .observe_source([pending_user.clone()])
        .expect("observe pending source");
    let handle = planner
        .begin_sampling(attempt("attempt-orphan-after-commit"))
        .expect("begin orphan sampling");
    let orphan_started = started(&planner, &handle);
    let expected_plan = planner
        .preview_context_plan()
        .expect("preview context plan");
    let expected_context = expected_plan
        .resolve(&planner.source_snapshot())
        .expect("resolve preview plan")
        .cells
        .into_iter()
        .map(|cell| cell.item)
        .collect::<Vec<_>>();
    input.extend([
        ReplayInput::Source(pending_user),
        ReplayInput::Archive(orphan_started),
    ]);

    let replay = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare(input)
        .expect("replay orphan after committed prefix");

    assert_eq!(replay.applied_commits, vec![open.record.commit_id]);
    assert_eq!(replay.live_plan, Some(expected_plan));
    assert_eq!(replay.live_context, expected_context);
}

#[test]
fn replay_accepts_child_commit_after_parent_orphan_started() {
    let root = namespace();
    let (_, mut input) = open_trace("spine.open", "{}", "ok");
    let prepared = CanonicalReplay::new(root.clone())
        .expect("replay runtime")
        .prepare(input.clone())
        .expect("replay root prefix");
    let mut planner = prepared.into_planner();
    let parent_orphan = planner
        .begin_sampling(attempt("attempt-parent-orphan"))
        .expect("begin parent orphan");
    input.push(ReplayInput::Archive(started(&planner, &parent_orphan)));

    let prepared = CanonicalReplay::new(root.clone())
        .expect("replay runtime")
        .prepare(input.clone())
        .expect("replay parent orphan");
    let mut planner = prepared.into_planner();
    let child = ThreadNamespace::parse("thread-replay-child").expect("child namespace");
    planner
        .continue_in_namespace(child.clone())
        .expect("continue namespace");
    let user_char = user(4, "child request");
    planner
        .observe_source([user_char.clone()])
        .expect("observe child source");
    let handle = planner
        .begin_sampling(SamplingAttemptId::parse(child.clone(), "attempt-child").expect("attempt"))
        .expect("begin child sampling");
    let started = started(&planner, &handle);
    let group = tool_group(5, 6, "call-child", "spine.open", "{}", "ok");
    planner
        .observe_source(group.clone())
        .expect("observe child tool");
    complete_in_namespace(
        &handle,
        &child,
        "execution-child",
        1,
        SpineFactKind::Open,
        "call-child",
        SpineOperationFact::Open {
            summary: "child scope".to_string(),
        },
    );
    let output = finish_in_namespace(
        &mut planner,
        &handle,
        &started,
        &child,
        ContextEpoch::ZERO,
        6,
        "commit-child",
    );
    input.extend([
        ReplayInput::Source(user_char),
        ReplayInput::Archive(started),
        ReplayInput::Source(group[0].clone()),
        ReplayInput::Source(group[1].clone()),
        ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(output.record)),
    ]);

    let replay = CanonicalReplay::new(root.clone())
        .expect("replay runtime")
        .prepare(input)
        .expect("resume child after parent orphan");
    assert_eq!(
        replay
            .applied_commits
            .iter()
            .map(SamplingCommitId::thread)
            .cloned()
            .collect::<Vec<_>>(),
        vec![root, child]
    );
}

#[test]
fn replay_accepts_chained_fork_namespace_commits() {
    let root = namespace();
    let (_, mut input) = open_trace("spine.open", "{}", "ok");
    let prepared = CanonicalReplay::new(root.clone())
        .expect("replay runtime")
        .prepare(input.clone())
        .expect("replay root prefix");
    let mut planner = prepared.into_planner();
    let child = ThreadNamespace::parse("thread-replay-child").expect("child namespace");
    let grandchild =
        ThreadNamespace::parse("thread-replay-grandchild").expect("grandchild namespace");

    for (thread, user_boundary, request_boundary, response_boundary, suffix) in [
        (child.clone(), 4, 5, 6, "child"),
        (grandchild.clone(), 7, 8, 9, "grandchild"),
    ] {
        planner
            .continue_in_namespace(thread.clone())
            .expect("continue namespace");
        let user_char = user(user_boundary, suffix);
        planner
            .observe_source([user_char.clone()])
            .expect("observe fork user");
        let handle = planner
            .begin_sampling(
                SamplingAttemptId::parse(thread.clone(), format!("attempt-{suffix}"))
                    .expect("attempt"),
            )
            .expect("begin fork sampling");
        let started = started(&planner, &handle);
        let group = tool_group(
            request_boundary,
            response_boundary,
            &format!("call-{suffix}"),
            "spine.open",
            "{}",
            "ok",
        );
        planner
            .observe_source(group.clone())
            .expect("observe fork tool");
        complete_in_namespace(
            &handle,
            &thread,
            &format!("execution-{suffix}"),
            1,
            SpineFactKind::Open,
            &format!("call-{suffix}"),
            SpineOperationFact::Open {
                summary: format!("{suffix} scope"),
            },
        );
        let output = finish_in_namespace(
            &mut planner,
            &handle,
            &started,
            &thread,
            ContextEpoch::ZERO,
            response_boundary,
            &format!("commit-{suffix}"),
        );
        input.extend([
            ReplayInput::Source(user_char),
            ReplayInput::Archive(started),
            ReplayInput::Source(group[0].clone()),
            ReplayInput::Source(group[1].clone()),
            ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(output.record)),
        ]);
    }

    let replay = CanonicalReplay::new(root.clone())
        .expect("replay runtime")
        .prepare(input)
        .expect("replay chained forks");
    assert_eq!(
        replay
            .applied_commits
            .iter()
            .map(SamplingCommitId::thread)
            .collect::<Vec<_>>(),
        vec![&root, &child, &grandchild]
    );
    assert_eq!(
        replay.into_planner().source_snapshot().thread(),
        &grandchild
    );
}

#[test]
fn jit_replay_equivalence_survives_compact_and_resumes_sampling() {
    let mut planner =
        SamplingPlanner::new(namespace(), ContextEpoch::ZERO, config()).expect("planner");
    let first_user = user(1, "first");
    planner
        .observe_source([first_user.clone()])
        .expect("first user");
    let first_handle = planner
        .begin_sampling(attempt("attempt-before-compact"))
        .expect("begin");
    let first_started = started(&planner, &first_handle);
    let first_group = tool_group(2, 3, "open-before", "ordinary", "{}", "ok");
    planner
        .observe_source(first_group.clone())
        .expect("first group");
    complete(
        &first_handle,
        "execution-before-compact",
        1,
        SpineFactKind::Open,
        "open-before",
        SpineOperationFact::Open {
            summary: "before compact".to_string(),
        },
    );
    let first = finish(
        &mut planner,
        &first_handle,
        &first_started,
        3,
        "commit-before-compact",
    );
    let mut input = vec![
        ReplayInput::Source(first_user),
        ReplayInput::Archive(first_started),
        ReplayInput::Source(first_group[0].clone()),
        ReplayInput::Source(first_group[1].clone()),
        ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(first.record)),
    ];

    let barrier = SpineCompactBarrierV1::new(
        namespace(),
        ContextEpoch::ZERO,
        ContextEpoch::new(1),
        RawBoundary(10),
        vec![RawBoundary(11)],
    )
    .expect("barrier");
    planner.compact(barrier.clone()).expect("compact");
    input.push(ReplayInput::Compact(barrier));

    let second_user = user(12, "second");
    planner
        .observe_source([second_user.clone()])
        .expect("second user");
    let second_handle = planner
        .begin_sampling(attempt("attempt-after-compact"))
        .expect("begin after compact");
    let second_started = started(&planner, &second_handle);
    let second_group = tool_group(13, 14, "open-after", "mutated", "bad", "carrier");
    planner
        .observe_source(second_group.clone())
        .expect("second group");
    complete(
        &second_handle,
        "execution-after-compact",
        1,
        SpineFactKind::Open,
        "open-after",
        SpineOperationFact::Open {
            summary: "after compact".to_string(),
        },
    );
    let second = finish_at_epoch(
        &mut planner,
        &second_handle,
        &second_started,
        ContextEpoch::new(1),
        14,
        "commit-after-compact",
    );
    input.extend([
        ReplayInput::Source(second_user),
        ReplayInput::Archive(second_started),
        ReplayInput::Source(second_group[0].clone()),
        ReplayInput::Source(second_group[1].clone()),
        ReplayInput::Archive(SamplingArchiveRecord::SamplingCommit(second.record.clone())),
    ]);

    let replay = CanonicalReplay::new(namespace())
        .expect("replay runtime")
        .prepare(input)
        .expect("replay");
    assert_eq!(replay.projection, second.projection);
    assert_eq!(replay.live_plan, Some(second.plan));
    assert_eq!(replay.final_epoch, ContextEpoch::new(1));

    let mut expected = planner;
    let mut resumed = replay.into_planner();
    let follow_up = user(15, "resume");
    expected
        .observe_source([follow_up.clone()])
        .expect("expected follow-up");
    resumed
        .observe_source([follow_up])
        .expect("resumed follow-up");
    let expected_handle = expected
        .begin_sampling(attempt("attempt-resume"))
        .expect("expected begin");
    let resumed_handle = resumed
        .begin_sampling(attempt("attempt-resume"))
        .expect("resumed begin");
    let expected_started = started(&expected, &expected_handle);
    let resumed_started = started(&resumed, &resumed_handle);
    let close_group = tool_group(16, 17, "close-after", "ignored", "{}", "done");
    expected
        .observe_source(close_group.clone())
        .expect("expected close source");
    resumed
        .observe_source(close_group)
        .expect("resumed close source");
    for handle in [&expected_handle, &resumed_handle] {
        complete(
            handle,
            "execution-resume",
            1,
            SpineFactKind::Close,
            "close-after",
            SpineOperationFact::Close {
                memory: "resumed".to_string(),
            },
        );
    }
    let expected = finish_at_epoch(
        &mut expected,
        &expected_handle,
        &expected_started,
        ContextEpoch::new(1),
        17,
        "commit-resume",
    );
    let resumed = finish_at_epoch(
        &mut resumed,
        &resumed_handle,
        &resumed_started,
        ContextEpoch::new(1),
        17,
        "commit-resume",
    );
    assert_eq!(resumed, expected);
}

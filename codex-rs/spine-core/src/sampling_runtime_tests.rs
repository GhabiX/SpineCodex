use crate::ContextEpoch;
use crate::ExecutionOrigin;
use crate::Feature;
use crate::Message;
use crate::MessageRole;
use crate::PlannerError;
use crate::RawBoundary;
use crate::RecordDigest;
use crate::ReplayInput;
use crate::SamplingArchiveRecord;
use crate::SamplingFinish;
use crate::SamplingRuntime;
use crate::SamplingTerminal;
use crate::SpineChar;
use crate::SpineCompactBarrierV1;
use crate::SpineConfig;
use crate::SpineOperationFact;
use crate::ThreadNamespace;
use crate::ToolOutcome;
use crate::ToolRequestChar;
use crate::ToolResponseChar;
use pretty_assertions::assert_eq;

fn namespace() -> ThreadNamespace {
    ThreadNamespace::parse("thread-runtime").expect("namespace")
}

fn config() -> SpineConfig {
    SpineConfig::default()
        .with_features([Feature::Jit, Feature::Trim, Feature::Spawn])
        .expect("runtime config")
}

fn message(boundary: u64, role: MessageRole, content: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role,
        content: content.to_string(),
    })
}

#[test]
fn managed_sampling_owns_identity_admission_and_commit_preparation() {
    let mut runtime =
        SamplingRuntime::new(namespace(), ContextEpoch::ZERO, config()).expect("runtime");
    runtime
        .observe_source([message(1, MessageRole::User, "request")])
        .expect("user source");
    let handle = runtime.begin_sampling().expect("begin");
    let started = runtime
        .sampling_started_record(&handle, RecordDigest::digest(b"prompt"))
        .expect("started");
    runtime.register_execution("open-call").expect("register");
    runtime
        .stage_execution(
            "open-call",
            ExecutionOrigin::Direct {
                call_id: "open-call".to_string(),
            },
            SpineOperationFact::Open {
                summary: "scope".to_string(),
            },
        )
        .expect("stage");
    runtime
        .observe_source([
            SpineChar::ToolRequest(ToolRequestChar {
                boundary: RawBoundary(2),
                call_id: "open-call".to_string(),
                name: "spine.open".to_string(),
                arguments: r#"{"summary":"scope"}"#.to_string(),
            }),
            SpineChar::ToolResponse(ToolResponseChar {
                boundary: RawBoundary(3),
                call_id: "open-call".to_string(),
                outcome: ToolOutcome::Succeeded,
                output: "ok".to_string(),
            }),
        ])
        .expect("tool source");
    runtime.finish_execution("open-call", true).expect("finish");

    let SamplingFinish::Prepared(prepared) = runtime
        .finish_sampling(handle, SamplingTerminal::Completed)
        .expect("finish")
    else {
        panic!("completed sampling must prepare a commit");
    };
    let SamplingArchiveRecord::SamplingStarted(started) = started else {
        panic!("expected sampling-started record");
    };
    assert_eq!(
        prepared.durable_record().started_record_digest,
        started.record_digest
    );
    assert_eq!(prepared.durable_record().executions.len(), 1);
    let installed = runtime.install_prepared(prepared).expect("install");
    assert_eq!(installed.projection.nodes.len(), 2);
}

#[test]
fn successful_execution_without_fact_aborts_sampling() {
    let mut runtime =
        SamplingRuntime::new(namespace(), ContextEpoch::ZERO, config()).expect("runtime");
    let handle = runtime.begin_sampling().expect("begin");
    runtime
        .sampling_started_record(&handle, RecordDigest::digest(b"prompt"))
        .expect("started");
    runtime.register_execution("open-call").expect("register");

    assert!(matches!(
        runtime.finish_execution("open-call", true),
        Err(PlannerError::SuccessfulExecutionMissingFact(key)) if key == "open-call"
    ));
    assert!(matches!(
        runtime.finish_sampling(handle, SamplingTerminal::Completed),
        Err(PlannerError::Sampling(
            crate::SamplingError::TransactionAborted
        ))
    ));
}

#[test]
fn failed_sampling_commits_only_after_the_attempt_observes_a_delta() {
    let mut runtime =
        SamplingRuntime::new(namespace(), ContextEpoch::ZERO, config()).expect("runtime");
    runtime
        .observe_source([message(1, MessageRole::User, "request")])
        .expect("user source");

    let handle = runtime.begin_sampling().expect("begin empty attempt");
    runtime
        .sampling_started_record(&handle, RecordDigest::digest(b"prompt-1"))
        .expect("started");
    assert!(matches!(
        runtime
            .finish_sampling(handle, SamplingTerminal::Failed)
            .expect("finish empty attempt"),
        SamplingFinish::OrphanedStart
    ));

    let handle = runtime.begin_sampling().expect("begin effectful attempt");
    runtime
        .sampling_started_record(&handle, RecordDigest::digest(b"prompt-2"))
        .expect("started");
    runtime
        .observe_source([message(2, MessageRole::Assistant, "partial")])
        .expect("partial source");
    let SamplingFinish::Prepared(prepared) = runtime
        .finish_sampling(handle, SamplingTerminal::Failed)
        .expect("finish failed delta")
    else {
        panic!("effectful failed sampling must prepare a commit");
    };
    runtime.install_prepared(prepared).expect("install");
}

#[test]
fn compact_absorbs_pending_source_without_a_synthetic_sampling() {
    let mut runtime =
        SamplingRuntime::new(namespace(), ContextEpoch::ZERO, config()).expect("runtime");
    let source = message(1, MessageRole::User, "pending before compact");
    runtime
        .observe_source([source.clone()])
        .expect("pending source");
    let barrier = SpineCompactBarrierV1::new(
        namespace(),
        ContextEpoch::ZERO,
        ContextEpoch::new(1),
        RawBoundary(2),
        vec![RawBoundary(3)],
    )
    .expect("barrier");
    let projection = runtime.compact(barrier.clone()).expect("compact");

    let replay = crate::CanonicalReplay::new(namespace())
        .expect("replay")
        .with_runtime_config(config())
        .expect("runtime config")
        .prepare([ReplayInput::Source(source), ReplayInput::Compact(barrier)])
        .expect("replay pending source compact");
    assert_eq!(replay.projection, projection);
    assert!(replay.applied_commits.is_empty());
}

#[test]
fn continuing_in_the_same_namespace_preserves_the_pre_boundary_chain() {
    let mut runtime =
        SamplingRuntime::new(namespace(), ContextEpoch::ZERO, config()).expect("runtime");
    runtime
        .observe_source([message(1, MessageRole::User, "request")])
        .expect("user source");
    let first = runtime.begin_sampling().expect("begin first sampling");
    runtime
        .sampling_started_record(&first, RecordDigest::digest(b"prompt-1"))
        .expect("first started");
    runtime
        .observe_source([message(2, MessageRole::Assistant, "first answer")])
        .expect("first answer");
    let SamplingFinish::Prepared(first) = runtime
        .finish_sampling(first, SamplingTerminal::Completed)
        .expect("finish first")
    else {
        panic!("completed sampling must prepare a commit");
    };
    let first_pre_boundary = first.durable_record().pre_boundary.clone();
    runtime.install_prepared(first).expect("install first");

    runtime
        .continue_in_namespace(namespace())
        .expect("same namespace continuation");
    let second = runtime.begin_sampling().expect("begin second sampling");
    runtime
        .sampling_started_record(&second, RecordDigest::digest(b"prompt-2"))
        .expect("second started");
    runtime
        .observe_source([message(3, MessageRole::Assistant, "second answer")])
        .expect("second answer");
    let SamplingFinish::Prepared(second) = runtime
        .finish_sampling(second, SamplingTerminal::Completed)
        .expect("finish second")
    else {
        panic!("completed sampling must prepare a commit");
    };

    assert_eq!(
        second.durable_record().previous_pre_boundary.as_ref(),
        Some(&first_pre_boundary)
    );
}

#[test]
fn sampling_start_is_unique_and_missing_start_aborts_the_attempt() {
    let mut runtime =
        SamplingRuntime::new(namespace(), ContextEpoch::ZERO, config()).expect("runtime");
    runtime
        .observe_source([message(1, MessageRole::User, "request")])
        .expect("user source");

    let handle = runtime.begin_sampling().expect("begin duplicate start");
    runtime
        .sampling_started_record(&handle, RecordDigest::digest(b"prompt"))
        .expect("first started record");
    assert!(matches!(
        runtime.sampling_started_record(&handle, RecordDigest::digest(b"other")),
        Err(crate::PlannerError::SamplingAlreadyStarted)
    ));
    runtime
        .abort_sampling(&handle)
        .expect("abort duplicate start");

    let handle = runtime.begin_sampling().expect("begin missing start");
    assert!(matches!(
        runtime.finish_sampling(handle, SamplingTerminal::Completed),
        Err(crate::PlannerError::SamplingNotStarted)
    ));
    runtime
        .begin_sampling()
        .expect("missing start must leave runtime idle");
}

#[test]
fn prepared_sampling_blocks_mutation_until_install() {
    let mut runtime =
        SamplingRuntime::new(namespace(), ContextEpoch::ZERO, config()).expect("runtime");
    runtime
        .observe_source([message(1, MessageRole::User, "request")])
        .expect("user source");
    let handle = runtime.begin_sampling().expect("begin");
    runtime
        .sampling_started_record(&handle, RecordDigest::digest(b"prompt"))
        .expect("started");
    let SamplingFinish::Prepared(prepared) = runtime
        .finish_sampling(handle, SamplingTerminal::Completed)
        .expect("finish")
    else {
        panic!("completed sampling must prepare a commit");
    };

    assert!(matches!(
        runtime.observe_source([message(2, MessageRole::Assistant, "racing")]),
        Err(crate::PlannerError::SamplingCommitPendingInstall)
    ));
    assert!(matches!(
        runtime.begin_sampling(),
        Err(crate::PlannerError::SamplingCommitPendingInstall)
    ));
    runtime.install_prepared(prepared).expect("install");
    runtime
        .observe_source([message(2, MessageRole::Assistant, "after install")])
        .expect("source after install");
}

use crate::archive::ArchiveError;
use crate::archive::CommittedSpineExecution;
use crate::archive::MAX_FACTS_PER_SAMPLING;
use crate::archive::RecordDigest;
use crate::archive::SAMPLING_COMMIT_SCHEMA;
use crate::archive::SAMPLING_STARTED_SCHEMA;
use crate::archive::SamplingArchiveRecord;
use crate::archive::SamplingCommit;
use crate::archive::SamplingStarted;
use crate::archive::SourceSpan;
use crate::executed_fact::ExecutionOrigin;
use crate::executed_fact::SpineOperationFact;
use crate::identity::AdmissionOrdinal;
use crate::identity::BoundaryId;
use crate::identity::ContextEpoch;
use crate::identity::ExecutionId;
use crate::identity::SamplingAttemptId;
use crate::identity::SamplingCommitId;
use crate::identity::SourceCellId;
use crate::identity::ThreadNamespace;
use pretty_assertions::assert_eq;

fn namespace() -> ThreadNamespace {
    ThreadNamespace::parse("thread-test").expect("valid namespace")
}

fn digest(value: char) -> RecordDigest {
    RecordDigest::parse(value.to_string().repeat(64)).expect("valid digest")
}

fn attempt_id(thread: &ThreadNamespace) -> SamplingAttemptId {
    SamplingAttemptId::parse(thread.clone(), "019fa8b4-a3a7-7743-a58b-539eb7ae6793")
        .expect("valid attempt ID")
}

fn commit_id(thread: &ThreadNamespace) -> SamplingCommitId {
    SamplingCommitId::parse(thread.clone(), "019fa8b4-a3a7-7743-a58b-539eb7ae6794")
        .expect("valid commit ID")
}

fn execution(
    thread: &ThreadNamespace,
    epoch: ContextEpoch,
    ordinal: u64,
    summary: String,
) -> CommittedSpineExecution {
    let execution_id = ExecutionId::parse(thread.clone(), format!("execution-{ordinal}"))
        .expect("valid execution ID");
    CommittedSpineExecution {
        source_span: SourceSpan {
            start: SourceCellId::new(thread.clone(), epoch, ordinal.saturating_mul(2)),
            end: SourceCellId::new(
                thread.clone(),
                epoch,
                ordinal.saturating_mul(2).saturating_add(1),
            ),
        },
        execution_id,
        ordinal: AdmissionOrdinal::new(ordinal),
        origin: ExecutionOrigin::Direct {
            call_id: format!("call-{ordinal}"),
        },
        operation: SpineOperationFact::Open { summary },
    }
}

fn commit(summary: &str) -> SamplingArchiveRecord {
    let thread = namespace();
    let epoch = ContextEpoch::new(2);
    SamplingArchiveRecord::SamplingCommit(SamplingCommit {
        schema: SAMPLING_COMMIT_SCHEMA.to_string(),
        attempt_id: attempt_id(&thread),
        started_record_digest: digest('b'),
        commit_id: commit_id(&thread),
        epoch,
        previous_pre_boundary: Some(BoundaryId::new(thread.clone(), epoch, 7)),
        pre_boundary: BoundaryId::new(thread.clone(), epoch, 8),
        post_boundary: BoundaryId::new(thread.clone(), epoch, 9),
        previous_commit_id: None,
        input_tokens: None,
        executions: vec![execution(&thread, epoch, 0, summary.to_string())],
        source_digest: digest('a'),
        record_digest: digest('0'),
    })
    .finalize_digest()
    .expect("finalize commit digest")
}

fn records() -> Vec<SamplingArchiveRecord> {
    let thread = namespace();
    let epoch = ContextEpoch::new(2);
    vec![
        SamplingArchiveRecord::SamplingStarted(SamplingStarted {
            schema: SAMPLING_STARTED_SCHEMA.to_string(),
            attempt_id: attempt_id(&thread),
            epoch,
            pre_boundary: BoundaryId::new(thread.clone(), epoch, 8),
            previous_commit_id: None,
            prompt_digest: digest('a'),
            source_digest: digest('1'),
            record_digest: digest('b'),
        })
        .finalize_digest()
        .expect("finalize started digest"),
        commit("scope"),
    ]
}

fn commit_record(record: SamplingArchiveRecord) -> SamplingCommit {
    match record {
        SamplingArchiveRecord::SamplingCommit(record) => record,
        SamplingArchiveRecord::SamplingStarted(_) => unreachable!(),
    }
}

fn large_trim_execution(
    thread: &ThreadNamespace,
    epoch: ContextEpoch,
    ordinal: u64,
) -> CommittedSpineExecution {
    let execution_id = ExecutionId::parse(thread.clone(), format!("large-execution-{ordinal}"))
        .expect("valid execution ID");
    let request = SourceCellId::new(thread.clone(), epoch, ordinal.saturating_mul(2));
    let response = SourceCellId::new(
        thread.clone(),
        epoch,
        ordinal.saturating_mul(2).saturating_add(1),
    );
    CommittedSpineExecution {
        execution_id,
        ordinal: AdmissionOrdinal::new(ordinal),
        origin: ExecutionOrigin::Direct {
            call_id: format!("large-call-{ordinal}"),
        },
        source_span: SourceSpan {
            start: request.clone(),
            end: response.clone(),
        },
        operation: SpineOperationFact::Trim {
            ticket: crate::TrimTicket::parse(thread.clone(), epoch, format!("ticket-{ordinal}"))
                .expect("ticket"),
            target: crate::StableToolOutputId {
                request,
                response,
                call_id: format!("large-call-{ordinal}"),
            },
            validated_edit: crate::TrimEdit::Sliced("x".repeat(crate::MAX_MEMORY_BYTES)),
            source_digest: "source".to_string(),
        },
    }
}

#[test]
fn sampling_archive_round_trips_started_and_self_contained_commit() {
    for record in records() {
        let encoded = record.encode().expect("encode archive record");
        assert_eq!(
            SamplingArchiveRecord::decode(&encoded).expect("decode archive record"),
            record
        );
    }

    let mut record = commit_record(commit("ignored"));
    record.executions[0].operation = SpineOperationFact::Next {
        closed_memory: "closed memory".to_string(),
        next_summary: "next summary".to_string(),
    };
    let record = SamplingArchiveRecord::SamplingCommit(record)
        .finalize_digest()
        .expect("finalize complete execution");
    let encoded = record.encode().expect("encode complete execution");
    assert_eq!(
        SamplingArchiveRecord::decode(&encoded).expect("decode complete execution"),
        record
    );

    let legacy = commit("legacy");
    let encoded = legacy.encode().expect("encode legacy commit");
    assert!(
        !String::from_utf8_lossy(&encoded).contains("input_tokens"),
        "an absent pressure sample must preserve the legacy canonical encoding"
    );
    assert_eq!(
        SamplingArchiveRecord::decode(&encoded).expect("decode legacy commit"),
        legacy
    );

    let mut pressure = commit_record(commit("pressure"));
    pressure.input_tokens = Some(42_000);
    let pressure = SamplingArchiveRecord::SamplingCommit(pressure)
        .finalize_digest()
        .expect("finalize pressure commit");
    let encoded = pressure.encode().expect("encode pressure commit");
    assert_eq!(
        SamplingArchiveRecord::decode(&encoded).expect("decode pressure commit"),
        pressure
    );
}

#[test]
fn sampling_archive_rejects_unknown_record_tags_and_schemas() {
    let encoded = commit("scope").encode().expect("encode archive record");
    let unknown_tag = String::from_utf8(encoded.clone())
        .expect("JSON")
        .replace("\"sampling_commit\"", "\"sampling_commit_v2\"");
    assert!(matches!(
        SamplingArchiveRecord::decode(unknown_tag.as_bytes()),
        Err(ArchiveError::Deserialize(_))
    ));

    let unknown_schema = String::from_utf8(encoded)
        .expect("JSON")
        .replace(SAMPLING_COMMIT_SCHEMA, "spine.sampling.commit.v2");
    assert!(matches!(
        SamplingArchiveRecord::decode(unknown_schema.as_bytes()),
        Err(ArchiveError::UnsupportedSchema { .. })
    ));
}

#[test]
fn sampling_archive_enforces_identity_fact_and_record_bounds() {
    assert!(RecordDigest::parse("ABC").is_err());
    assert!(serde_json::from_str::<ThreadNamespace>(r#""invalid namespace""#).is_err());
    let parent = namespace();
    let child = parent
        .for_fork("thread-child")
        .expect("unique fork namespace");
    assert_ne!(parent, child);
    assert!(parent.for_fork(parent.as_str()).is_err());

    let mut record = commit_record(commit("scope"));
    record.executions = (0..=MAX_FACTS_PER_SAMPLING)
        .map(|ordinal| execution(&parent, record.epoch, ordinal as u64, "scope".to_string()))
        .collect();
    assert!(matches!(
        SamplingArchiveRecord::SamplingCommit(record).validate(),
        Err(ArchiveError::TooManyFacts { .. })
    ));

    let mut record = commit_record(commit("scope"));
    record.executions = (0..9)
        .map(|ordinal| large_trim_execution(&parent, record.epoch, ordinal))
        .collect();
    assert!(matches!(
        SamplingArchiveRecord::SamplingCommit(record).validate(),
        Err(ArchiveError::RecordTooLarge { .. })
    ));

    let mut record = commit_record(commit("scope"));
    record.executions[0].execution_id =
        ExecutionId::parse(child, "child-execution").expect("child execution");
    assert!(matches!(
        SamplingArchiveRecord::SamplingCommit(record).validate(),
        Err(ArchiveError::IdentityScopeMismatch)
    ));
}

#[test]
fn sampling_archive_rejects_tampered_execution_or_source_span() {
    let mut record = commit_record(commit("scope"));
    record.executions[0].operation = SpineOperationFact::Open {
        summary: "tampered".to_string(),
    };
    assert!(matches!(
        SamplingArchiveRecord::SamplingCommit(record).validate(),
        Err(ArchiveError::DigestMismatch { .. })
    ));

    let mut record = commit_record(commit("scope"));
    record.executions[0].source_span.start = SourceCellId::new(namespace(), record.epoch, 2);
    record.executions[0].source_span.end = SourceCellId::new(namespace(), record.epoch, 1);
    assert!(matches!(
        SamplingArchiveRecord::SamplingCommit(record).validate(),
        Err(ArchiveError::InvalidFactSourceBinding)
    ));
}

#[test]
fn sampling_archive_rejects_unordered_or_conflicting_executions() {
    let mut record = commit_record(commit("scope"));
    record.executions = vec![
        execution(&namespace(), record.epoch, 2, "later".to_string()),
        execution(&namespace(), record.epoch, 1, "earlier".to_string()),
    ];
    assert!(matches!(
        SamplingArchiveRecord::SamplingCommit(record).validate(),
        Err(ArchiveError::InvalidFactOrder)
    ));

    let mut record = commit_record(commit("scope"));
    record.executions = vec![
        execution(&namespace(), record.epoch, 1, "first".to_string()),
        execution(&namespace(), record.epoch, 2, "second".to_string()),
    ];
    assert!(matches!(
        SamplingArchiveRecord::SamplingCommit(record).validate(),
        Err(ArchiveError::ConflictingFacts)
    ));
}

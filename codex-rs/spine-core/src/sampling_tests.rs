use super::*;
use crate::ExecutionOrigin;
use crate::SpineOperationFact;
use crate::ThreadNamespace;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::thread;

fn namespace() -> ThreadNamespace {
    ThreadNamespace::parse("thread-a").expect("valid namespace")
}

fn attempt(value: &str) -> SamplingAttempt {
    let thread = namespace();
    let epoch = ContextEpoch::new(4);
    SamplingAttempt::new(
        SamplingAttemptId::parse(thread.clone(), value).expect("valid attempt"),
        epoch,
        BoundaryId::new(thread, epoch, 10),
    )
    .expect("valid sampling attempt")
}

fn reservation_for(
    attempt: &SamplingAttempt,
    value: &str,
    ordinal: u64,
    kind: SpineFactKind,
) -> SpineFactReservation {
    SpineFactReservation::new(
        attempt,
        ExecutionId::parse(namespace(), value).expect("valid execution"),
        AdmissionOrdinal::new(ordinal),
        kind,
    )
    .expect("valid reservation")
}

fn fact(reservation: &SpineFactReservation) -> ExecutedSpineFact {
    let operation = match reservation.kind() {
        SpineFactKind::Open => SpineOperationFact::Open {
            summary: "scope".to_string(),
        },
        SpineFactKind::Close => SpineOperationFact::Close {
            memory: "memory".to_string(),
        },
        SpineFactKind::Next => SpineOperationFact::Next {
            closed_memory: "memory".to_string(),
            next_summary: "next".to_string(),
        },
        SpineFactKind::Spawn => unreachable!("spawn helper requires terminal results"),
        SpineFactKind::Deferred => unreachable!("test reservations always bind a fact kind"),
        SpineFactKind::Trim => unreachable!("trim helper requires a stable ticket"),
    };
    ExecutedSpineFact {
        execution_id: reservation.execution_id().clone(),
        ordinal: reservation.ordinal(),
        origin: ExecutionOrigin::Direct {
            call_id: format!("call-{}", reservation.ordinal().value()),
        },
        operation,
    }
}

#[test]
fn sampling_transaction_orders_concurrent_completion_by_admission() {
    let attempt = attempt("attempt-order");
    let open = reservation_for(&attempt, "exec-open", 2, SpineFactKind::Open);
    let handle = attempt.begin();
    let sink = handle.fact_sink();
    let first_permit = sink.reserve(open.clone()).expect("reserve open");

    let sink_for_thread = Arc::clone(&sink);
    let worker = thread::spawn(move || {
        sink_for_thread
            .complete(first_permit, fact(&open))
            .expect("complete open")
    });
    worker.join().expect("worker must finish");

    let sealed = handle.seal(ContextEpoch::new(4)).expect("seal transaction");
    assert_eq!(
        sealed
            .facts()
            .iter()
            .map(|fact| fact.ordinal)
            .collect::<Vec<_>>(),
        vec![AdmissionOrdinal::new(2)]
    );
}

#[test]
fn sampling_transaction_reverses_parallel_trim_completion_without_reordering() {
    use crate::SourceCellId;
    use crate::StableToolOutputId;
    use crate::TrimEdit;
    use crate::TrimTicket;

    fn trim_fact(reservation: &SpineFactReservation, source: u64) -> ExecutedSpineFact {
        let thread = namespace();
        let epoch = ContextEpoch::new(4);
        ExecutedSpineFact {
            execution_id: reservation.execution_id().clone(),
            ordinal: reservation.ordinal(),
            origin: ExecutionOrigin::Direct {
                call_id: format!("call-{}", reservation.ordinal().value()),
            },
            operation: SpineOperationFact::Trim {
                ticket: TrimTicket::parse(thread.clone(), epoch, format!("ticket-{source}"))
                    .expect("valid ticket"),
                target: StableToolOutputId {
                    request: SourceCellId::new(thread.clone(), epoch, source),
                    response: SourceCellId::new(thread, epoch, source + 1),
                    call_id: format!("tool-{source}"),
                },
                validated_edit: TrimEdit::Snipped,
                source_digest: format!("digest-{source}"),
            },
        }
    }

    let attempt = attempt("attempt-parallel");
    let first = reservation_for(&attempt, "exec-first", 1, SpineFactKind::Trim);
    let second = reservation_for(&attempt, "exec-second", 2, SpineFactKind::Trim);
    let handle = attempt.begin();
    let sink = handle.fact_sink();
    let first_permit = sink.reserve(first.clone()).expect("reserve first");
    let second_permit = sink.reserve(second.clone()).expect("reserve second");
    let second_sink = Arc::clone(&sink);
    let second_worker = thread::spawn(move || {
        second_sink
            .complete(second_permit, trim_fact(&second, 20))
            .expect("complete second")
    });
    second_worker.join().expect("second worker");
    sink.complete(first_permit, trim_fact(&first, 10))
        .expect("complete first");

    let sealed = handle.seal(ContextEpoch::new(4)).expect("seal transaction");
    assert_eq!(
        sealed
            .facts()
            .iter()
            .map(|fact| fact.ordinal)
            .collect::<Vec<_>>(),
        vec![AdmissionOrdinal::new(1), AdmissionOrdinal::new(2)]
    );
}

#[test]
fn sampling_transaction_rejects_conflicting_trim_targets() {
    use crate::SourceCellId;
    use crate::StableToolOutputId;
    use crate::TrimEdit;
    use crate::TrimTicket;

    let attempt = attempt("attempt-trim-conflict");
    let first = reservation_for(&attempt, "exec-first", 1, SpineFactKind::Trim);
    let second = reservation_for(&attempt, "exec-second", 2, SpineFactKind::Trim);
    let handle = attempt.begin();
    let sink = handle.fact_sink();
    let first_permit = sink.reserve(first.clone()).expect("reserve first");
    let second_permit = sink.reserve(second.clone()).expect("reserve second");
    let thread = namespace();
    let epoch = ContextEpoch::new(4);
    let target = StableToolOutputId {
        request: SourceCellId::new(thread.clone(), epoch, 10),
        response: SourceCellId::new(thread.clone(), epoch, 11),
        call_id: "same-output".to_string(),
    };
    let trim_fact = |reservation: &SpineFactReservation, ticket: &str| ExecutedSpineFact {
        execution_id: reservation.execution_id().clone(),
        ordinal: reservation.ordinal(),
        origin: ExecutionOrigin::Direct {
            call_id: format!("call-{}", reservation.ordinal().value()),
        },
        operation: SpineOperationFact::Trim {
            ticket: TrimTicket::parse(thread.clone(), epoch, ticket).expect("valid ticket"),
            target: target.clone(),
            validated_edit: TrimEdit::Snipped,
            source_digest: "digest".to_string(),
        },
    };

    sink.complete(first_permit, trim_fact(&first, "ticket-first"))
        .expect("complete first");
    assert_eq!(
        sink.complete(second_permit, trim_fact(&second, "ticket-second")),
        Err(SamplingError::ConflictingTrimTarget)
    );
    assert_eq!(
        handle.seal(ContextEpoch::new(4)),
        Err(SamplingError::TransactionFailed)
    );
}

#[test]
fn sampling_transaction_abort_permit_releases_exclusive_admission() {
    let attempt = attempt("attempt-cancel");
    let open = reservation_for(&attempt, "exec-open", 1, SpineFactKind::Open);
    let close = reservation_for(&attempt, "exec-close", 2, SpineFactKind::Close);
    let handle = attempt.begin();
    let sink = handle.fact_sink();
    let permit = sink.reserve(open).expect("reserve open");
    sink.abort_permit(permit).expect("abort reservation");

    let close_permit = sink.reserve(close.clone()).expect("reserve close");
    sink.complete(close_permit, fact(&close))
        .expect("complete close");
    assert_eq!(
        handle
            .seal(ContextEpoch::new(4))
            .expect("seal transaction")
            .facts(),
        &[fact(&close)]
    );
}

#[test]
fn sampling_transaction_rejects_reservation_from_another_attempt() {
    let first_attempt = attempt("attempt-owner");
    let foreign_attempt = attempt("attempt-foreign");
    let foreign = reservation_for(&foreign_attempt, "exec-foreign", 1, SpineFactKind::Open);
    let handle = first_attempt.begin();

    assert_eq!(
        handle.fact_sink().reserve(foreign),
        Err(SamplingError::ReservationAttemptMismatch)
    );
}

#[test]
fn sampling_transaction_rejects_exclusive_duplicates_and_mismatches() {
    let attempt = attempt("attempt-conflict");
    let open = reservation_for(&attempt, "exec-open", 1, SpineFactKind::Open);
    let duplicate_ordinal = reservation_for(&attempt, "exec-other", 1, SpineFactKind::Trim);
    let handle = attempt.begin();
    let sink = handle.fact_sink();
    sink.reserve(open).expect("reserve open");
    assert!(matches!(
        sink.reserve(duplicate_ordinal),
        Err(SamplingError::DuplicateOrdinal(_))
    ));
    assert_eq!(
        handle.seal(ContextEpoch::new(4)),
        Err(SamplingError::TransactionFailed)
    );
}

#[test]
fn sampling_transaction_poisoned_by_exclusive_conflict_or_mismatched_completion() {
    let exclusive_attempt = attempt("attempt-exclusive-conflict");
    let open = reservation_for(&exclusive_attempt, "exec-open", 1, SpineFactKind::Open);
    let close = reservation_for(&exclusive_attempt, "exec-close", 2, SpineFactKind::Close);
    let handle = exclusive_attempt.begin();
    let sink = handle.fact_sink();
    sink.reserve(open).expect("reserve open");
    assert!(matches!(
        sink.reserve(close),
        Err(SamplingError::ExclusiveConflict { .. })
    ));
    assert_eq!(
        handle.seal(ContextEpoch::new(4)),
        Err(SamplingError::TransactionFailed)
    );

    let attempt = attempt("attempt-mismatch");
    let open = reservation_for(&attempt, "exec-open", 1, SpineFactKind::Open);
    let handle = attempt.begin();
    let sink = handle.fact_sink();
    let open_permit = sink.reserve(open.clone()).expect("reserve open");
    let mut mismatched = fact(&open);
    mismatched.ordinal = AdmissionOrdinal::new(9);
    assert_eq!(
        sink.complete(open_permit, mismatched),
        Err(SamplingError::FactReservationMismatch("ordinal"))
    );
    assert_eq!(
        handle.seal(ContextEpoch::new(4)),
        Err(SamplingError::TransactionFailed)
    );
}

#[test]
fn sampling_transaction_retry_attempts_are_isolated() {
    let first_attempt = attempt("attempt-first");
    let first_reservation = reservation_for(&first_attempt, "exec-shared", 1, SpineFactKind::Open);
    let first = first_attempt.begin();
    let first_sink = first.fact_sink();
    let permit = first_sink
        .reserve(first_reservation.clone())
        .expect("reserve first attempt");
    first_sink
        .complete(permit, fact(&first_reservation))
        .expect("complete first attempt");
    first.abort().expect("abort failed attempt");
    assert_eq!(
        first.seal(ContextEpoch::new(4)),
        Err(SamplingError::TransactionAborted)
    );

    let second_attempt = attempt("attempt-second");
    let second_reservation =
        reservation_for(&second_attempt, "exec-shared", 1, SpineFactKind::Open);
    let second = second_attempt.begin();
    let second_sink = second.fact_sink();
    let permit = second_sink
        .reserve(second_reservation.clone())
        .expect("reserve retry attempt");
    second_sink
        .complete(permit, fact(&second_reservation))
        .expect("complete retry attempt");
    assert_eq!(
        second
            .seal(ContextEpoch::new(4))
            .expect("seal retry")
            .facts(),
        &[fact(&second_reservation)]
    );
}

#[test]
fn sampling_transaction_seal_and_abort_have_explicit_terminal_states() {
    let empty_attempt = attempt("attempt-empty");
    let late_reservation = reservation_for(&empty_attempt, "exec-late", 1, SpineFactKind::Trim);
    let empty = empty_attempt.begin();
    let empty_sink = empty.fact_sink();
    assert!(
        empty
            .seal(ContextEpoch::new(4))
            .expect("zero-fact commit")
            .facts()
            .is_empty()
    );
    assert_eq!(
        empty.seal(ContextEpoch::new(4)),
        Err(SamplingError::TransactionSealed)
    );
    assert_eq!(empty.abort(), Err(SamplingError::TransactionSealed));
    assert_eq!(
        empty_sink.reserve(late_reservation),
        Err(SamplingError::TransactionSealed)
    );

    let aborted_attempt = attempt("attempt-aborted");
    let late_reservation = reservation_for(&aborted_attempt, "exec-late", 1, SpineFactKind::Trim);
    let aborted = aborted_attempt.begin();
    let aborted_sink = aborted.fact_sink();
    aborted.abort().expect("first abort");
    aborted.abort().expect("abort is idempotent");
    assert_eq!(
        aborted_sink.reserve(late_reservation),
        Err(SamplingError::TransactionAborted)
    );
}

#[test]
fn sampling_transaction_pending_permit_and_stale_epoch_abort_without_facts() {
    let pending_attempt = attempt("attempt-pending");
    let pending_reservation =
        reservation_for(&pending_attempt, "exec-pending", 7, SpineFactKind::Open);
    let late_reservation = reservation_for(&pending_attempt, "exec-late", 8, SpineFactKind::Trim);
    let pending = pending_attempt.begin();
    let sink = pending.fact_sink();
    sink.reserve(pending_reservation).expect("reserve pending");
    assert_eq!(
        pending.seal(ContextEpoch::new(4)),
        Err(SamplingError::PendingPermits(vec![AdmissionOrdinal::new(
            7
        )]))
    );
    assert_eq!(
        sink.reserve(late_reservation),
        Err(SamplingError::TransactionAborted)
    );

    let stale_attempt = attempt("attempt-stale");
    let reservation = reservation_for(&stale_attempt, "exec-stale", 1, SpineFactKind::Open);
    let stale = stale_attempt.begin();
    let stale_sink = stale.fact_sink();
    let permit = stale_sink
        .reserve(reservation.clone())
        .expect("reserve stale");
    stale_sink
        .complete(permit, fact(&reservation))
        .expect("complete stale");
    assert_eq!(
        stale.seal(ContextEpoch::new(5)),
        Err(SamplingError::StaleEpoch {
            expected: ContextEpoch::new(4),
            actual: ContextEpoch::new(5),
        })
    );
    assert_eq!(
        stale.seal(ContextEpoch::new(4)),
        Err(SamplingError::TransactionAborted)
    );
}

use crate::agent::AgentControl;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use pretty_assertions::assert_eq;

fn control_with_limit(max_threads: usize) -> AgentControl {
    let control = AgentControl::default();
    control.agent_execution_limiter.initialize(max_threads);
    control
}

#[test]
fn execution_guards_count_active_v2_subagent_turns() {
    let control = control_with_limit(/*max_threads*/ 1);
    // Child role configs cannot replace the root-derived session limit.
    control
        .agent_execution_limiter
        .initialize(/*max_threads*/ 2);
    let source = SessionSource::SubAgent(SubAgentSource::Other("worker".to_string()));

    control
        .ensure_execution_capacity(MultiAgentVersion::V2, &source)
        .expect("first active turn should fit");
    let first = control
        .execution_guard(ThreadId::new(), MultiAgentVersion::V2, &source)
        .expect("v2 subagent execution should be counted");
    let Err(err) = control.ensure_execution_capacity(MultiAgentVersion::V2, &source) else {
        panic!("second active turn should exceed the derived non-root cap");
    };
    let CodexErr::AgentLimitReached { max_threads } = err else {
        panic!("expected AgentLimitReached");
    };
    assert_eq!(max_threads, 1);

    drop(first);
    control
        .ensure_execution_capacity(MultiAgentVersion::V2, &source)
        .expect("capacity should be released when the running task drops");
}

#[test]
fn execution_guards_ignore_root_and_v1_turns() {
    let control = control_with_limit(/*max_threads*/ 0);

    assert!(
        control
            .execution_guard(ThreadId::new(), MultiAgentVersion::V2, &SessionSource::Cli)
            .is_none()
    );
    assert!(
        control
            .execution_guard(
                ThreadId::new(),
                MultiAgentVersion::V1,
                &SessionSource::SubAgent(SubAgentSource::Other("worker".to_string())),
            )
            .is_none()
    );
}

#[test]
fn batch_execution_reservation_is_atomic_and_claimed_by_thread() {
    let control = control_with_limit(/*max_threads*/ 2);
    let source = SessionSource::SubAgent(SubAgentSource::Other("worker".to_string()));

    let mut reservations = control
        .reserve_execution_slots(/*count*/ 2)
        .expect("reserve whole batch");
    assert!(
        control.reserve_execution_slots(/*count*/ 1).is_err(),
        "a full batch reservation must exclude partial admission"
    );

    drop(reservations.pop());
    let thread_id = codex_protocol::ThreadId::new();
    reservations
        .pop()
        .expect("second reservation")
        .commit(thread_id);
    let claimed = control
        .execution_guard(thread_id, MultiAgentVersion::V2, &source)
        .expect("reserved child turn claims its slot");
    let ordinary = control
        .execution_guard(
            codex_protocol::ThreadId::new(),
            MultiAgentVersion::V2,
            &source,
        )
        .expect("released reservation permits one ordinary turn");
    assert!(control.reserve_execution_slots(/*count*/ 1).is_err());

    drop(ordinary);
    drop(claimed);
    assert!(control.reserve_execution_slots(/*count*/ 2).is_ok());
}

#[test]
fn prepared_reservation_claims_for_non_v2_surface() {
    let control = control_with_limit(/*max_threads*/ 1);
    let source = SessionSource::SubAgent(SubAgentSource::Other("spine".to_string()));
    let thread_id = ThreadId::new();
    let reservation = control
        .reserve_execution_slots(/*count*/ 1)
        .expect("reserve prepared child")
        .pop()
        .expect("one reservation");
    reservation.commit(thread_id);

    let guard = control
        .execution_guard(thread_id, MultiAgentVersion::V1, &source)
        .expect("prepared child must claim its slot independent of surface version");
    assert!(
        control.reserve_execution_slots(/*count*/ 1).is_err(),
        "claimed prepared child must consume the active slot"
    );
    drop(guard);
    assert!(control.reserve_execution_slots(/*count*/ 1).is_ok());
}

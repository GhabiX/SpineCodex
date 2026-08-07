use crate::agent::AgentControl;
use codex_protocol::AgentPath;
use codex_protocol::error::CodexErrorDetails;
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
        .execution_guard(MultiAgentVersion::V2, &source)
        .expect("v2 subagent execution should be counted");
    let Err(err) = control.ensure_execution_capacity(MultiAgentVersion::V2, &source) else {
        panic!("second active turn should exceed the derived non-root cap");
    };
    let CodexErrorDetails::AgentLimitReached { max_threads } = err.details() else {
        panic!("expected AgentLimitReached");
    };
    assert_eq!(*max_threads, 1);

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
            .execution_guard(MultiAgentVersion::V2, &SessionSource::Cli)
            .is_none()
    );
    assert!(
        control
            .execution_guard(
                MultiAgentVersion::V1,
                &SessionSource::SubAgent(SubAgentSource::Other("worker".to_string())),
            )
            .is_none()
    );
}

#[test]
fn spine_batch_reservations_are_atomic_and_claimed_by_agent_path() {
    let control = AgentControl::default();
    control.spine_spawn_limiter.initialize(/*max_threads*/ 2);
    let mut reservations = control
        .reserve_spine_spawn_slots(/*count*/ 2)
        .expect("entire batch should reserve");
    let Err(err) = control.reserve_spine_spawn_slots(/*count*/ 1) else {
        panic!("capacity occupied by a prepared batch must not be overcommitted");
    };
    let CodexErrorDetails::AgentLimitReached { max_threads } = err.details() else {
        panic!("expected AgentLimitReached");
    };
    assert_eq!(*max_threads, 2);

    let agent_path = AgentPath::try_from("/root/spawn_0").expect("agent path");
    reservations.pop().expect("reservation").commit(&agent_path);
    drop(reservations);
    let source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: codex_protocol::ThreadId::new(),
        depth: 1,
        agent_path: Some(agent_path),
        agent_nickname: None,
        agent_role: None,
    });
    let guard = control
        .execution_guard(MultiAgentVersion::Disabled, &source)
        .expect("reserved Spine child should claim dedicated capacity");
    drop(guard);

    assert_eq!(
        control
            .reserve_spine_spawn_slots(/*count*/ 2)
            .expect("claimed and dropped capacity should be reusable")
            .len(),
        2
    );
}

#[test]
fn spine_capacity_is_independent_from_native_v2_capacity() {
    let control = control_with_limit(/*max_threads*/ 0);
    control.spine_spawn_limiter.initialize(/*max_threads*/ 1);
    assert_eq!(
        control
            .reserve_spine_spawn_slots(/*count*/ 1)
            .expect("dedicated Spine capacity should remain available")
            .len(),
        1
    );

    let source = SessionSource::SubAgent(SubAgentSource::Other("worker".to_string()));
    let Err(err) = control.ensure_execution_capacity(MultiAgentVersion::V2, &source) else {
        panic!("native V2 capacity should retain its own zero limit");
    };
    let CodexErrorDetails::AgentLimitReached { max_threads } = err.details() else {
        panic!("expected AgentLimitReached");
    };
    assert_eq!(*max_threads, 0);
}

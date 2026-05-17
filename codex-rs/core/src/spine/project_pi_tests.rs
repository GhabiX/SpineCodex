use super::*;
use crate::spine::state::NodeRecord;
use crate::spine::state::NodeStatus;
use crate::spine::state::SpineState;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn mem(compact_id: &str, node_id: NodeId, start: u64, end: u64) -> ProjectMemInstall {
    ProjectMemInstall::new(compact_id, node_id, start, end).expect("project mem")
}

fn set_raw_start(state: &mut SpineState, node_id: &[u32], raw_start: u64) {
    state
        .set_raw_start_ordinal(&id(node_id), raw_start)
        .expect("set raw start");
}

#[test]
fn project_pi_simple_live_leaf_keeps_raw_tail() {
    let state = SpineState::new();
    let result = project_pi(ProjectInput::new(5, state)).expect("project pi");

    assert_eq!(result.pi, vec![Segment::raw(0, 5).expect("raw")]);
    assert_eq!(result.visible_nodes, vec![id(&[1]), id(&[1, 1])]);
    assert_eq!(result.live_boundaries, vec![0]);
    assert!(result.admitted_mem_ids.is_empty());
    assert!(result.rejected_mem_ids.is_empty());
    assert_eq!(result.cost, None);
}

#[test]
fn project_pi_admits_visible_mem_and_raw_tail() {
    let mut state = SpineState::new();
    state.next("first done").expect("next");
    set_raw_start(&mut state, &[1, 2], 5);
    let mut input = ProjectInput::new(8, state);
    input.mem_installs.push(mem("mem-1", id(&[1, 1]), 0, 5));

    let result = project_pi(input).expect("project pi");

    assert_eq!(
        result.pi,
        vec![Segment::mem("mem-1"), Segment::raw(5, 8).expect("raw")]
    );
    assert_eq!(result.admitted_mem_ids, vec!["mem-1".to_string()]);
    assert_eq!(result.live_boundaries, vec![0, 5]);
}

#[test]
fn project_pi_root_archive_subsumes_descendant_mem() {
    let mut state = SpineState::new();
    state.open().expect("open");
    state
        .reset_root_epoch("context compacted", 21)
        .expect("root archive");
    let mut input = ProjectInput::new(25, state);
    input.mem_installs.push(mem("root-mem", id(&[1]), 0, 21));
    input
        .mem_installs
        .push(mem("child-mem", id(&[1, 1]), 0, 10));

    let result = project_pi(input).expect("project pi");

    assert_eq!(
        result.pi,
        vec![Segment::mem("root-mem"), Segment::raw(21, 25).expect("raw")]
    );
    assert_eq!(result.admitted_mem_ids, vec!["root-mem".to_string()]);
    assert_eq!(
        result.rejected_mem_ids,
        vec![RejectedProjectMem {
            compact_id: "child-mem".to_string(),
            reason: ProjectMemRejectionReason::CoveredByAncestor,
        }]
    );
}

#[test]
fn project_pi_rejects_live_start_inside_mem() {
    let mut state = SpineState::new();
    state.next("first done").expect("next");
    set_raw_start(&mut state, &[1, 2], 3);
    let mut input = ProjectInput::new(8, state);
    input.mem_installs.push(mem("mem-1", id(&[1, 1]), 0, 5));

    let error = project_pi(input).expect_err("live start inside mem");

    assert!(matches!(
        error,
        ProjectError::ProjectLiveStartInsideMem { .. }
    ));
}

#[test]
fn projection_rejects_mem_past_restricted_raw_len() {
    let mut state = SpineState::new();
    state.next("first done").expect("next");
    set_raw_start(&mut state, &[1, 2], 5);
    let mut input = ProjectInput::new(4, state);
    input.mem_installs.push(mem("mem-1", id(&[1, 1]), 0, 5));

    let error = project_pi(input).expect_err("future mem must fail");

    assert!(matches!(
        error,
        ProjectError::ProjectForkEvidenceIncomplete { .. }
    ));
}

#[test]
fn project_pi_rejects_missing_required_mem_install() {
    let state = SpineState::new();
    let mut input = ProjectInput::new(3, state);
    input.required_mem_ids.insert("missing-mem".to_string());

    let error = project_pi(input).expect_err("missing mem install");

    assert_eq!(
        error,
        ProjectError::ProjectMissingMemInstall {
            compact_id: "missing-mem".to_string(),
        }
    );
}

#[test]
fn project_pi_rejects_missing_memory_body() {
    let mut state = SpineState::new();
    state.next("first done").expect("next");
    set_raw_start(&mut state, &[1, 2], 5);
    let mut install = mem("mem-1", id(&[1, 1]), 0, 5);
    install.body_verified = false;
    let mut input = ProjectInput::new(8, state);
    input.mem_installs.push(install);

    let error = project_pi(input).expect_err("missing body");

    assert_eq!(
        error,
        ProjectError::ProjectMissingMemoryBody {
            compact_id: "mem-1".to_string(),
        }
    );
}

#[test]
fn project_pi_inserts_notes_without_advancing_raw() {
    let state = SpineState::new();
    let mut input = ProjectInput::new(5, state);
    input.notes.push(ProjectNote::new(2, "handoff"));

    let result = project_pi(input).expect("project pi");

    assert_eq!(
        result.pi,
        vec![
            Segment::raw(0, 2).expect("raw"),
            Segment::note("handoff"),
            Segment::raw(2, 5).expect("raw"),
        ]
    );
}

#[test]
fn project_pi_cost_under_budget_accepts() {
    let mut state = SpineState::new();
    state.next("first done").expect("next");
    set_raw_start(&mut state, &[1, 2], 5);
    let mut input = ProjectInput::new(8, state);
    input.mem_installs.push(mem("mem-1", id(&[1, 1]), 0, 5));
    input.resource_profile = Some(ProjectResourceProfile {
        raw_tokens_per_item: 2,
        mem_tokens: BTreeMap::from([("mem-1".to_string(), 4)]),
        note_tokens: BTreeMap::new(),
        budget_tokens: Some(10),
    });

    let result = project_pi(input).expect("project pi");

    assert_eq!(result.cost, Some(ProjectCost { total_tokens: 10 }));
}

#[test]
fn project_pi_cost_over_budget_rejects() {
    let state = SpineState::new();
    let mut input = ProjectInput::new(6, state);
    input.resource_profile = Some(ProjectResourceProfile {
        raw_tokens_per_item: 3,
        mem_tokens: BTreeMap::new(),
        note_tokens: BTreeMap::new(),
        budget_tokens: Some(17),
    });

    let error = project_pi(input).expect_err("budget exceeded");

    assert_eq!(
        error,
        ProjectError::ProjectBudgetExceeded {
            cost: 18,
            budget: 17,
        }
    );
}

#[test]
fn project_pi_stop_boundary_fails_closed() {
    let state = SpineState::new();
    let mut input = ProjectInput::new(1, state);
    input.stop_reason = Some("native compact".to_string());

    let error = project_pi(input).expect_err("stop boundary");

    assert_eq!(
        error,
        ProjectError::ProjectStopBoundary {
            reason: "native compact".to_string(),
        }
    );
}

#[test]
fn project_pi_detects_missing_mutable_raw_start() {
    let state = SpineState::from_records(
        id(&[1, 1]),
        vec![
            NodeRecord {
                node_id: id(&[1]),
                parent_id: None,
                raw_start_ordinal: Some(0),
                status: NodeStatus::Opened,
                summary: None,
            },
            NodeRecord {
                node_id: id(&[1, 1]),
                parent_id: Some(id(&[1])),
                raw_start_ordinal: None,
                status: NodeStatus::Live,
                summary: None,
            },
        ],
    )
    .expect("state");

    let error = project_pi(ProjectInput::new(3, state)).expect_err("missing raw start");

    assert!(matches!(
        error,
        ProjectError::ProjectForkEvidenceIncomplete { .. }
    ));
}

use super::*;
use crate::spine::ids::NodeId;
use crate::spine::project_pi::ProjectMemRejectionReason;
use crate::spine::segment::Segment;
use crate::spine::state::SpineState;
use crate::spine::store::SpineOperation;
use pretty_assertions::assert_eq;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn installed_span(
    compact_id: &str,
    node_id: NodeId,
    op: SpineOperation,
    cut_ordinal: u64,
    fold_end_ordinal: u64,
) -> InstalledCompactSpan {
    InstalledCompactSpan {
        compact_id: compact_id.to_string(),
        node_id,
        op,
        cut_ordinal,
        fold_end_ordinal,
        replacement_history_len: 0,
        message_hash: format!("sha1:{compact_id}"),
    }
}

#[test]
fn candidate_mem_cover_adds_candidate_to_runtime_artifacts() {
    let spans = vec![installed_span(
        "compact-a",
        id(&[1]),
        SpineOperation::Next,
        1,
        4,
    )];
    let candidate = CandidateMem::new(
        "compact-b",
        id(&[2]),
        SpineOperation::Next,
        RawSpan { start: 5, end: 8 },
    );

    let cover = plan_candidate_mem_cover(9, &spans, &candidate).expect("cover");

    assert_eq!(
        cover.artifacts.get("compact-a"),
        Some(&RawSpan { start: 1, end: 4 })
    );
    assert_eq!(
        cover.artifacts.get("compact-b"),
        Some(&RawSpan { start: 5, end: 8 })
    );
    assert_eq!(
        cover.pi,
        vec![
            Segment::Raw(RawSpan { start: 0, end: 1 }),
            Segment::Mem {
                compact_id: "compact-a".to_string()
            },
            Segment::Raw(RawSpan { start: 4, end: 5 }),
            Segment::Mem {
                compact_id: "compact-b".to_string()
            },
            Segment::Raw(RawSpan { start: 8, end: 9 }),
        ]
    );
}

#[test]
fn candidate_mem_cover_rejects_candidate_past_raw_len() {
    let candidate = CandidateMem::new(
        "compact-b",
        id(&[2]),
        SpineOperation::Next,
        RawSpan { start: 5, end: 8 },
    );

    let err = plan_candidate_mem_cover(7, &[], &candidate).expect_err("past raw_len");

    assert!(
        err.to_string()
            .contains("compact-b node 2 op Next span [5,8)"),
        "error should identify the candidate: {err}"
    );
}

#[test]
fn candidate_mem_plan_cover_only_rejects_live_boundary_inside_candidate() {
    let candidate = CandidateMem::anonymous("root-archive", RawSpan { start: 0, end: 8 });

    let err = plan_candidate_mem(
        10,
        &[],
        &candidate,
        CandidateMemPlanMode::CoverOnly {
            live_boundaries: &[4],
            admission: CandidateAdmissionPolicy::MustAdmit,
        },
    )
    .expect_err("live boundary inside candidate");

    assert!(
        err.to_string()
            .contains("live-boundary validation failed for root-archive"),
        "error should name root candidate: {err}"
    );
}

#[test]
fn candidate_mem_plan_projection_backed_allows_node_not_visible_rejection() {
    let state = SpineState::new();
    let candidate = CandidateMem::new(
        "hidden-candidate",
        id(&[9]),
        SpineOperation::Next,
        RawSpan { start: 0, end: 4 },
    );

    let plan = plan_candidate_mem(
        6,
        &[],
        &candidate,
        CandidateMemPlanMode::ProjectionBacked {
            state: &state,
            admission: CandidateAdmissionPolicy::AdmitOrRejectNodeNotVisible,
        },
    )
    .expect("node-not-visible rejection is allowed for suffix");

    assert!(!plan.admitted_candidate);
    assert_eq!(
        plan.rejected_candidate_reason,
        Some(ProjectMemRejectionReason::NodeNotVisible)
    );
}

#[test]
fn candidate_mem_plan_projection_backed_requires_candidate_when_policy_must_admit() {
    let state = SpineState::new();
    let candidate = CandidateMem::new(
        "hidden-candidate",
        id(&[9]),
        SpineOperation::Next,
        RawSpan { start: 0, end: 4 },
    );

    assert!(
        plan_candidate_mem(
            6,
            &[],
            &candidate,
            CandidateMemPlanMode::ProjectionBacked {
                state: &state,
                admission: CandidateAdmissionPolicy::MustAdmit,
            },
        )
        .is_err()
    );
}

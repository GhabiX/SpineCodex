use super::*;
use crate::spine::ids::NodeId;
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
    }
}

#[test]
fn candidate_mem_cover_adds_candidate_to_runtime_artifacts() {
    let spans = vec![installed_span(
        "compact-a",
        id(&[1]),
        SpineOperation::Close,
        1,
        4,
    )];
    let candidate = CandidateMem::new(
        "compact-b",
        id(&[2]),
        SpineOperation::Close,
        RawSpan { start: 5, end: 8 },
    );

    let (pi, artifacts) = plan_candidate_mem_cover(9, &spans, &candidate).expect("cover");

    assert_eq!(
        artifacts.get("compact-a"),
        Some(&RawSpan { start: 1, end: 4 })
    );
    assert_eq!(
        artifacts.get("compact-b"),
        Some(&RawSpan { start: 5, end: 8 })
    );
    assert_eq!(
        pi,
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
        SpineOperation::Close,
        RawSpan { start: 5, end: 8 },
    );

    let err = plan_candidate_mem_cover(7, &[], &candidate).expect_err("past raw_len");

    assert!(
        err.to_string()
            .contains("compact-b node 2 op Close span [5,8)"),
        "error should identify the candidate: {err}"
    );
}

#[test]
fn candidate_mem_plan_cover_only_rejects_live_boundary_inside_candidate() {
    let candidate = CandidateMem::new(
        "root-archive",
        id(&[1]),
        SpineOperation::Archive,
        RawSpan { start: 0, end: 8 },
    );

    let err = plan_candidate_mem(
        10,
        &[],
        &candidate,
        CandidateMemPlanMode::CoverOnly {
            live_boundaries: &[4],
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
fn candidate_mem_plan_projection_backed_rejects_node_not_visible_candidate() {
    let state = SpineState::new();
    let candidate = CandidateMem::new(
        "hidden-candidate",
        id(&[9]),
        SpineOperation::Close,
        RawSpan { start: 0, end: 4 },
    );

    let err = plan_candidate_mem(
        6,
        &[],
        &candidate,
        CandidateMemPlanMode::ProjectionBacked { state: &state },
    )
    .expect_err("node-not-visible candidate must fail closed");

    assert!(
        err.to_string().contains("NodeNotVisible"),
        "error should report Project(Pi) rejection reason: {err}"
    );
}

use super::*;
use crate::spine::compact::SpineCompactBoundary;
use crate::spine::compact::render_spine_memory_item;
use crate::spine::ids::NodeId;
use crate::spine::project_pi::ProjectInput;
use crate::spine::project_pi::ProjectMemInstall;
use crate::spine::segment::RawSpan;
use crate::spine::segment::Segment;
use crate::spine::segment::SegmentArtifacts;
use crate::spine::state::SpineState;
use crate::spine::store::InstalledCompactSpan;
use crate::spine::store::SpineOperation;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

fn id(segments: &[u32]) -> NodeId {
    NodeId::from_segments(segments.to_vec())
}

fn text_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
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
fn runtime_debug_checks_project_pi_accepts_clean_projection() {
    let result = audit_project_pi(
        RuntimeDebugBoundary::StartupResume,
        ProjectInput::new(3, SpineState::new()),
        "rollout.jsonl",
    )
    .expect("debug project audit should pass");

    assert_eq!(result.pi, vec![Segment::raw(0, 3).expect("raw")]);
}

#[test]
fn runtime_debug_checks_reject_invalid_pi_with_invariant_name() {
    let pi = vec![Segment::raw(1, 3).expect("raw")];
    let artifacts = SegmentArtifacts::new();

    let error = audit_segment_cover(
        RuntimeDebugBoundary::StartupResume,
        &pi,
        &artifacts,
        &[],
        "rollout.jsonl",
    )
    .expect_err("gap must fail debug audit");

    assert_eq!(error.invariant(), INV_SEGMENT_COVER);
    assert!(error.to_string().contains("rollout.jsonl"));
}

#[test]
fn runtime_debug_checks_reject_project_live_start_inside_mem() {
    let mut state = SpineState::new();
    state.next("first done").expect("next");
    state
        .set_raw_start_ordinal(&id(&[1, 2]), 3)
        .expect("set raw start");
    let mut input = ProjectInput::new(8, state);
    input
        .mem_installs
        .push(ProjectMemInstall::new("mem-1", id(&[1, 1]), 0, 5).expect("mem"));

    let error = audit_project_pi(
        RuntimeDebugBoundary::RollbackProjection,
        input,
        "projected sidecar",
    )
    .expect_err("live start inside Mem must fail debug audit");

    assert_eq!(error.invariant(), INV_LIVE_BOUNDARY);
}

#[test]
fn runtime_debug_checks_reject_meminstall_missing_body_with_invariant_name() {
    let mut state = SpineState::new();
    state.next("first done").expect("next");
    state
        .set_raw_start_ordinal(&id(&[1, 2]), 5)
        .expect("set raw start");
    let mut install = ProjectMemInstall::new("mem-1", id(&[1, 1]), 0, 5).expect("mem install");
    install.body_verified = false;
    let mut input = ProjectInput::new(8, state);
    input.mem_installs.push(install);

    let error = audit_project_pi(
        RuntimeDebugBoundary::AfterCompactInstall,
        input,
        "mem install audit",
    )
    .expect_err("missing body must fail debug audit");

    assert_eq!(error.invariant(), INV_MEM_EVIDENCE);
}

#[test]
fn runtime_debug_checks_reject_compact_boundary_inside_existing_mem() {
    let history = vec![
        render_spine_memory_item(&id(&[1, 1]), SpineOperation::Next, "done", "facts"),
        text_item("tail"),
    ];
    let spans = vec![installed_span(
        "compact-1",
        id(&[1, 1]),
        SpineOperation::Next,
        0,
        5,
    )];
    let boundary = SpineCompactBoundary {
        op: SpineOperation::Next,
        node_id: id(&[1, 2]),
        scope_node_id: Some(id(&[1])),
        cut_ordinal: 1,
        fold_end_ordinal: 5,
        transition_summary: "next".to_string(),
        compact_instruction: None,
    };

    let error = audit_compact_plan_boundaries(&history, &spans, &boundary, "compact plan")
        .expect_err("boundary inside Mem must fail");

    assert_eq!(error.invariant(), INV_RAW_BOUNDARY);
}

#[test]
fn runtime_debug_checks_reject_checkpoint_with_unmappable_required_boundary() {
    let memory_item = render_spine_memory_item(&id(&[1]), SpineOperation::Archive, "root", "facts");
    let replacement_history = vec![memory_item];
    let spans = vec![installed_span(
        "compact-root",
        id(&[1]),
        SpineOperation::Archive,
        0,
        2,
    )];

    let error = audit_compact_checkpoint(
        RuntimeDebugBoundary::BeforeCheckpointInstall,
        &replacement_history,
        &spans,
        &[1],
        "compact.index.jsonl",
    )
    .expect_err("interior required boundary must fail");

    assert_eq!(error.invariant(), INV_RENDER_BRIDGE);
    assert!(error.to_string().contains("compact.index.jsonl"));
}

#[test]
fn runtime_debug_checks_render_pi_equivalence_names_bridge_invariant() {
    let actual = vec![text_item("actual")];
    let expected = vec![text_item("expected")];

    let error = audit_render_pi_equivalence(&actual, &expected, "render bridge")
        .expect_err("mismatched render should fail");

    assert_eq!(error.invariant(), INV_RENDER_BRIDGE);
}

#[test]
fn runtime_debug_checks_projection_epoch_mismatch_names_projection_invariant() {
    let state = SpineState::new();
    let projection = crate::spine::projection_epoch::projection_epoch_metadata(
        "rollout.jsonl",
        &[],
        &state,
        3,
        &Default::default(),
        &Default::default(),
    )
    .expect("epoch");

    let error = audit_projection_epoch(
        RuntimeDebugBoundary::ForkSeed,
        2,
        &projection,
        "fork sidecar",
    )
    .expect_err("raw len mismatch must fail");

    assert_eq!(error.invariant(), INV_PROJECTION);
}

#[test]
fn runtime_debug_checks_feature_off_boundary_is_inert_without_runtime() {
    audit_feature_off_boundary(false, "feature off session").expect("no runtime is inert");
}

#[test]
fn runtime_debug_checks_feature_off_boundary_rejects_runtime() {
    let error = audit_feature_off_boundary(true, "feature off session")
        .expect_err("feature off runtime must fail");

    assert_eq!(error.invariant(), INV_FEATURE_OFF);
}

#[test]
fn runtime_debug_checks_segment_live_boundary_uses_invariant_name() {
    let pi = vec![Segment::mem("mem-1")];
    let artifacts = SegmentArtifacts::from([("mem-1".to_string(), RawSpan { start: 0, end: 5 })]);

    let error = audit_segment_cover(
        RuntimeDebugBoundary::StartupResume,
        &pi,
        &artifacts,
        &[3],
        "live boundary",
    )
    .expect_err("live boundary inside Mem must fail");

    assert_eq!(error.invariant(), INV_LIVE_BOUNDARY);
}

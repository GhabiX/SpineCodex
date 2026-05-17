use super::*;
use pretty_assertions::assert_eq;

fn artifact(compact_id: &str, start: u64, end: u64) -> (String, RawSpan) {
    (
        compact_id.to_string(),
        RawSpan::new(start, end).expect("valid span"),
    )
}

fn artifacts(items: Vec<(String, RawSpan)>) -> SegmentArtifacts {
    items.into_iter().collect()
}

fn raw(start: u64, end: u64) -> Segment {
    Segment::raw(start, end).expect("valid raw")
}

#[test]
fn segment_valid_cover_accepts_ordered_raw_and_mem() {
    let artifacts = artifacts(vec![artifact("m1", 2, 20)]);
    let segments = vec![raw(0, 2), Segment::mem("m1"), raw(20, 25)];

    assert_eq!(validate_cover(&segments, &artifacts), Ok(25));
}

#[test]
fn segment_rejects_gap() {
    let artifacts = SegmentArtifacts::new();
    let segments = vec![raw(0, 2), raw(4, 5)];

    assert!(matches!(
        validate_cover(&segments, &artifacts),
        Err(SegmentError::CoverGap {
            index: 1,
            expected_start: 2,
            actual_start: 4,
        })
    ));
}

#[test]
fn segment_rejects_overlap() {
    let artifacts = SegmentArtifacts::new();
    let segments = vec![raw(0, 5), raw(3, 8)];

    assert!(matches!(
        validate_cover(&segments, &artifacts),
        Err(SegmentError::CoverOverlap {
            index: 1,
            expected_start: 5,
            actual_start: 3,
        })
    ));
}

#[test]
fn segment_maps_legal_boundaries_round_trip() {
    let artifacts = artifacts(vec![artifact("m1", 2, 20)]);
    let segments = vec![raw(0, 2), Segment::mem("m1"), raw(20, 25)];

    for raw_boundary in [0, 2, 20, 23, 25] {
        let position = f_boundary(&segments, &artifacts, raw_boundary)
            .expect("f succeeds")
            .expect("boundary maps");
        assert_eq!(
            g_boundary(&segments, &artifacts, position),
            Ok(Some(raw_boundary))
        );
    }
    assert_eq!(f_boundary(&segments, &artifacts, 10), Ok(None));
}

#[test]
fn segment_rejects_live_boundary_inside_mem() {
    let artifacts = artifacts(vec![artifact("root-492-788", 492, 788)]);
    let segments = vec![raw(0, 492), Segment::mem("root-492-788")];

    assert!(matches!(
        validate_future_live_boundaries(&segments, &artifacts, &[652]),
        Err(SegmentError::LiveStartInsideMem {
            raw_start: 652,
            compact_id,
            span: RawSpan {
                start: 492,
                end: 788,
            },
        }) if compact_id == "root-492-788"
    ));
}

#[test]
fn project_pi_accepts_live_start_at_mem_endpoint() {
    let artifacts = artifacts(vec![artifact("m1", 2, 20)]);
    let segments = vec![raw(0, 2), Segment::mem("m1"), raw(20, 25)];

    assert_eq!(
        validate_future_live_boundaries(&segments, &artifacts, &[2, 20]),
        Ok(())
    );
}

#[test]
fn project_pi_rejects_live_start_inside_mem() {
    let artifacts = artifacts(vec![artifact("m1", 2, 20)]);
    let segments = vec![raw(0, 2), Segment::mem("m1"), raw(20, 25)];

    assert!(matches!(
        validate_future_live_boundaries(&segments, &artifacts, &[10]),
        Err(SegmentError::LiveStartInsideMem { .. })
    ));
}

#[test]
fn suffix_fold_rejects_closure_crossing_live_boundary() {
    let artifacts = artifacts(vec![artifact("closed-tool-call", 2, 12)]);
    let segments = vec![raw(0, 2), Segment::mem("closed-tool-call"), raw(12, 14)];

    assert!(matches!(
        validate_future_live_boundaries(&segments, &artifacts, &[10]),
        Err(SegmentError::LiveStartInsideMem { .. })
    ));
}

#[test]
fn segment_note_has_empty_span() {
    let artifacts = SegmentArtifacts::new();

    assert_eq!(span(&Segment::note("handoff"), &artifacts), Ok(None));
}

#[test]
fn segment_rejects_note_as_raw_width() {
    let artifacts = SegmentArtifacts::new();
    let segments = vec![Segment::note("handoff"), raw(0, 1)];

    assert_eq!(
        g_boundary(
            &segments,
            &artifacts,
            SegmentPosition {
                segment_index: 0,
                offset: 1,
            },
        ),
        Ok(None)
    );
}

#[test]
fn replace_exact_cover_replaces_complete_span() {
    let mut artifacts = artifacts(vec![artifact("child", 5, 20)]);
    let segments = vec![raw(0, 5), Segment::mem("child"), raw(20, 25)];
    artifacts.insert(
        "parent".to_string(),
        RawSpan::new(0, 20).expect("valid span"),
    );

    let replaced = replace_exact_cover(
        &segments,
        &artifacts,
        RawSpan::new(0, 20).expect("valid span"),
        Segment::mem("parent"),
    )
    .expect("replace cover");

    assert_eq!(replaced, vec![Segment::mem("parent"), raw(20, 25)]);
}

#[test]
fn replace_exact_cover_rejects_cutting_through_span() {
    let artifacts = SegmentArtifacts::new();
    let segments = vec![raw(0, 10)];

    assert!(matches!(
        replace_exact_cover(
            &segments,
            &artifacts,
            RawSpan::new(2, 8).expect("valid span"),
            raw(2, 8),
        ),
        Err(SegmentError::ReplacementCutsSpan {
            replacement: RawSpan { start: 2, end: 8 },
            existing: RawSpan { start: 0, end: 10 },
        })
    ));
}

#[test]
fn canonical_cover_subsumes_child_mems() {
    let artifacts = artifacts(vec![
        artifact("child-22-93", 22, 93),
        artifact("child-191-196", 191, 196),
        artifact("root-22-788", 22, 788),
    ]);

    let cover = canonical_cover(
        789,
        ["child-22-93", "child-191-196", "root-22-788"],
        &artifacts,
    )
    .expect("canonical cover");

    assert_eq!(
        cover,
        vec![raw(0, 22), Segment::mem("root-22-788"), raw(788, 789)]
    );
}

#[test]
fn python_segment_harness_cases_match_rust_kernel() {
    struct Case {
        name: &'static str,
        segments: Vec<Segment>,
        artifacts: SegmentArtifacts,
        live_starts: Vec<u64>,
        should_pass: bool,
    }

    let cases = vec![
        Case {
            name: "cut_652_discontinuous_prior_mem",
            segments: vec![
                raw(0, 22),
                Segment::mem("child-22-93"),
                Segment::mem("child-191-196"),
                raw(652, 653),
            ],
            artifacts: artifacts(vec![
                artifact("child-22-93", 22, 93),
                artifact("child-191-196", 191, 196),
            ]),
            live_starts: vec![652],
            should_pass: false,
        },
        Case {
            name: "root_archive_subsumes_prior_mems",
            segments: vec![raw(0, 22), Segment::mem("root-22-788"), raw(788, 789)],
            artifacts: artifacts(vec![artifact("root-22-788", 22, 788)]),
            live_starts: vec![788],
            should_pass: true,
        },
        Case {
            name: "future_live_start_inside_mem",
            segments: vec![raw(0, 492), Segment::mem("root-492-788")],
            artifacts: artifacts(vec![artifact("root-492-788", 492, 788)]),
            live_starts: vec![652],
            should_pass: false,
        },
        Case {
            name: "parent_and_child_mem_visible_together",
            segments: vec![
                raw(0, 2),
                Segment::mem("parent-2-20"),
                Segment::mem("child-5-20"),
            ],
            artifacts: artifacts(vec![
                artifact("child-5-20", 5, 20),
                artifact("parent-2-20", 2, 20),
            ]),
            live_starts: Vec::new(),
            should_pass: false,
        },
        Case {
            name: "close_parent_after_child_compact",
            segments: vec![raw(0, 2), Segment::mem("parent-2-20"), raw(20, 21)],
            artifacts: artifacts(vec![artifact("parent-2-20", 2, 20)]),
            live_starts: vec![20],
            should_pass: true,
        },
        Case {
            name: "rollback_drops_non_surviving_artifact",
            segments: vec![raw(0, 20), Segment::mem("rolled-compact")],
            artifacts: SegmentArtifacts::new(),
            live_starts: Vec::new(),
            should_pass: false,
        },
        Case {
            name: "rollback_restricts_rendered_cover",
            segments: vec![raw(0, 2), Segment::mem("committed-compact")],
            artifacts: artifacts(vec![artifact("committed-compact", 2, 20)]),
            live_starts: vec![20],
            should_pass: true,
        },
        Case {
            name: "partial_install_exposes_mem_without_artifact",
            segments: vec![raw(0, 2), Segment::mem("missing-artifact")],
            artifacts: SegmentArtifacts::new(),
            live_starts: Vec::new(),
            should_pass: false,
        },
        Case {
            name: "note_zero_width",
            segments: vec![raw(0, 2), Segment::note("handoff"), raw(2, 3)],
            artifacts: SegmentArtifacts::new(),
            live_starts: vec![2, 3],
            should_pass: true,
        },
    ];

    for case in cases {
        let result = validate_cover(&case.segments, &case.artifacts).and_then(|_| {
            validate_future_live_boundaries(&case.segments, &case.artifacts, &case.live_starts)
        });
        assert_eq!(
            result.is_ok(),
            case.should_pass,
            "case {} returned {result:?}",
            case.name
        );
    }
}

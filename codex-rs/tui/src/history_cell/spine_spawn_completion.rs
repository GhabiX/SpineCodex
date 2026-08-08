use codex_app_server_protocol::SpineSpawnOutcome;
use codex_app_server_protocol::SpineTreeNodeKind;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) const COMPLETION_DURATION: Duration = Duration::from_millis(850);
pub(super) const LAST_LOGICAL_FRAME: u8 = 51;

const BODY_ROWS: usize = 5;
const MASTER_END: f32 = 0.84;
const REFERENCE_EVENT_COUNT: usize = 13;
const REFERENCE_ROW_POSITIONS: [usize; BODY_ROWS] = [2, 4, 6, 9, 12];
const WORD_GLYPH_FADE_SPAN: f32 = 0.211;
const BODY_FADE_SPAN: f32 = 0.300;
const CHECK_FADE_START: f32 = 0.80;
const CHECK_FADE_END: f32 = 0.94;
const LINEAR_ACCELERATION: f32 = 0.30;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ActivitySlot {
    pub(super) grapheme: String,
    pub(super) width: usize,
    pub(super) alpha: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CompletionFrame {
    pub(super) activity_slots: Vec<ActivitySlot>,
    pub(super) check_alpha: f32,
    pub(super) body_alphas: Vec<f32>,
}

pub(super) fn completion_deadline(started_at: Instant) -> Instant {
    started_at + COMPLETION_DURATION
}

pub(super) fn completion_frame(
    activity_word: &str,
    started_at: Instant,
    now: Instant,
) -> CompletionFrame {
    let mut activity_slots = std::iter::once("")
        .take(activity_word.is_empty() as usize)
        .chain(activity_word.graphemes(true))
        .map(|grapheme| ActivitySlot {
            grapheme: grapheme.to_string(),
            width: UnicodeWidthStr::width(grapheme).max(1),
            alpha: 1.0,
        })
        .collect::<Vec<_>>();
    let elapsed = now
        .saturating_duration_since(started_at)
        .min(COMPLETION_DURATION);
    let frame = logical_frame(elapsed);
    let progress = accelerated_progress(f32::from(frame) / f32::from(LAST_LOGICAL_FRAME));
    let (glyph_deadlines, row_deadlines) = deadlines(activity_slots.len());
    for (slot, deadline) in activity_slots
        .iter_mut()
        .zip(glyph_deadlines.into_iter().rev())
    {
        slot.alpha = fade_out(progress, deadline, WORD_GLYPH_FADE_SPAN);
    }
    let body_alphas = row_deadlines
        .into_iter()
        .rev()
        .take_while(|deadline| progress < *deadline)
        .map(|deadline| fade_out(progress, deadline, BODY_FADE_SPAN))
        .collect();
    CompletionFrame {
        activity_slots,
        check_alpha: smootherstep(
            (progress - CHECK_FADE_START) / (CHECK_FADE_END - CHECK_FADE_START),
        ),
        body_alphas,
    }
}

pub(super) fn next_frame_in(started_at: Instant, now: Instant) -> Option<Duration> {
    let frame = logical_frame(now.saturating_duration_since(started_at));
    (frame < LAST_LOGICAL_FRAME).then(|| {
        let next_elapsed =
            COMPLETION_DURATION.mul_f64(f64::from(frame + 1) / f64::from(LAST_LOGICAL_FRAME));
        (started_at + next_elapsed).saturating_duration_since(now)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SettledTaskVisual {
    pub(super) outcome: SpineSpawnOutcome,
    pub(super) completion_deadline: Option<Instant>,
}

pub(super) fn plan_handoff(
    prior: &SpineTreeUpdatedNotification,
    latest: &SpineTreeUpdatedNotification,
    settled_tasks: &[SettledTaskVisual],
    now: Instant,
) -> Option<Instant> {
    (prior.active_node_id == latest.active_node_id
        && latest.nodes.len() == prior.nodes.len() + settled_tasks.len()
        && latest.nodes.starts_with(&prior.nodes))
    .then_some(())?;
    let mut latest_ids = HashSet::with_capacity(latest.nodes.len());
    for node in &latest.nodes {
        latest_ids.insert(node.node_id.as_str()).then_some(())?;
    }
    let mut reveal_at = None;
    for (node, task) in latest.nodes[prior.nodes.len()..].iter().zip(settled_tasks) {
        let outcome = node.spawn_outcome?;
        (node.parent_id.as_deref() == Some(latest.active_node_id.as_str())
            && node.kind == SpineTreeNodeKind::Task
            && node.status == SpineTreeNodeStatus::Closed
            && outcome == task.outcome
            && !latest
                .nodes
                .iter()
                .any(|candidate| candidate.parent_id.as_deref() == Some(node.node_id.as_str())))
        .then_some(())?;
        let deadline = match (task.outcome, task.completion_deadline) {
            (SpineSpawnOutcome::Completed, Some(deadline)) => Some(deadline),
            (SpineSpawnOutcome::Errored | SpineSpawnOutcome::Aborted, None) => None,
            _ => return None,
        };
        reveal_at = reveal_at.max(deadline);
    }
    reveal_at.filter(|deadline| *deadline > now)
}

fn logical_frame(elapsed: Duration) -> u8 {
    ((elapsed.as_nanos() * u128::from(LAST_LOGICAL_FRAME)) / COMPLETION_DURATION.as_nanos())
        .min(u128::from(LAST_LOGICAL_FRAME)) as u8
}

fn accelerated_progress(progress: f32) -> f32 {
    let p = progress.clamp(0.0, 1.0);
    (p + 0.5 * LINEAR_ACCELERATION * p * p) / (1.0 + 0.5 * LINEAR_ACCELERATION)
}

fn smootherstep(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    progress * progress * progress * (progress * (progress * 6.0 - 15.0) + 10.0)
}

fn fade_out(progress: f32, fade_end: f32, fade_span: f32) -> f32 {
    1.0 - smootherstep((progress - (fade_end - fade_span)) / fade_span)
}

fn deadlines(glyph_count: usize) -> (Vec<f32>, [f32; BODY_ROWS]) {
    let event_count = glyph_count.max(1) + BODY_ROWS;
    let positions = std::array::from_fn(|index| {
        let reference = REFERENCE_ROW_POSITIONS[index];
        let scaled = (reference * event_count + REFERENCE_EVENT_COUNT / 2) / REFERENCE_EVENT_COUNT;
        scaled.clamp(index + 1, event_count - (BODY_ROWS - index - 1))
    });
    let deadline = |position: usize| {
        let distance = position as f32 / event_count as f32;
        MASTER_END * (2.0 * (1.0 - distance).acos() / std::f32::consts::PI)
    };
    let rows = positions.map(deadline);
    let glyphs = (1..=event_count)
        .filter(|position| !positions.contains(position))
        .map(deadline)
        .collect();
    (glyphs, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::SpineTreeNode;
    use pretty_assertions::assert_eq;

    #[test]
    fn timeline_has_fixed_frames_and_skips_missed_frames() {
        let start = Instant::now();
        assert_eq!(logical_frame(Duration::ZERO), 0);
        assert_eq!(logical_frame(COMPLETION_DURATION / 2), 25);
        assert_eq!(logical_frame(COMPLETION_DURATION), LAST_LOGICAL_FRAME);
        assert_eq!(next_frame_in(start, start + COMPLETION_DURATION), None);
        assert_eq!(
            completion_frame("Growing", start, start + COMPLETION_DURATION * 2).check_alpha,
            1.0
        );
    }

    #[test]
    fn timeline_erases_graphemes_right_to_left_and_folds_bottom_up() {
        let start = Instant::now();
        let word = "Growing";
        let frames = (0..=LAST_LOGICAL_FRAME)
            .map(|frame| {
                completion_frame(
                    word,
                    start,
                    start
                        + COMPLETION_DURATION
                            .mul_f64(f64::from(frame) / f64::from(LAST_LOGICAL_FRAME)),
                )
            })
            .collect::<Vec<_>>();

        for pair in frames.windows(2) {
            for (before, after) in pair[0].activity_slots.iter().zip(&pair[1].activity_slots) {
                assert!(after.alpha <= before.alpha);
            }
            assert!(pair[1].body_alphas.len() <= pair[0].body_alphas.len());
        }
        let erased_at = |index: usize| {
            frames
                .iter()
                .position(|frame| frame.activity_slots[index].alpha == 0.0)
                .expect("every glyph erases")
        };
        for left_to_right in 1..frames[0].activity_slots.len() {
            assert!(erased_at(left_to_right - 1) >= erased_at(left_to_right));
        }
        assert_eq!(frames.first().expect("first").body_alphas.len(), BODY_ROWS);
        assert!(frames.last().expect("last").body_alphas.is_empty());
        assert_eq!(frames.last().expect("last").check_alpha, 1.0);
    }

    #[test]
    fn timeline_is_grapheme_safe_and_preserves_slot_widths() {
        let start = Instant::now();
        let frame = completion_frame("A界e\u{301}", start, start);

        assert_eq!(
            frame
                .activity_slots
                .iter()
                .map(|slot| slot.grapheme.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "界", "e\u{301}"]
        );
        assert_eq!(
            frame
                .activity_slots
                .iter()
                .map(|slot| slot.width)
                .collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
    }

    #[test]
    fn timeline_supports_every_current_activity_word_length() {
        for word in crate::motion::ORGANIC_ACTIVITY_WORDS {
            let start = Instant::now();
            let glyph_count = word.graphemes(true).count();
            assert_eq!(deadlines(glyph_count).0.len(), glyph_count);
            assert_eq!(
                completion_frame(word, start, completion_deadline(start)).check_alpha,
                1.0
            );
        }
    }

    #[test]
    fn normalized_curve_has_thirty_percent_linear_acceleration() {
        let samples = (0..=50)
            .map(|step| {
                accelerated_progress((step + 1) as f32 / 51.0)
                    - accelerated_progress(step as f32 / 51.0)
            })
            .collect::<Vec<_>>();
        let ratio = samples.last().expect("last") / samples.first().expect("first");
        assert!((1.29..=1.30).contains(&ratio));
        for acceleration in samples.windows(2).map(|pair| pair[1] - pair[0]) {
            assert!((acceleration - (samples[1] - samples[0])).abs() < 1e-6);
        }
    }

    #[test]
    fn handoff_hides_the_complete_matching_imported_batch() {
        let now = Instant::now();
        let prior = snapshot(vec![node("root", None, SpineTreeNodeStatus::Opened)]);
        let latest = snapshot(vec![
            node("root", None, SpineTreeNodeStatus::Opened),
            node(
                "root.1",
                Some(SpineSpawnOutcome::Completed),
                SpineTreeNodeStatus::Closed,
            ),
            node(
                "root.2",
                Some(SpineSpawnOutcome::Errored),
                SpineTreeNodeStatus::Closed,
            ),
        ]);
        let decision = plan_handoff(
            &prior,
            &latest,
            &[
                SettledTaskVisual {
                    outcome: SpineSpawnOutcome::Completed,
                    completion_deadline: Some(now + COMPLETION_DURATION),
                },
                SettledTaskVisual {
                    outcome: SpineSpawnOutcome::Errored,
                    completion_deadline: None,
                },
            ],
            now,
        );

        assert_eq!(decision, Some(now + COMPLETION_DURATION));
    }

    #[test]
    fn handoff_fails_open_for_partial_or_mismatched_batches() {
        let now = Instant::now();
        let prior = snapshot(vec![node("root", None, SpineTreeNodeStatus::Opened)]);
        let latest = snapshot(vec![
            node("root", None, SpineTreeNodeStatus::Opened),
            node(
                "root.1",
                Some(SpineSpawnOutcome::Completed),
                SpineTreeNodeStatus::Closed,
            ),
            node(
                "root.2",
                Some(SpineSpawnOutcome::Errored),
                SpineTreeNodeStatus::Closed,
            ),
        ]);
        let completed = SettledTaskVisual {
            outcome: SpineSpawnOutcome::Completed,
            completion_deadline: Some(now + COMPLETION_DURATION),
        };
        let errored = SettledTaskVisual {
            outcome: SpineSpawnOutcome::Errored,
            completion_deadline: None,
        };

        assert_eq!(plan_handoff(&prior, &latest, &[completed], now), None);
        assert_eq!(
            plan_handoff(&prior, &latest, &[errored, completed], now),
            None
        );
        assert_eq!(
            plan_handoff(
                &prior,
                &latest,
                &[
                    completed,
                    SettledTaskVisual {
                        outcome: SpineSpawnOutcome::Aborted,
                        completion_deadline: None,
                    },
                ],
                now,
            ),
            None
        );
    }

    #[test]
    fn handoff_fails_open_without_active_success_or_for_invalid_typed_nodes() {
        let now = Instant::now();
        let prior = snapshot(vec![node("root", None, SpineTreeNodeStatus::Opened)]);
        let errored = node(
            "root.1",
            Some(SpineSpawnOutcome::Errored),
            SpineTreeNodeStatus::Closed,
        );
        assert_eq!(
            plan_handoff(
                &prior,
                &snapshot(vec![prior.nodes[0].clone(), errored]),
                &[SettledTaskVisual {
                    outcome: SpineSpawnOutcome::Errored,
                    completion_deadline: None,
                }],
                now,
            ),
            None
        );

        let invalid = node(
            "root.1",
            Some(SpineSpawnOutcome::Completed),
            SpineTreeNodeStatus::Live,
        );
        assert_eq!(
            plan_handoff(
                &prior,
                &snapshot(vec![prior.nodes[0].clone(), invalid]),
                &[SettledTaskVisual {
                    outcome: SpineSpawnOutcome::Completed,
                    completion_deadline: Some(now + COMPLETION_DURATION),
                }],
                now,
            ),
            None
        );

        let parent = node(
            "root.1",
            Some(SpineSpawnOutcome::Completed),
            SpineTreeNodeStatus::Closed,
        );
        let child = node(
            "root.1.1",
            Some(SpineSpawnOutcome::Completed),
            SpineTreeNodeStatus::Closed,
        );
        assert_eq!(
            plan_handoff(
                &prior,
                &snapshot(vec![prior.nodes[0].clone(), parent, child]),
                &[
                    SettledTaskVisual {
                        outcome: SpineSpawnOutcome::Completed,
                        completion_deadline: Some(now + COMPLETION_DURATION),
                    },
                    SettledTaskVisual {
                        outcome: SpineSpawnOutcome::Completed,
                        completion_deadline: Some(now + COMPLETION_DURATION),
                    },
                ],
                now,
            ),
            None
        );
    }

    #[test]
    fn handoff_deadline_is_bounded_and_expired_decisions_reveal() {
        let now = Instant::now();
        let prior = snapshot(vec![node("root", None, SpineTreeNodeStatus::Opened)]);
        let latest = snapshot(vec![
            prior.nodes[0].clone(),
            node(
                "root.1",
                Some(SpineSpawnOutcome::Completed),
                SpineTreeNodeStatus::Closed,
            ),
        ]);
        let visual = SettledTaskVisual {
            outcome: SpineSpawnOutcome::Completed,
            completion_deadline: Some(now + COMPLETION_DURATION),
        };

        let first = plan_handoff(&prior, &latest, &[visual], now);
        let second = plan_handoff(&prior, &latest, &[visual], now);
        assert_eq!(first, second);
        assert_eq!(
            plan_handoff(&prior, &latest, &[visual], now + COMPLETION_DURATION),
            None
        );
    }

    #[test]
    fn handoff_fails_open_for_unrelated_authoritative_changes() {
        let now = Instant::now();
        let prior = snapshot(vec![node("root", None, SpineTreeNodeStatus::Opened)]);
        let mut latest = snapshot(vec![
            prior.nodes[0].clone(),
            node(
                "root.1",
                Some(SpineSpawnOutcome::Completed),
                SpineTreeNodeStatus::Closed,
            ),
        ]);
        let visual = SettledTaskVisual {
            outcome: SpineSpawnOutcome::Completed,
            completion_deadline: Some(now + COMPLETION_DURATION),
        };

        latest.nodes[0].summary = Some("changed ancestor".to_string());
        assert_eq!(plan_handoff(&prior, &latest, &[visual], now), None);
        latest.nodes[0] = prior.nodes[0].clone();
        latest.active_node_id = "root.1".to_string();
        assert_eq!(plan_handoff(&prior, &latest, &[visual], now), None);
        latest.active_node_id = "root".to_string();
        latest
            .nodes
            .push(node("root.2", None, SpineTreeNodeStatus::Closed));
        assert_eq!(plan_handoff(&prior, &latest, &[visual], now), None);

        let prior = snapshot(vec![
            node("root", None, SpineTreeNodeStatus::Opened),
            node("root.0", None, SpineTreeNodeStatus::Closed),
        ]);
        let imported = node(
            "root.1",
            Some(SpineSpawnOutcome::Completed),
            SpineTreeNodeStatus::Closed,
        );
        let reordered = snapshot(vec![
            prior.nodes[1].clone(),
            prior.nodes[0].clone(),
            imported.clone(),
        ]);
        assert_eq!(plan_handoff(&prior, &reordered, &[visual], now), None);

        let mut wrong_parent = snapshot(vec![prior.nodes[0].clone(), imported]);
        wrong_parent.nodes[1].parent_id = Some("elsewhere".to_string());
        assert_eq!(
            plan_handoff(
                &snapshot(vec![prior.nodes[0].clone()]),
                &wrong_parent,
                &[visual],
                now,
            ),
            None
        );
    }

    fn snapshot(nodes: Vec<SpineTreeNode>) -> SpineTreeUpdatedNotification {
        SpineTreeUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            snapshot_seq: 1,
            active_node_id: "root".to_string(),
            nodes,
            settled_spawn_call_ids: Vec::new(),
        }
    }

    fn node(
        node_id: &str,
        spawn_outcome: Option<SpineSpawnOutcome>,
        status: SpineTreeNodeStatus,
    ) -> SpineTreeNode {
        SpineTreeNode {
            node_id: node_id.to_string(),
            parent_id: node_id
                .rsplit_once('.')
                .map(|(parent, _)| parent.to_string()),
            kind: if node_id == "root" {
                SpineTreeNodeKind::RootEpoch
            } else {
                SpineTreeNodeKind::Task
            },
            status,
            summary: None,
            memory_summary: None,
            spawn_outcome,
            start: 0,
            end: (status == SpineTreeNodeStatus::Closed).then_some(1),
            context_pressure: None,
        }
    }
}

use super::*;
use crate::context_manager::is_user_turn_boundary;
use crate::spine::context_materialization::SpineMaterializationInput;
use crate::spine::context_materialization::materialize_spine_context;
use codex_protocol::protocol::SpineCompactedCheckpointKind;

// Return value of `Session::reconstruct_history_from_rollout`, bundling the rebuilt history with
// the resume/fork hydration metadata derived from the same replay.
#[derive(Debug)]
pub(super) struct RolloutReconstruction {
    pub(super) history: Vec<ResponseItem>,
    pub(super) previous_turn_settings: Option<PreviousTurnSettings>,
    pub(super) reference_context_item: Option<TurnContextItem>,
}

#[derive(Debug, Default)]
enum TurnReferenceContextItem {
    /// No `TurnContextItem` has been seen for this replay span yet.
    ///
    /// This differs from `Cleared`: `NeverSet` means there is no evidence this turn ever
    /// established a baseline, while `Cleared` means a baseline existed and a later compaction
    /// invalidated it. Only the latter must emit an explicit clearing segment for resume/fork
    /// hydration.
    #[default]
    NeverSet,
    /// A previously established baseline was invalidated by later compaction.
    Cleared,
    /// The latest baseline established by this replay span.
    Latest(Box<TurnContextItem>),
}

#[derive(Debug, Default)]
struct ActiveReplaySegment {
    turn_id: Option<String>,
    counts_as_user_turn: bool,
    previous_turn_settings: Option<PreviousTurnSettings>,
    reference_context_item: TurnReferenceContextItem,
    base: Option<ReplayBase>,
}

#[derive(Debug, Clone)]
struct ReplayBase {
    history: Vec<ResponseItem>,
    // The replay suffix belongs to the same checkpoint as this base, so rollback filtering must
    // accept or reject both together.
    suffix_start: usize,
}

fn turn_ids_are_compatible(active_turn_id: Option<&str>, item_turn_id: Option<&str>) -> bool {
    active_turn_id
        .is_none_or(|turn_id| item_turn_id.is_none_or(|item_turn_id| item_turn_id == turn_id))
}

fn finalize_active_segment(
    active_segment: ActiveReplaySegment,
    base: &mut Option<ReplayBase>,
    previous_turn_settings: &mut Option<PreviousTurnSettings>,
    reference_context_item: &mut TurnReferenceContextItem,
    pending_rollback_turns: &mut usize,
) {
    // Thread rollback drops the newest surviving real user-message boundaries. In replay, that
    // means skipping the next finalized segments that contain a non-contextual
    // `EventMsg::UserMessage`.
    if *pending_rollback_turns > 0 {
        if active_segment.counts_as_user_turn {
            *pending_rollback_turns -= 1;
        }
        return;
    }

    // A surviving compact checkpoint is a complete materialized host-history base.
    // Once we know the newest surviving one, older rollout items do not affect rebuilt history.
    if base.is_none()
        && let Some(segment_base) = active_segment.base
    {
        *base = Some(segment_base);
    }

    // `previous_turn_settings` come from the newest surviving user turn that established them.
    if previous_turn_settings.is_none() && active_segment.counts_as_user_turn {
        *previous_turn_settings = active_segment.previous_turn_settings;
    }

    // `reference_context_item` comes from the newest surviving user turn baseline, or
    // from a surviving compaction that explicitly cleared that baseline.
    if matches!(reference_context_item, TurnReferenceContextItem::NeverSet)
        && (active_segment.counts_as_user_turn
            || matches!(
                active_segment.reference_context_item,
                TurnReferenceContextItem::Cleared
            ))
    {
        *reference_context_item = active_segment.reference_context_item;
    }
}

impl Session {
    fn materialize_replay_base_from_compacted(
        &self,
        compacted: &CompactedItem,
        replay_items: &[RolloutItem],
        rollout_path: &Path,
        index: usize,
    ) -> CodexResult<Vec<ResponseItem>> {
        let Some(spine_checkpoint) = compacted.spine.as_ref() else {
            if let Some(replacement_history) = &compacted.replacement_history {
                return Ok(replacement_history.clone());
            }
            return Err(CodexErr::Fatal(format!(
                "unsupported compacted rollout item at index {index}: missing replacement_history"
            )));
        };
        if spine_checkpoint.kind != SpineCompactedCheckpointKind::Suffix {
            if let Some(replacement_history) = &compacted.replacement_history {
                return Ok(replacement_history.clone());
            }
            return Err(CodexErr::Fatal(format!(
                "unsupported Spine {:?} compacted rollout item at index {index}: missing replacement_history",
                spine_checkpoint.kind
            )));
        }
        let store = SpineSidecarStore::for_rollout(rollout_path).map_err(|err| {
            CodexErr::Fatal(format!(
                "failed to load Spine sidecar for compacted rollout item at index {index}: {err}"
            ))
        })?;
        materialize_spine_context(SpineMaterializationInput {
            replay_items,
            branch_ref: rollout_path.to_string_lossy().into_owned(),
            persisted_prefix_items: replay_items,
            store: &store,
        })
        .map(|materialized| materialized.history)
    }

    pub(super) async fn reconstruct_history_from_rollout(
        &self,
        turn_context: &TurnContext,
        rollout_items: &[RolloutItem],
    ) -> CodexResult<RolloutReconstruction> {
        // Replay metadata should already match the shape of the future lazy reverse loader, even
        // while history materialization still uses an eager bridge. Scan newest-to-oldest,
        // stopping once a surviving materialized compact checkpoint and the required resume
        // metadata are both known; then replay only the buffered surviving tail forward to
        // preserve exact host-history materialization.
        let rollout_path = self
            .current_rollout_path()
            .await
            .map_err(|err| CodexErr::Fatal(format!("failed to resolve rollout path: {err:#}")))?;
        let mut base: Option<ReplayBase> = None;
        let mut previous_turn_settings = None;
        let mut reference_context_item = TurnReferenceContextItem::NeverSet;
        // Rollback is "drop the newest N user turns". While scanning in reverse, that becomes
        // "skip the next N user-turn segments we finalize".
        let mut pending_rollback_turns = 0usize;
        // Reverse replay accumulates rollout items into the newest in-progress turn segment until
        // we hit its matching `TurnStarted`, at which point the segment can be finalized.
        let mut active_segment: Option<ActiveReplaySegment> = None;

        for (index, item) in rollout_items.iter().enumerate().rev() {
            match item {
                RolloutItem::Compacted(compacted) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    // Looking backward, compaction clears any older baseline unless a newer
                    // `TurnContextItem` in this same segment has already re-established it.
                    if matches!(
                        active_segment.reference_context_item,
                        TurnReferenceContextItem::NeverSet
                    ) {
                        active_segment.reference_context_item = TurnReferenceContextItem::Cleared;
                    }
                    if active_segment.base.is_none() {
                        let replay_items = &rollout_items[..=index];
                        let history = match rollout_path.as_deref() {
                            Some(rollout_path) => self.materialize_replay_base_from_compacted(
                                compacted,
                                replay_items,
                                rollout_path,
                                index,
                            )?,
                            None => {
                                let Some(replacement_history) = &compacted.replacement_history
                                else {
                                    return Err(CodexErr::Fatal(format!(
                                        "unsupported compacted rollout item at index {index}: missing replacement_history"
                                    )));
                                };
                                replacement_history.clone()
                            }
                        };
                        active_segment.base = Some(ReplayBase {
                            history,
                            suffix_start: index + 1,
                        });
                    }
                }
                RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                    let rollback_turns = usize::try_from(rollback.num_turns).map_err(|_| {
                        CodexErr::Fatal(format!(
                            "unsupported rolled-back turn count {} in rollout reconstruction",
                            rollback.num_turns
                        ))
                    })?;
                    pending_rollback_turns = pending_rollback_turns
                        .checked_add(rollback_turns)
                        .ok_or_else(|| {
                            CodexErr::Fatal(
                                "rolled-back turn count overflowed rollout reconstruction"
                                    .to_string(),
                            )
                        })?;
                }
                RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    // Reverse replay often sees `TurnComplete` before any turn-scoped metadata.
                    // Capture the turn id early so later `TurnContext` / abort items can match it.
                    if active_segment.turn_id.is_none() {
                        active_segment.turn_id = Some(event.turn_id.clone());
                    }
                }
                RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
                    if let Some(active_segment) = active_segment.as_mut() {
                        if active_segment.turn_id.is_none()
                            && let Some(turn_id) = &event.turn_id
                        {
                            active_segment.turn_id = Some(turn_id.clone());
                        }
                    } else if let Some(turn_id) = &event.turn_id {
                        active_segment = Some(ActiveReplaySegment {
                            turn_id: Some(turn_id.clone()),
                            ..Default::default()
                        });
                    }
                }
                RolloutItem::EventMsg(EventMsg::UserMessage(_)) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.counts_as_user_turn = true;
                }
                RolloutItem::TurnContext(ctx) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    // `TurnContextItem` can attach metadata to an existing segment, but only a
                    // real `UserMessage` event should make the segment count as a user turn.
                    if active_segment.turn_id.is_none() {
                        active_segment.turn_id = ctx.turn_id.clone();
                    }
                    if turn_ids_are_compatible(
                        active_segment.turn_id.as_deref(),
                        ctx.turn_id.as_deref(),
                    ) {
                        active_segment.previous_turn_settings = Some(PreviousTurnSettings {
                            model: ctx.model.clone(),
                            realtime_active: ctx.realtime_active,
                        });
                        if matches!(
                            active_segment.reference_context_item,
                            TurnReferenceContextItem::NeverSet
                        ) {
                            active_segment.reference_context_item =
                                TurnReferenceContextItem::Latest(Box::new(ctx.clone()));
                        }
                    }
                }
                RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
                    // `TurnStarted` is the oldest boundary of the active reverse segment.
                    if active_segment.as_ref().is_some_and(|active_segment| {
                        turn_ids_are_compatible(
                            active_segment.turn_id.as_deref(),
                            Some(event.turn_id.as_str()),
                        )
                    }) && let Some(active_segment) = active_segment.take()
                    {
                        finalize_active_segment(
                            active_segment,
                            &mut base,
                            &mut previous_turn_settings,
                            &mut reference_context_item,
                            &mut pending_rollback_turns,
                        );
                    }
                }
                RolloutItem::ResponseItem(response_item) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.counts_as_user_turn |= is_user_turn_boundary(response_item);
                }
                RolloutItem::EventMsg(_) | RolloutItem::SessionMeta(_) => {}
            }

            if base.is_some()
                && previous_turn_settings.is_some()
                && !matches!(reference_context_item, TurnReferenceContextItem::NeverSet)
            {
                // At this point we have both eager resume metadata values and the materialized
                // compact checkpoint base for the surviving tail, so older rollout items cannot
                // affect this result.
                break;
            }
        }

        if let Some(active_segment) = active_segment.take() {
            finalize_active_segment(
                active_segment,
                &mut base,
                &mut previous_turn_settings,
                &mut reference_context_item,
                &mut pending_rollback_turns,
            );
        }

        let mut history = ContextManager::new();
        let rollout_suffix_start = if let Some(base) = base {
            history.replace(base.history);
            base.suffix_start
        } else {
            0
        };
        // Materialize exact host-history state from the replay-derived suffix. The eventual lazy
        // design should keep this same replay shape, but drive it from a resumable reverse source
        // instead of an eagerly loaded `&[RolloutItem]`.
        for (suffix_offset, item) in rollout_items[rollout_suffix_start..].iter().enumerate() {
            let index = rollout_suffix_start + suffix_offset;
            match item {
                RolloutItem::ResponseItem(response_item) => {
                    history.record_items(
                        std::iter::once(response_item),
                        turn_context.truncation_policy,
                    );
                }
                RolloutItem::Compacted(compacted) => {
                    // A later compact checkpoint in the surviving suffix replaces the
                    // materialized host history when replay admits it.
                    let replay_items = &rollout_items[..=index];
                    let compacted_history = match rollout_path.as_deref() {
                        Some(rollout_path) => self.materialize_replay_base_from_compacted(
                            compacted,
                            replay_items,
                            rollout_path,
                            index,
                        )?,
                        None => {
                            let Some(replacement_history) = &compacted.replacement_history else {
                                return Err(CodexErr::Fatal(format!(
                                    "unsupported compacted rollout item at index {index}: missing replacement_history"
                                )));
                            };
                            replacement_history.clone()
                        }
                    };
                    history.replace(compacted_history);
                }
                RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                    let rollback_turns = usize::try_from(rollback.num_turns).map_err(|_| {
                        CodexErr::Fatal(format!(
                            "unsupported rolled-back turn count {} in rollout reconstruction",
                            rollback.num_turns
                        ))
                    })?;
                    history.drop_last_n_user_turns(rollback_turns);
                }
                RolloutItem::EventMsg(_)
                | RolloutItem::TurnContext(_)
                | RolloutItem::SessionMeta(_) => {}
            }
        }

        let reference_context_item = match reference_context_item {
            TurnReferenceContextItem::NeverSet | TurnReferenceContextItem::Cleared => None,
            TurnReferenceContextItem::Latest(turn_reference_context_item) => {
                Some(*turn_reference_context_item)
            }
        };
        Ok(RolloutReconstruction {
            history: history.raw_items().to_vec(),
            previous_turn_settings,
            reference_context_item,
        })
    }
}

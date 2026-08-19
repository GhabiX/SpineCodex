use super::spine_spawn_completion::SettledTaskVisual;
use super::spine_spawn_completion::plan_handoff;
use super::spine_spawn_progress::SpineSpawnOverlay;
use super::*;
use crate::product_brand::SPINE_BRAND_COLOR;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SpineSpawnOutcome;
use codex_app_server_protocol::SpineSpawnProgressUpdatedNotification;
use codex_app_server_protocol::SpineTreeNode;
use codex_app_server_protocol::SpineTreeNodeKind;
use codex_app_server_protocol::SpineTreeNodeStatus;
use codex_app_server_protocol::SpineTreeUpdatedNotification;
use codex_protocol::ThreadId;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;

#[path = "spine_tree_debug.rs"]
mod debug;

const PRETTY_MAX_VISIBLE_SIBLINGS: usize = 3;
const INVALID_SPINE_TREE_SNAPSHOT_LABEL: &str = "invalid Spine tree snapshot";
const LIVE_SPAWN_ROOT_ID: &str = "spine-live-root";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OverlaySignature {
    turn_id: String,
    call_id: String,
    tasks: Vec<(u32, String)>,
}

impl OverlaySignature {
    fn from_progress(notification: &SpineSpawnProgressUpdatedNotification) -> Option<Self> {
        if notification.tasks.is_empty() {
            return None;
        }
        let mut child_ids = HashSet::with_capacity(notification.tasks.len());
        for (index, task) in notification.tasks.iter().enumerate() {
            if u32::try_from(index).ok() != Some(task.ordinal)
                || task.thread_id.is_empty()
                || !child_ids.insert(task.thread_id.as_str())
            {
                return None;
            }
        }
        Some(Self {
            turn_id: notification.turn_id.clone(),
            call_id: notification.call_id.clone(),
            tasks: notification
                .tasks
                .iter()
                .map(|task| (task.ordinal, task.thread_id.clone()))
                .collect(),
        })
    }

    fn from_overlay(overlay: &SpineSpawnOverlay) -> Self {
        Self {
            turn_id: overlay.turn_id().to_string(),
            call_id: overlay.call_id().to_string(),
            tasks: overlay.task_signature(),
        }
    }

    fn same_transaction(&self, turn_id: &str, call_id: &str) -> bool {
        self.turn_id == turn_id && self.call_id == call_id
    }
}

#[cfg(test)]
pub(crate) fn new_spine_tree_snapshot(
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        snapshot,
        display_mode: SpineTreeDisplayMode::Pretty,
        spawn_overlays: Vec::new(),
        pending_handoff: None,
        animations_enabled: false,
        automatic_history: false,
    }
}

pub(crate) fn new_debug_spine_tree_snapshot(
    snapshot: SpineTreeUpdatedNotification,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        snapshot,
        display_mode: SpineTreeDisplayMode::Debug(None),
        spawn_overlays: Vec::new(),
        pending_handoff: None,
        animations_enabled: false,
        automatic_history: false,
    }
}

pub(crate) fn new_debug_spine_node_snapshot(
    snapshot: SpineTreeUpdatedNotification,
    node_id: String,
) -> SpineTreeUpdateCell {
    SpineTreeUpdateCell {
        snapshot,
        display_mode: SpineTreeDisplayMode::Debug(Some(node_id)),
        spawn_overlays: Vec::new(),
        pending_handoff: None,
        animations_enabled: false,
        automatic_history: false,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SpineTreeViewState {
    snapshot: Option<SpineTreeUpdatedNotification>,
    pending_history: Option<SpineTreeUpdatedNotification>,
    overlays: Vec<SpineSpawnOverlay>,
    settled_spawn_signatures: HashSet<OverlaySignature>,
    // Retry changes thread identities, so a signature cannot guard the whole transaction. Keep
    // settled turn/call pairs for this view's lifetime; clearing one at normal turn completion
    // would let a delayed progress event from any attempt recreate an already-settled overlay.
    // Incomplete/reset cleanup clears obsolete guards.
    settled_spawn_transactions: HashSet<(String, String)>,
    pending_handoff: Option<PendingTreeHandoff>,
    animations_enabled: bool,
}

#[derive(Debug, Clone)]
struct PendingTreeHandoff {
    snapshot: SpineTreeUpdatedNotification,
    reveal_at: Instant,
    overlays: Vec<SpineSpawnOverlay>,
    activity_pending: bool,
}

#[derive(Debug, Clone, Copy)]
enum OverlayLocation {
    Active(usize),
    Pending(usize),
}

impl Default for SpineTreeViewState {
    fn default() -> Self {
        Self::new(false)
    }
}

impl SpineTreeViewState {
    pub(crate) fn new(animations_enabled: bool) -> Self {
        Self {
            snapshot: None,
            pending_history: None,
            overlays: Vec::new(),
            settled_spawn_signatures: HashSet::new(),
            settled_spawn_transactions: HashSet::new(),
            pending_handoff: None,
            animations_enabled,
        }
    }

    pub(crate) fn snapshot(&self) -> Option<&SpineTreeUpdatedNotification> {
        self.snapshot.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn apply_tree_update(&mut self, snapshot: SpineTreeUpdatedNotification) {
        self.apply_tree_update_at(
            snapshot,
            Instant::now(),
            /*await_terminal_activity*/ false,
        );
    }

    pub(crate) fn apply_tree_update_awaiting_terminal_activity(
        &mut self,
        snapshot: SpineTreeUpdatedNotification,
    ) {
        self.apply_tree_update_at(
            snapshot,
            Instant::now(),
            /*await_terminal_activity*/ true,
        );
    }

    fn apply_tree_update_at(
        &mut self,
        snapshot: SpineTreeUpdatedNotification,
        now: Instant,
        await_terminal_activity: bool,
    ) {
        if self
            .snapshot
            .as_ref()
            .is_some_and(|current| snapshot.snapshot_seq < current.snapshot_seq)
        {
            return;
        }
        let settlement =
            self.settlement_overlays_for(&snapshot.turn_id, &snapshot.settled_spawn_call_ids);
        let has_matching_settlement = settlement
            .as_ref()
            .is_some_and(|matched| !matched.is_empty());
        let display_changed = self
            .snapshot
            .as_ref()
            .is_some_and(|current| display_tree_changed(current, &snapshot));
        let handoff_superseded =
            self.pending_handoff.is_some() && (has_matching_settlement || display_changed);
        if handoff_superseded {
            self.pending_handoff = None;
        }
        let prior = self
            .snapshot
            .replace(snapshot)
            .or_else(|| self.live_spawn_snapshot());

        let mut started_handoff = false;
        if let Some(matched) = settlement
            && !matched.is_empty()
        {
            let signatures = matched
                .iter()
                .map(|(signature, _)| signature.clone())
                .collect::<HashSet<_>>();
            let matched_overlays = matched
                .iter()
                .map(|(_, overlay)| overlay.clone())
                .collect::<Vec<_>>();
            let reveal_at = if self.animations_enabled {
                prior.as_ref().and_then(|prior| {
                    Self::settled_visuals_for(&matched_overlays).and_then(|settled_tasks| {
                        self.snapshot
                            .as_ref()
                            .and_then(|latest| plan_handoff(prior, latest, &settled_tasks, now))
                    })
                })
            } else {
                None
            };
            if let (Some(prior), Some(reveal_at)) =
                (prior, reveal_at.or(await_terminal_activity.then_some(now)))
            {
                self.pending_handoff = Some(PendingTreeHandoff {
                    snapshot: prior,
                    reveal_at,
                    overlays: matched_overlays,
                    activity_pending: await_terminal_activity,
                });
                started_handoff = true;
            }
            self.settled_spawn_signatures
                .extend(signatures.iter().cloned());
            self.settled_spawn_transactions.extend(
                signatures
                    .iter()
                    .map(|signature| (signature.turn_id.clone(), signature.call_id.clone())),
            );
            self.overlays
                .retain(|overlay| !signatures.contains(&OverlaySignature::from_overlay(overlay)));
        }

        let refresh_pending_history = !started_handoff && (display_changed || handoff_superseded);
        if refresh_pending_history {
            self.pending_history = self.snapshot.clone();
        }
    }

    fn settlement_overlays_for(
        &self,
        turn_id: &str,
        call_ids: &[String],
    ) -> Option<Vec<(OverlaySignature, SpineSpawnOverlay)>> {
        let mut seen = HashSet::with_capacity(call_ids.len());
        let mut matched = Vec::new();
        for call_id in call_ids {
            if !seen.insert(call_id.as_str()) {
                return None;
            }
            let mut overlays = self
                .overlays
                .iter()
                .filter(|overlay| overlay.turn_id() == turn_id && overlay.call_id() == call_id);
            let Some(overlay) = overlays.next() else {
                continue;
            };
            if overlays.next().is_some() {
                return None;
            }
            let signature = OverlaySignature::from_overlay(overlay);
            if self.settled_spawn_signatures.contains(&signature) {
                return None;
            }
            matched.push((signature, overlay.clone()));
        }
        Some(matched)
    }

    pub(crate) fn settling_spawn_root_thread_ids(
        &self,
        turn_id: &str,
        call_ids: &[String],
    ) -> Vec<ThreadId> {
        self.settlement_overlays_for(turn_id, call_ids)
            .unwrap_or_default()
            .into_iter()
            .flat_map(|(_, overlay)| {
                overlay
                    .child_thread_ids()
                    .filter_map(|thread_id| ThreadId::from_string(thread_id).ok())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(crate) fn incomplete_spawn_root_thread_ids(&self, turn_id: Option<&str>) -> Vec<ThreadId> {
        let overlays = self.overlays.iter().chain(
            self.pending_handoff
                .iter()
                .flat_map(|pending| pending.overlays.iter()),
        );
        overlays
            .filter(|overlay| turn_id.is_none_or(|turn_id| overlay.turn_id() == turn_id))
            .flat_map(SpineSpawnOverlay::child_thread_ids)
            .filter_map(|thread_id| ThreadId::from_string(thread_id).ok())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    fn settled_visuals_for(overlays: &[SpineSpawnOverlay]) -> Option<Vec<SettledTaskVisual>> {
        let mut tasks = Vec::new();
        for overlay in overlays {
            tasks.extend(overlay.settled_task_visuals()?);
        }
        Some(tasks)
    }

    pub(crate) fn clear_incomplete_spawn_overlays(&mut self, turn_id: Option<&str>) -> bool {
        let pending_cleared = self
            .pending_handoff
            .take_if(|pending| {
                pending
                    .overlays
                    .iter()
                    .any(|overlay| turn_id.is_none_or(|turn_id| overlay.turn_id() == turn_id))
            })
            .is_some();
        let before = self.overlays.len();
        self.overlays
            .retain(|overlay| turn_id.is_some_and(|turn_id| overlay.turn_id() != turn_id));
        let guards_before = self.settled_spawn_signatures.len();
        let transactions_before = self.settled_spawn_transactions.len();
        if let Some(turn_id) = turn_id {
            self.settled_spawn_signatures
                .retain(|signature| signature.turn_id != turn_id);
            self.settled_spawn_transactions
                .retain(|(settled_turn_id, _)| settled_turn_id != turn_id);
        } else {
            self.settled_spawn_signatures.clear();
            self.settled_spawn_transactions.clear();
        }
        if pending_cleared {
            self.pending_history = self.snapshot.clone();
        }
        pending_cleared
            || self.overlays.len() != before
            || self.settled_spawn_signatures.len() != guards_before
            || self.settled_spawn_transactions.len() != transactions_before
    }

    pub(crate) fn clear_completed_spawn_overlays(&mut self, turn_id: &str) -> bool {
        let before = self.overlays.len();
        self.overlays.retain(|overlay| overlay.turn_id() != turn_id);
        let guards_before = self.settled_spawn_signatures.len();
        self.settled_spawn_signatures
            .retain(|signature| signature.turn_id != turn_id);
        self.overlays.len() != before || self.settled_spawn_signatures.len() != guards_before
    }

    pub(crate) fn apply_spawn_progress(
        &mut self,
        notification: SpineSpawnProgressUpdatedNotification,
    ) {
        let Some(signature) = OverlaySignature::from_progress(&notification) else {
            return;
        };
        if self.settled_spawn_signatures.contains(&signature)
            || self
                .settled_spawn_transactions
                .contains(&(notification.turn_id.clone(), notification.call_id.clone()))
        {
            return;
        }
        let matching_index = {
            let mut indices = self
                .overlays
                .iter()
                .enumerate()
                .filter_map(|(index, overlay)| {
                    OverlaySignature::from_overlay(overlay)
                        .same_transaction(&notification.turn_id, &notification.call_id)
                        .then_some(index)
                });
            let first = indices.next();
            if indices.next().is_some() {
                return;
            }
            first
        };
        match matching_index {
            None => self.overlays.push(SpineSpawnOverlay::new(notification)),
            Some(index) if self.overlays[index].can_replace_with(&notification) => {
                self.overlays[index].replace_notification(notification);
            }
            Some(_) => {}
        }
    }

    fn unique_overlay_location(
        &self,
        mut matches: impl FnMut(&SpineSpawnOverlay) -> bool,
    ) -> Option<OverlayLocation> {
        let mut location = None;
        for (index, overlay) in self.overlays.iter().enumerate() {
            if matches(overlay) {
                if location.is_some() {
                    return None;
                }
                location = Some(OverlayLocation::Active(index));
            }
        }
        for (index, overlay) in self
            .pending_handoff
            .iter()
            .flat_map(|pending| pending.overlays.iter())
            .enumerate()
        {
            if matches(overlay) {
                if location.is_some() {
                    return None;
                }
                location = Some(OverlayLocation::Pending(index));
            }
        }
        location
    }

    fn overlay(&self, location: OverlayLocation) -> &SpineSpawnOverlay {
        match location {
            OverlayLocation::Active(index) => &self.overlays[index],
            OverlayLocation::Pending(index) => {
                &self
                    .pending_handoff
                    .as_ref()
                    .expect("pending overlay location requires a handoff")
                    .overlays[index]
            }
        }
    }

    fn overlay_mut(&mut self, location: OverlayLocation) -> &mut SpineSpawnOverlay {
        match location {
            OverlayLocation::Active(index) => &mut self.overlays[index],
            OverlayLocation::Pending(index) => {
                &mut self
                    .pending_handoff
                    .as_mut()
                    .expect("pending overlay location requires a handoff")
                    .overlays[index]
            }
        }
    }

    pub(crate) fn seed_activity(
        &mut self,
        turn_id: &str,
        call_id: &str,
        thread_id: &str,
        notifications: impl Iterator<Item = ServerNotification>,
    ) -> bool {
        let Some(location) = self.unique_overlay_location(|overlay| {
            overlay.turn_id() == turn_id
                && overlay.call_id() == call_id
                && overlay.has_child_thread(thread_id)
        }) else {
            return false;
        };
        self.overlay_mut(location)
            .seed_activity(thread_id, notifications)
    }

    pub(crate) fn overlay_key_for_child_thread(&self, thread_id: &str) -> Option<(String, String)> {
        let location =
            self.unique_overlay_location(|overlay| overlay.has_child_thread(thread_id))?;
        let overlay = self.overlay(location);
        Some((overlay.turn_id().to_string(), overlay.call_id().to_string()))
    }

    pub(crate) fn spawn_summary_for_child_thread(&self, thread_id: &str) -> Option<&str> {
        let location =
            self.unique_overlay_location(|overlay| overlay.has_child_thread(thread_id))?;
        self.overlay(location).summary_for_child_thread(thread_id)
    }

    pub(crate) fn is_activity_seeded(&self, turn_id: &str, call_id: &str, thread_id: &str) -> bool {
        self.unique_overlay_location(|overlay| {
            overlay.turn_id() == turn_id
                && overlay.call_id() == call_id
                && overlay.has_child_thread(thread_id)
        })
        .is_some_and(|location| self.overlay(location).has_activity(thread_id))
    }

    pub(crate) fn apply_activity(
        &mut self,
        turn_id: &str,
        call_id: &str,
        thread_id: &str,
        notification: &ServerNotification,
        status: Option<codex_app_server_protocol::CollabAgentStatus>,
    ) -> bool {
        let Some(location) = self.unique_overlay_location(|overlay| {
            overlay.turn_id() == turn_id
                && overlay.call_id() == call_id
                && overlay.has_child_thread(thread_id)
        }) else {
            return false;
        };
        self.overlay_mut(location)
            .update_activity(thread_id, notification, status)
    }

    pub(crate) fn update_status(
        &mut self,
        turn_id: &str,
        call_id: &str,
        thread_id: &str,
        status: codex_app_server_protocol::CollabAgentStatus,
    ) -> bool {
        let Some(location) = self.unique_overlay_location(|overlay| {
            overlay.turn_id() == turn_id
                && overlay.call_id() == call_id
                && overlay.has_child_thread(thread_id)
        }) else {
            return false;
        };
        self.overlay_mut(location).update_status(thread_id, status)
    }

    pub(crate) fn render_cell(&self) -> Option<SpineTreeUpdateCell> {
        if self.overlays.is_empty() && self.pending_handoff.is_none() {
            return None;
        }
        let snapshot = self
            .snapshot
            .clone()
            .or_else(|| self.live_spawn_snapshot())?;
        Some(SpineTreeUpdateCell {
            snapshot,
            display_mode: SpineTreeDisplayMode::Pretty,
            spawn_overlays: self.overlays.clone(),
            pending_handoff: self.pending_handoff.clone(),
            animations_enabled: self.animations_enabled,
            automatic_history: false,
        })
    }

    fn live_spawn_snapshot(&self) -> Option<SpineTreeUpdatedNotification> {
        let first = self.overlays.first()?;
        if self.overlays.iter().any(|overlay| {
            overlay.thread_id() != first.thread_id() || overlay.turn_id() != first.turn_id()
        }) {
            return None;
        }
        Some(SpineTreeUpdatedNotification {
            thread_id: first.thread_id().to_string(),
            turn_id: first.turn_id().to_string(),
            snapshot_seq: 0,
            active_node_id: LIVE_SPAWN_ROOT_ID.to_string(),
            nodes: vec![SpineTreeNode {
                node_id: LIVE_SPAWN_ROOT_ID.to_string(),
                parent_id: None,
                kind: SpineTreeNodeKind::RootEpoch,
                status: SpineTreeNodeStatus::Live,
                summary: None,
                memory_summary: None,
                spawn_outcome: None,
                start: 0,
                end: None,
                context_pressure: None,
            }],
            settled_spawn_call_ids: Vec::new(),
        })
    }

    pub(crate) fn snapshot_cell(&self) -> Option<SpineTreeUpdateCell> {
        let snapshot = self.snapshot.clone()?;
        Some(SpineTreeUpdateCell {
            snapshot,
            display_mode: SpineTreeDisplayMode::Pretty,
            spawn_overlays: Vec::new(),
            pending_handoff: None,
            animations_enabled: false,
            automatic_history: false,
        })
    }

    pub(crate) fn take_pending_history_cell(&mut self) -> Option<SpineTreeUpdateCell> {
        self.pending_history
            .take()
            .map(SpineTreeUpdateCell::automatic_history)
    }

    #[cfg(test)]
    pub(crate) fn has_pending_history(&self) -> bool {
        self.pending_history.is_some()
    }

    pub(crate) fn promote_due_handoff_to_pending(&mut self, now: Instant) -> bool {
        if self
            .pending_handoff
            .as_ref()
            .is_none_or(|pending| pending.activity_pending || now < pending.reveal_at)
        {
            return false;
        }
        self.pending_handoff = None;
        self.pending_history = self.snapshot.clone();
        true
    }

    pub(crate) fn set_pending_handoff_activity_pending(&mut self, activity_pending: bool) {
        if let Some(pending) = self.pending_handoff.as_mut() {
            pending.activity_pending = activity_pending;
        }
    }

    #[cfg(test)]
    pub(crate) fn make_pending_handoff_due(&mut self) {
        if let Some(handoff) = self.pending_handoff.as_mut() {
            handoff.reveal_at = Instant::now();
        }
    }

    pub(crate) fn take_due_handoff_history(&mut self, now: Instant) -> Option<SpineTreeUpdateCell> {
        if !self.promote_due_handoff_to_pending(now) {
            return None;
        }
        self.take_pending_history_cell()
    }

    #[cfg(test)]
    pub(crate) fn has_spawn_call(&self, call_id: &str) -> bool {
        self.overlays
            .iter()
            .any(|overlay| overlay.call_id() == call_id)
    }
}

fn display_tree_changed(
    previous: &SpineTreeUpdatedNotification,
    current: &SpineTreeUpdatedNotification,
) -> bool {
    previous.active_node_id != current.active_node_id
        || previous.nodes.len() != current.nodes.len()
        || previous
            .nodes
            .iter()
            .zip(&current.nodes)
            .any(|(left, right)| {
                left.node_id != right.node_id
                    || left.parent_id != right.parent_id
                    || left.kind != right.kind
                    || left.status != right.status
                    || left.summary != right.summary
                    || left.memory_summary != right.memory_summary
                    || left.spawn_outcome != right.spawn_outcome
                    || left.start != right.start
                    || left.end != right.end
            })
}

#[derive(Debug, Clone)]
pub(crate) struct SpineTreeUpdateCell {
    snapshot: SpineTreeUpdatedNotification,
    display_mode: SpineTreeDisplayMode,
    spawn_overlays: Vec<SpineSpawnOverlay>,
    pending_handoff: Option<PendingTreeHandoff>,
    animations_enabled: bool,
    automatic_history: bool,
}

#[derive(Debug, Clone)]
enum SpineTreeDisplayMode {
    Pretty,
    Debug(Option<String>),
}

impl HistoryCell for SpineTreeUpdateCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.display_lines_at(width, Instant::now())
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        match &self.display_mode {
            SpineTreeDisplayMode::Pretty => pretty_raw_lines(&self.snapshot),
            SpineTreeDisplayMode::Debug(node_id) => {
                debug::raw_lines(&self.snapshot, node_id.as_deref())
            }
        }
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if !self.animations_enabled {
            return None;
        }
        let now = Instant::now();
        let started_at = self
            .spawn_overlays
            .iter()
            .chain(
                self.pending_handoff
                    .iter()
                    .filter(|pending| now < pending.reveal_at)
                    .flat_map(|pending| &pending.overlays),
            )
            .map(SpineSpawnOverlay::animation_start)
            .min()?;
        Some(now.saturating_duration_since(started_at).as_millis() as u64 / 50)
    }
}

impl SpineTreeUpdateCell {
    fn automatic_history(snapshot: SpineTreeUpdatedNotification) -> Self {
        Self {
            snapshot,
            display_mode: SpineTreeDisplayMode::Pretty,
            spawn_overlays: Vec::new(),
            pending_handoff: None,
            animations_enabled: false,
            automatic_history: true,
        }
    }

    pub(crate) fn turn_id(&self) -> &str {
        &self.snapshot.turn_id
    }

    pub(crate) fn snapshot_seq(&self) -> u64 {
        self.snapshot.snapshot_seq
    }

    pub(crate) fn is_automatic_history(&self) -> bool {
        self.automatic_history
    }

    pub(crate) fn next_frame_in(&self, now: Instant) -> Option<Duration> {
        if !self.animations_enabled || !matches!(self.display_mode, SpineTreeDisplayMode::Pretty) {
            return None;
        }
        let pending = self
            .pending_handoff
            .as_ref()
            .filter(|handoff| handoff.activity_pending || now < handoff.reveal_at);
        self.spawn_overlays
            .iter()
            .chain(pending.into_iter().flat_map(|pending| &pending.overlays))
            .filter_map(|overlay| overlay.next_completion_frame_in(now))
            .chain(
                pending
                    .filter(|handoff| now < handoff.reveal_at)
                    .map(|handoff| handoff.reveal_at - now),
            )
            .min()
    }

    fn display_lines_at(&self, width: u16, now: Instant) -> Vec<Line<'static>> {
        match &self.display_mode {
            SpineTreeDisplayMode::Pretty => {
                let active_handoff = self
                    .pending_handoff
                    .as_ref()
                    .filter(|pending| pending.activity_pending || now < pending.reveal_at);
                let mut overlays = self.spawn_overlays.clone();
                if let Some(pending) = active_handoff {
                    overlays.extend_from_slice(&pending.overlays);
                }
                pretty_display_lines(
                    active_handoff.map_or(&self.snapshot, |pending| &pending.snapshot),
                    &overlays,
                    width,
                    self.animations_enabled,
                )
            }
            SpineTreeDisplayMode::Debug(node_id) => {
                debug::display_lines(&self.snapshot, width, node_id.as_deref())
            }
        }
    }
}

fn pretty_display_lines(
    snapshot: &SpineTreeUpdatedNotification,
    overlays: &[SpineSpawnOverlay],
    width: u16,
    animations_enabled: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![pretty_header(snapshot)];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(invalid_snapshot_display_line(error));
        return lines;
    }

    let root_nodes = visible_pretty_nodes(snapshot, &child_nodes(snapshot, None));
    let overlays_at_root = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id)
        .is_some_and(|node| {
            should_elide_pretty_node(
                node,
                !child_nodes(snapshot, Some(node.node_id.as_str())).is_empty(),
                true,
            )
        });
    if root_nodes.is_empty() && !(overlays_at_root && !overlays.is_empty()) {
        lines.push(
            vec![
                format!("  {}", pretty_branch(true)).dim(),
                "(empty)".dim().italic(),
            ]
            .into(),
        );
        return lines;
    }

    let active_path = active_path_ids(snapshot);
    render_pretty_nodes(
        snapshot,
        overlays,
        &root_nodes,
        &active_path,
        "  ",
        width,
        &mut lines,
        overlays_at_root && !overlays.is_empty(),
        animations_enabled,
    );
    if overlays_at_root {
        for (index, overlay) in overlays.iter().enumerate() {
            lines.extend(overlay.display_lines(
                "  ",
                index + 1 == overlays.len(),
                width,
                animations_enabled,
            ));
        }
    }
    lines
}

fn pretty_raw_lines(snapshot: &SpineTreeUpdatedNotification) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("Spine Tree")];
    if let Err(error) = validate_spine_tree_snapshot(snapshot) {
        lines.push(invalid_snapshot_raw_line(error));
        return lines;
    }

    let root_nodes = visible_pretty_nodes(snapshot, &child_nodes(snapshot, None));
    if root_nodes.is_empty() {
        lines.push(Line::from(format!("  {}(empty)", pretty_branch(true))));
        return lines;
    }

    let active_path = active_path_ids(snapshot);
    append_pretty_raw_nodes(snapshot, &root_nodes, &active_path, "  ", &mut lines);
    lines
}

fn pretty_header(_snapshot: &SpineTreeUpdatedNotification) -> Line<'static> {
    vec!["• ".dim(), "Spine Tree".fg(SPINE_BRAND_COLOR).bold()].into()
}
fn render_pretty_node(
    snapshot: &SpineTreeUpdatedNotification,
    overlays: &[SpineSpawnOverlay],
    node: &SpineTreeNode,
    active_path: &HashSet<&str>,
    prefix: &str,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
    animations_enabled: bool,
) {
    let children = child_nodes(snapshot, Some(node.node_id.as_str()));
    let active = node.node_id == snapshot.active_node_id;
    let line_prefix = format!("{}{}", prefix, pretty_branch(is_last));
    let child_prefix = format!("{}{}", prefix, pretty_child_prefix(is_last));
    let mut spans = vec![Span::from(line_prefix).dim()];
    spans.push(pretty_marker(node, active, !children.is_empty()));
    spans.push(" ".into());
    spans.push(Span::from(pretty_node_label_text(node, active)));

    let line = Line::from(spans);
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.saturating_sub(2).max(1) as usize)
            .subsequent_indent(Span::from(format!("{child_prefix}  ")).dim().into()),
    );
    push_owned_lines(&wrapped, out);

    if should_collapse_pretty_subtree(node, !children.is_empty(), active_path) {
        return;
    }

    let node_overlays = if active { overlays } else { &[] };
    render_pretty_nodes(
        snapshot,
        overlays,
        &children,
        active_path,
        &child_prefix,
        width,
        out,
        !node_overlays.is_empty(),
        animations_enabled,
    );
    for (index, overlay) in node_overlays.iter().enumerate() {
        out.extend(overlay.display_lines(
            &child_prefix,
            index + 1 == node_overlays.len(),
            width,
            animations_enabled,
        ));
    }
}

fn render_pretty_nodes(
    snapshot: &SpineTreeUpdatedNotification,
    overlays: &[SpineSpawnOverlay],
    nodes: &[&SpineTreeNode],
    active_path: &HashSet<&str>,
    prefix: &str,
    width: u16,
    out: &mut Vec<Line<'static>>,
    has_trailing_overlay: bool,
    animations_enabled: bool,
) {
    let items = pretty_render_items(snapshot, nodes, active_path);
    let item_count = items.len();
    for (index, item) in items.into_iter().enumerate() {
        let is_last = index + 1 == item_count && !has_trailing_overlay;
        match item {
            PrettySiblingItem::HistoryBucket(count) => {
                render_history_bucket(count, prefix, is_last, width, out);
            }
            PrettySiblingItem::Node(node) => {
                render_pretty_node(
                    snapshot,
                    overlays,
                    node,
                    active_path,
                    prefix,
                    is_last,
                    width,
                    out,
                    animations_enabled,
                );
            }
        }
    }
}

fn append_pretty_raw_nodes(
    snapshot: &SpineTreeUpdatedNotification,
    nodes: &[&SpineTreeNode],
    active_path: &HashSet<&str>,
    prefix: &str,
    out: &mut Vec<Line<'static>>,
) {
    let items = pretty_render_items(snapshot, nodes, active_path);
    let item_count = items.len();
    for (index, item) in items.into_iter().enumerate() {
        let is_last = index + 1 == item_count;
        match item {
            PrettySiblingItem::HistoryBucket(count) => out.push(Line::from(format!(
                "{}{}◌ {}",
                prefix,
                pretty_branch(is_last),
                history_bucket_label(count)
            ))),
            PrettySiblingItem::Node(node) => {
                let children = child_nodes(snapshot, Some(node.node_id.as_str()));
                let active = node.node_id == snapshot.active_node_id;
                let marker = pretty_marker_text(node, active, !children.is_empty());
                out.push(Line::from(format!(
                    "{}{}{} {}",
                    prefix,
                    pretty_branch(is_last),
                    marker,
                    pretty_node_label_text(node, active)
                )));
                if should_collapse_pretty_subtree(node, !children.is_empty(), active_path) {
                    continue;
                }
                let child_prefix = format!("{}{}", prefix, pretty_child_prefix(is_last));
                append_pretty_raw_nodes(snapshot, &children, active_path, &child_prefix, out);
            }
        }
    }
}

fn pretty_render_items<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    nodes: &[&'a SpineTreeNode],
    active_path: &HashSet<&str>,
) -> Vec<PrettySiblingItem<'a>> {
    let mut normalized_nodes = Vec::new();
    append_visible_pretty_nodes(snapshot, nodes, &mut normalized_nodes);
    pretty_sibling_items(&normalized_nodes, active_path)
}

fn visible_pretty_nodes<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    nodes: &[&'a SpineTreeNode],
) -> Vec<&'a SpineTreeNode> {
    let mut visible = Vec::new();
    append_visible_pretty_nodes(snapshot, nodes, &mut visible);
    visible
}

fn append_visible_pretty_nodes<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    nodes: &[&'a SpineTreeNode],
    out: &mut Vec<&'a SpineTreeNode>,
) {
    for node in nodes.iter().copied() {
        let children = child_nodes(snapshot, Some(node.node_id.as_str()));
        let active = node.node_id == snapshot.active_node_id;
        if should_elide_pretty_node(node, !children.is_empty(), active) {
            append_visible_pretty_nodes(snapshot, &children, out);
        } else {
            out.push(node);
        }
    }
}

enum PrettySiblingItem<'a> {
    HistoryBucket(usize),
    Node(&'a SpineTreeNode),
}

fn pretty_sibling_items<'a>(
    nodes: &[&'a SpineTreeNode],
    active_path: &HashSet<&str>,
) -> Vec<PrettySiblingItem<'a>> {
    let mut items = nodes
        .iter()
        .copied()
        .map(|node| {
            if bucketable_history_node(node, active_path) {
                PrettySiblingItem::HistoryBucket(1)
            } else {
                PrettySiblingItem::Node(node)
            }
        })
        .collect::<Vec<_>>();

    let active_index = nodes
        .iter()
        .position(|node| active_path.contains(node.node_id.as_str()));
    let visible_end = active_index.map_or(nodes.len(), |index| index + 1);
    if visible_end < nodes.len() {
        return merge_adjacent_history_buckets(items);
    };
    if nodes.len() <= PRETTY_MAX_VISIBLE_SIBLINGS {
        return merge_adjacent_history_buckets(items);
    }
    let visible_start = visible_end.saturating_sub(PRETTY_MAX_VISIBLE_SIBLINGS);

    let mut folded = Vec::new();
    if visible_start > 0 {
        let hidden_count = items[..visible_start]
            .iter()
            .map(pretty_sibling_item_history_count)
            .sum();
        folded.push(PrettySiblingItem::HistoryBucket(hidden_count));
    }
    folded.extend(items.drain(visible_start..visible_end));
    merge_adjacent_history_buckets(folded)
}

fn bucketable_history_node(node: &SpineTreeNode, active_path: &HashSet<&str>) -> bool {
    is_completed_history_node(node)
        && trimmed_summary(node).is_none()
        && !active_path.contains(node.node_id.as_str())
}

fn should_collapse_pretty_subtree(
    node: &SpineTreeNode,
    has_children: bool,
    active_path: &HashSet<&str>,
) -> bool {
    has_children && is_completed_history_node(node) && !active_path.contains(node.node_id.as_str())
}

fn is_completed_history_node(node: &SpineTreeNode) -> bool {
    matches!(
        node.status,
        SpineTreeNodeStatus::Closed | SpineTreeNodeStatus::Compacted
    )
}

fn pretty_sibling_item_history_count(item: &PrettySiblingItem<'_>) -> usize {
    match item {
        PrettySiblingItem::HistoryBucket(count) => *count,
        PrettySiblingItem::Node(_) => 1,
    }
}

fn merge_adjacent_history_buckets<'a>(
    items: Vec<PrettySiblingItem<'a>>,
) -> Vec<PrettySiblingItem<'a>> {
    let mut merged = Vec::with_capacity(items.len());
    for item in items {
        match item {
            PrettySiblingItem::HistoryBucket(count) => {
                if let Some(PrettySiblingItem::HistoryBucket(previous)) = merged.last_mut() {
                    *previous += count;
                } else {
                    merged.push(PrettySiblingItem::HistoryBucket(count));
                }
            }
            PrettySiblingItem::Node(node) => merged.push(PrettySiblingItem::Node(node)),
        }
    }
    merged
}

fn active_path_ids(snapshot: &SpineTreeUpdatedNotification) -> HashSet<&str> {
    let mut active_path = HashSet::new();
    let mut current = snapshot.active_node_id.as_str();
    active_path.insert(current);

    while let Some(node) = snapshot.nodes.iter().find(|node| node.node_id == current) {
        let Some(parent_id) = node.parent_id.as_deref() else {
            break;
        };
        if !active_path.insert(parent_id) {
            break;
        }
        current = parent_id;
    }

    active_path
}

fn pretty_marker(node: &SpineTreeNode, active: bool, has_children: bool) -> Span<'static> {
    match pretty_marker_text(node, active, has_children) {
        "◉" => "◉".cyan().bold(),
        "✓" => "✓".green().bold(),
        "×" => "×".red().bold(),
        "!" => "!".yellow().bold(),
        "▾" => "▾".dim(),
        "◌" => "◌".dim(),
        marker => Span::from(marker),
    }
}

fn pretty_marker_text(node: &SpineTreeNode, active: bool, has_children: bool) -> &'static str {
    if active {
        return "◉";
    }
    match node.spawn_outcome {
        Some(SpineSpawnOutcome::Completed) => return "✓",
        Some(SpineSpawnOutcome::Errored) => return "×",
        Some(SpineSpawnOutcome::Aborted) => return "!",
        None => {}
    }
    match node.status {
        SpineTreeNodeStatus::Live => "◉",
        SpineTreeNodeStatus::Closed => "✓",
        SpineTreeNodeStatus::Compacted => "◌",
        SpineTreeNodeStatus::Opened if has_children => "▾",
        SpineTreeNodeStatus::Opened => "◌",
    }
}

fn render_history_bucket(
    count: usize,
    prefix: &str,
    is_last: bool,
    width: u16,
    out: &mut Vec<Line<'static>>,
) {
    let line_prefix = format!("{}{}", prefix, pretty_branch(is_last));
    let child_prefix = format!("{}{}", prefix, pretty_child_prefix(is_last));
    let line = Line::from(vec![
        Span::from(line_prefix).dim(),
        "◌".dim(),
        " ".into(),
        Span::from(count.to_string()).green(),
        " previous ".green(),
        Span::from(history_bucket_noun(count)).green(),
    ]);
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(width.saturating_sub(2).max(1) as usize)
            .subsequent_indent(Span::from(format!("{child_prefix}  ")).dim().into()),
    );
    push_owned_lines(&wrapped, out);
}

fn history_bucket_label(count: usize) -> String {
    format!("{count} previous {}", history_bucket_noun(count))
}

fn history_bucket_noun(count: usize) -> &'static str {
    if count == 1 { "leaf" } else { "leaves" }
}

fn pretty_node_label_text(node: &SpineTreeNode, active: bool) -> String {
    trimmed_summary(node)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| pretty_default_node_label(node, active).to_string())
}

fn trimmed_summary(node: &SpineTreeNode) -> Option<&str> {
    node.summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn should_elide_pretty_node(node: &SpineTreeNode, has_children: bool, active: bool) -> bool {
    node.kind == SpineTreeNodeKind::RootEpoch
        || (has_children
            && !active
            && trimmed_summary(node).is_none()
            && !is_completed_history_node(node))
}

fn pretty_default_node_label(node: &SpineTreeNode, active: bool) -> &'static str {
    if active || node.status == SpineTreeNodeStatus::Live {
        return "Current task";
    }
    match node.status {
        SpineTreeNodeStatus::Live => "Current task",
        SpineTreeNodeStatus::Opened => "Task",
        SpineTreeNodeStatus::Closed => "Completed task",
        SpineTreeNodeStatus::Compacted => "Previous task",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpineTreeSnapshotValidationError {
    DuplicateNodeId,
    MissingActiveNode,
    MissingParent,
    ParentCycle,
}

impl SpineTreeSnapshotValidationError {
    fn label(self) -> &'static str {
        match self {
            SpineTreeSnapshotValidationError::DuplicateNodeId => "duplicate node id",
            SpineTreeSnapshotValidationError::MissingActiveNode => "missing active node",
            SpineTreeSnapshotValidationError::MissingParent => "missing parent node",
            SpineTreeSnapshotValidationError::ParentCycle => "parent cycle",
        }
    }
}

fn validate_spine_tree_snapshot(
    snapshot: &SpineTreeUpdatedNotification,
) -> Result<(), SpineTreeSnapshotValidationError> {
    if snapshot.nodes.is_empty() {
        return Ok(());
    }

    let mut node_ids = HashSet::new();
    for node in &snapshot.nodes {
        if !node_ids.insert(node.node_id.as_str()) {
            return Err(SpineTreeSnapshotValidationError::DuplicateNodeId);
        }
    }

    if !node_ids.contains(snapshot.active_node_id.as_str()) {
        return Err(SpineTreeSnapshotValidationError::MissingActiveNode);
    }

    for node in &snapshot.nodes {
        if let Some(parent_id) = node.parent_id.as_deref()
            && !node_ids.contains(parent_id)
        {
            return Err(SpineTreeSnapshotValidationError::MissingParent);
        }
    }

    for node in &snapshot.nodes {
        let mut seen = HashSet::new();
        let mut current_id = Some(node.node_id.as_str());
        while let Some(node_id) = current_id {
            if !seen.insert(node_id) {
                return Err(SpineTreeSnapshotValidationError::ParentCycle);
            }
            current_id = snapshot
                .nodes
                .iter()
                .find(|candidate| candidate.node_id == node_id)
                .and_then(|candidate| candidate.parent_id.as_deref());
        }
    }

    Ok(())
}

fn invalid_snapshot_display_line(error: SpineTreeSnapshotValidationError) -> Line<'static> {
    vec![
        format!("  {}", pretty_branch(true)).dim(),
        Span::from(invalid_snapshot_message(error)).red().bold(),
    ]
    .into()
}

fn invalid_snapshot_raw_line(error: SpineTreeSnapshotValidationError) -> Line<'static> {
    Line::from(format!(
        "  {}{}",
        pretty_branch(true),
        invalid_snapshot_message(error)
    ))
}

fn invalid_snapshot_message(error: SpineTreeSnapshotValidationError) -> String {
    format!("{INVALID_SPINE_TREE_SNAPSHOT_LABEL}: {}", error.label())
}

fn child_nodes<'a>(
    snapshot: &'a SpineTreeUpdatedNotification,
    parent_id: Option<&str>,
) -> Vec<&'a SpineTreeNode> {
    snapshot
        .nodes
        .iter()
        .filter(|node| node.parent_id.as_deref() == parent_id)
        .collect()
}

fn pretty_branch(is_last: bool) -> &'static str {
    if is_last { "└ " } else { "├ " }
}

fn pretty_child_prefix(is_last: bool) -> &'static str {
    if is_last { "  " } else { "│ " }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn snapshot(active_node_id: &str, nodes: Vec<SpineTreeNode>) -> SpineTreeUpdatedNotification {
        SpineTreeUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            snapshot_seq: 1,
            active_node_id: active_node_id.to_string(),
            nodes,
            settled_spawn_call_ids: Vec::new(),
        }
    }

    fn node(
        node_id: &str,
        parent_id: Option<&str>,
        summary: Option<&str>,
        status: SpineTreeNodeStatus,
    ) -> SpineTreeNode {
        SpineTreeNode {
            node_id: node_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            kind: SpineTreeNodeKind::Task,
            status,
            summary: summary.map(str::to_string),
            memory_summary: None,
            start: 0,
            end: None,
            context_pressure: None,
            spawn_outcome: None,
        }
    }

    fn spawn_progress(
        call_id: &str,
        tasks: &[(&str, &str, codex_app_server_protocol::CollabAgentStatus)],
    ) -> SpineSpawnProgressUpdatedNotification {
        SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: call_id.to_string(),
            tasks: tasks
                .iter()
                .enumerate()
                .map(|(ordinal, (thread_id, summary, status))| {
                    codex_app_server_protocol::SpineSpawnTaskProgress {
                        ordinal: ordinal as u32,
                        summary: (*summary).to_string(),
                        thread_id: (*thread_id).to_string(),
                        agent_path: Some(format!("/root/{thread_id}")),
                        status: status.clone(),
                    }
                })
                .collect(),
        }
    }

    fn identified_spawn_progress(
        turn_id: &str,
        call_id: &str,
        tasks: &[(u32, &str)],
        status: codex_app_server_protocol::CollabAgentStatus,
    ) -> SpineSpawnProgressUpdatedNotification {
        SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: turn_id.to_string(),
            call_id: call_id.to_string(),
            tasks: tasks
                .iter()
                .map(
                    |(ordinal, thread_id)| codex_app_server_protocol::SpineSpawnTaskProgress {
                        ordinal: *ordinal,
                        summary: format!("task {thread_id}"),
                        thread_id: (*thread_id).to_string(),
                        agent_path: Some(format!("/root/{thread_id}")),
                        status: status.clone(),
                    },
                )
                .collect(),
        }
    }

    fn root_epoch(
        node_id: &str,
        summary: Option<&str>,
        status: SpineTreeNodeStatus,
    ) -> SpineTreeNode {
        let mut node = node(node_id, None, summary, status);
        node.kind = SpineTreeNodeKind::RootEpoch;
        node
    }

    #[test]
    fn spawn_progress_renders_before_the_authoritative_tree_arrives() {
        let mut state = SpineTreeViewState::new(false);
        state.apply_spawn_progress(spawn_progress(
            "spawn-live",
            &[(
                "child-live",
                "visible before baseline",
                codex_app_server_protocol::CollabAgentStatus::Running,
            )],
        ));
        assert!(state.overlays[0].set_activity_word_for_test("child-live", "Blooming"));

        assert!(state.snapshot().is_none());
        let rendered = render(
            &state
                .render_cell()
                .expect("spawn progress should have a live-only presentation")
                .display_lines(80),
        );
        assert!(rendered.contains("Spine Tree"), "{rendered}");
        assert!(rendered.contains("visible before baseline"), "{rendered}");
        assert!(!rendered.contains("(empty)"), "{rendered}");
        insta::assert_snapshot!("live_spawn_without_authoritative_tree", rendered);
    }

    #[test]
    fn first_authoritative_settlement_preserves_the_live_only_handoff() {
        let mut state = SpineTreeViewState::new(false);
        state.apply_spawn_progress(spawn_progress(
            "spawn-live",
            &[(
                "child-live",
                "visible before baseline",
                codex_app_server_protocol::CollabAgentStatus::Completed,
            )],
        ));
        assert!(state.overlays[0].set_activity_word_for_test("child-live", "Blooming"));
        let mut committed = snapshot(
            "1",
            vec![
                root_epoch("1", Some("root"), SpineTreeNodeStatus::Live),
                node(
                    "1.1",
                    Some("1"),
                    Some("visible after settlement"),
                    SpineTreeNodeStatus::Closed,
                ),
            ],
        );
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-live".to_string()];

        state.apply_tree_update_awaiting_terminal_activity(committed);

        assert_eq!(
            state.snapshot().map(|snapshot| snapshot.snapshot_seq),
            Some(2)
        );
        let rendered = render(
            &state
                .render_cell()
                .expect("terminal activity barrier should retain the live handoff")
                .display_lines(80),
        );
        assert!(rendered.contains("visible before baseline"), "{rendered}");
        insta::assert_snapshot!("live_spawn_settlement_handoff", rendered);

        state.set_pending_handoff_activity_pending(/*activity_pending*/ false);
        state.make_pending_handoff_due();
        assert!(state.promote_due_handoff_to_pending(Instant::now()));
        let history = render(
            &state
                .take_pending_history_cell()
                .expect("authoritative settlement should become history")
                .display_lines(80),
        );
        assert!(history.contains("visible after settlement"), "{history}");
        insta::assert_snapshot!("settled_spawn_authoritative_history", history);
    }

    #[test]
    fn renders_pretty_hierarchy_and_active_path() {
        let cell = new_spine_tree_snapshot(snapshot(
            "2.1",
            vec![
                node("1", None, Some("earlier work"), SpineTreeNodeStatus::Closed),
                node(
                    "2",
                    None,
                    Some("current scope"),
                    SpineTreeNodeStatus::Opened,
                ),
                node(
                    "2.1",
                    Some("2"),
                    Some("focused task"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        ));

        insta::assert_snapshot!(render(&cell.display_lines(80)), @r###"
        • Spine Tree
          ├ ✓ earlier work
          └ ▾ current scope
            └ ◉ focused task
        "###);
    }

    #[test]
    fn renders_pretty_header_in_spine_brand_color() {
        let header = pretty_header(&snapshot(
            "1",
            vec![node(
                "1",
                None,
                Some("current task"),
                SpineTreeNodeStatus::Live,
            )],
        ));
        let title = &header.spans[1];

        assert_eq!(title.content.as_ref(), "Spine Tree");
        assert_eq!(title.style.fg, Some(SPINE_BRAND_COLOR));
        assert!(title.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn folds_older_siblings_and_elides_empty_structural_nodes() {
        let cell = new_spine_tree_snapshot(snapshot(
            "3.3",
            vec![
                node("1", None, Some("old root 1"), SpineTreeNodeStatus::Closed),
                node("2", None, Some("old root 2"), SpineTreeNodeStatus::Closed),
                node("3", None, None, SpineTreeNodeStatus::Opened),
                node(
                    "3.1",
                    Some("3"),
                    Some("child 1"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "3.2",
                    Some("3"),
                    Some("child 2"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "3.3",
                    Some("3"),
                    Some("active child"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        ));

        let lines = cell.display_lines(80);
        let rendered = render(&lines);
        insta::assert_snapshot!(rendered, @r###"
        • Spine Tree
          ├ ◌ 2 previous leaves
          ├ ✓ child 1
          ├ ✓ child 2
          └ ◉ active child
        "###);
        let history_count = lines[1]
            .spans
            .iter()
            .find(|span| span.content == "2")
            .expect("history bucket count");
        assert_eq!(history_count.style.fg, Some(Color::Green));
        assert!(!history_count.style.add_modifier.contains(Modifier::BOLD));
        assert!(!history_count.style.add_modifier.contains(Modifier::DIM));
        let history_previous = lines[1]
            .spans
            .iter()
            .find(|span| span.content == " previous ")
            .expect("history bucket previous label");
        assert_eq!(history_previous.style.fg, Some(Color::Green));
        assert!(!history_previous.style.add_modifier.contains(Modifier::BOLD));
        assert!(!history_previous.style.add_modifier.contains(Modifier::DIM));
        let history_noun = lines[1]
            .spans
            .iter()
            .find(|span| span.content == "leaves")
            .expect("history bucket noun");
        assert_eq!(history_noun.style.fg, Some(Color::Green));
        assert!(!history_noun.style.add_modifier.contains(Modifier::BOLD));
        assert!(!history_noun.style.add_modifier.contains(Modifier::DIM));
        assert!(render(&cell.raw_lines()).contains("2 previous leaves"));
        assert!(!rendered.contains("old root"));
        assert!(!rendered.contains("3 "));
    }

    #[test]
    fn collapses_completed_parent_subtrees_after_root_epoch_promotion() {
        let snapshot = snapshot(
            "2.1",
            vec![
                root_epoch("1", Some("root"), SpineTreeNodeStatus::Compacted),
                node(
                    "1.1",
                    Some("1"),
                    Some("compacted parent"),
                    SpineTreeNodeStatus::Compacted,
                ),
                node(
                    "1.1.1",
                    Some("1.1"),
                    Some("hidden compacted child"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "1.2",
                    Some("1"),
                    Some("closed parent"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "1.2.1",
                    Some("1.2"),
                    Some("hidden closed child"),
                    SpineTreeNodeStatus::Closed,
                ),
                root_epoch("2", Some("root"), SpineTreeNodeStatus::Opened),
                node(
                    "2.1",
                    Some("2"),
                    Some("active task"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );
        let pretty = new_spine_tree_snapshot(snapshot.clone());

        insta::assert_snapshot!(render(&pretty.display_lines(80)), @r###"
        • Spine Tree
          ├ ◌ compacted parent
          ├ ✓ closed parent
          └ ◉ active task
        "###);
        let raw = render(&pretty.raw_lines());
        assert!(!raw.contains("hidden compacted child"), "{raw}");
        assert!(!raw.contains("hidden closed child"), "{raw}");

        let debug = render(&new_debug_spine_tree_snapshot(snapshot).display_lines(80));
        assert!(debug.contains("hidden compacted child"), "{debug}");
        assert!(debug.contains("hidden closed child"), "{debug}");
    }

    #[test]
    fn folds_anonymous_completed_parent_as_one_previous_leaf() {
        let cell = new_spine_tree_snapshot(snapshot(
            "2.1",
            vec![
                root_epoch("1", Some("root"), SpineTreeNodeStatus::Compacted),
                node("1.1", Some("1"), None, SpineTreeNodeStatus::Compacted),
                node(
                    "1.1.1",
                    Some("1.1"),
                    Some("hidden historical child"),
                    SpineTreeNodeStatus::Closed,
                ),
                root_epoch("2", Some("root"), SpineTreeNodeStatus::Opened),
                node(
                    "2.1",
                    Some("2"),
                    Some("active task"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        ));

        let lines = cell.display_lines(80);
        insta::assert_snapshot!(render(&lines), @r###"
        • Spine Tree
          ├ ◌ 1 previous leaf
          └ ◉ active task
        "###);
        let history_noun = lines[1]
            .spans
            .iter()
            .find(|span| span.content == "leaf")
            .expect("history bucket noun");
        assert_eq!(history_noun.style.fg, Some(Color::Green));
        assert!(!history_noun.style.add_modifier.contains(Modifier::BOLD));
        assert!(!history_noun.style.add_modifier.contains(Modifier::DIM));
        let history_previous = lines[1]
            .spans
            .iter()
            .find(|span| span.content == " previous ")
            .expect("history bucket previous label");
        assert_eq!(history_previous.style.fg, Some(Color::Green));
        assert!(!history_previous.style.add_modifier.contains(Modifier::BOLD));
        assert!(!history_previous.style.add_modifier.contains(Modifier::DIM));
        let history_count = lines[1]
            .spans
            .iter()
            .find(|span| span.content == "1")
            .expect("history bucket count");
        assert_eq!(history_count.style.fg, Some(Color::Green));
        assert!(!history_count.style.add_modifier.contains(Modifier::BOLD));
        assert!(!history_count.style.add_modifier.contains(Modifier::DIM));
        let raw = render(&cell.raw_lines());
        assert!(raw.contains("1 previous leaf"), "{raw}");
        assert!(!raw.contains("hidden historical child"));
    }

    #[test]
    fn hides_root_epochs_and_promotes_their_tasks_in_display_and_raw() {
        let cell = new_spine_tree_snapshot(snapshot(
            "3.2",
            vec![
                root_epoch("1", Some("root"), SpineTreeNodeStatus::Closed),
                node(
                    "1.1",
                    Some("1"),
                    Some("first task"),
                    SpineTreeNodeStatus::Closed,
                ),
                root_epoch("2", Some("root"), SpineTreeNodeStatus::Closed),
                node(
                    "2.1",
                    Some("2"),
                    Some("second task"),
                    SpineTreeNodeStatus::Closed,
                ),
                root_epoch("3", Some("root"), SpineTreeNodeStatus::Opened),
                node(
                    "3.1",
                    Some("3"),
                    Some("current scope"),
                    SpineTreeNodeStatus::Opened,
                ),
                node(
                    "3.2",
                    Some("3.1"),
                    Some("active task"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        ));

        let display = render(&cell.display_lines(80));
        insta::assert_snapshot!(display, @r###"
        • Spine Tree
          ├ ✓ first task
          ├ ✓ second task
          └ ▾ current scope
            └ ◉ active task
        "###);
        assert!(!display.contains("root"));

        let raw = render(&cell.raw_lines());
        assert!(!raw.contains("root"));
        assert!(raw.contains("first task"));
        assert!(raw.contains("active task"));
    }

    #[test]
    fn root_epoch_only_snapshot_renders_empty_pretty_tree() {
        let cell = new_spine_tree_snapshot(snapshot(
            "1",
            vec![root_epoch("1", Some("root"), SpineTreeNodeStatus::Live)],
        ));

        insta::assert_snapshot!(render(&cell.display_lines(80)), @r###"
        • Spine Tree
          └ (empty)
        "###);
        insta::assert_snapshot!(render(&cell.raw_lines()), @r###"
        Spine Tree
          └ (empty)
        "###);
    }

    #[test]
    fn debug_tree_keeps_root_epoch_structure() {
        let cell = new_debug_spine_tree_snapshot(snapshot(
            "1",
            vec![root_epoch("1", Some("root"), SpineTreeNodeStatus::Live)],
        ));

        let rendered = render(&cell.display_lines(80));
        assert!(rendered.contains("Debug Spine Tree"));
        assert!(rendered.contains("1 root current"));
    }

    #[test]
    fn wraps_long_summary_using_tree_indent() {
        let cell = new_spine_tree_snapshot(snapshot(
            "1",
            vec![node(
                "1",
                None,
                Some("a summary that is deliberately long enough to wrap"),
                SpineTreeNodeStatus::Live,
            )],
        ));

        let lines = cell.display_lines(24);
        assert!(lines.len() > 2);
        assert!(render(&lines).contains("  └ ◉ "));
        assert!(
            lines[2].spans[0].style.add_modifier.contains(Modifier::DIM),
            "wrapped tree prefix should retain the tree line style: {lines:?}"
        );
    }

    #[test]
    fn reports_invalid_parent_snapshot_without_panicking() {
        let cell = new_spine_tree_snapshot(snapshot(
            "1",
            vec![SpineTreeNode {
                node_id: "1".to_string(),
                parent_id: Some("missing".to_string()),
                kind: SpineTreeNodeKind::Task,
                status: SpineTreeNodeStatus::Live,
                summary: None,
                memory_summary: None,
                start: 0,
                end: None,
                context_pressure: None,
                spawn_outcome: None,
            }],
        ));

        assert!(
            render(&cell.display_lines(80))
                .contains("invalid Spine tree snapshot: missing parent node")
        );
    }

    #[test]
    fn spawn_outcome_controls_the_final_closed_leaf_marker() {
        let mut completed = node(
            "1.1",
            Some("1"),
            Some("completed"),
            SpineTreeNodeStatus::Closed,
        );
        completed.spawn_outcome = Some(SpineSpawnOutcome::Completed);
        let mut errored = completed.clone();
        errored.node_id = "1.2".to_string();
        errored.spawn_outcome = Some(SpineSpawnOutcome::Errored);
        let mut aborted = completed.clone();
        aborted.node_id = "1.3".to_string();
        aborted.spawn_outcome = Some(SpineSpawnOutcome::Aborted);

        assert_eq!(pretty_marker_text(&completed, false, false), "✓");
        assert_eq!(pretty_marker_text(&errored, false, false), "×");
        assert_eq!(pretty_marker_text(&aborted, false, false), "!");
    }

    #[test]
    fn active_root_epoch_promotes_spawn_overlay_into_visible_forest() {
        for closed_children in [false, true] {
            let mut root = node("root", None, None, SpineTreeNodeStatus::Live);
            root.kind = SpineTreeNodeKind::RootEpoch;
            let mut nodes = vec![root];
            if closed_children {
                nodes.push(node(
                    "root.1",
                    Some("root"),
                    Some("previous work"),
                    SpineTreeNodeStatus::Closed,
                ));
            }
            let mut state = SpineTreeViewState::default();
            state.apply_tree_update(snapshot("root", nodes));
            state.apply_spawn_progress(SpineSpawnProgressUpdatedNotification {
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                call_id: "spawn-root".to_string(),
                tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                    ordinal: 0,
                    summary: "root worker".to_string(),
                    thread_id: "child-root".to_string(),
                    agent_path: Some("/root/worker".to_string()),
                    status: codex_app_server_protocol::CollabAgentStatus::Running,
                }],
            });

            let rendered = render(
                &state
                    .render_cell()
                    .expect("tree snapshot should render")
                    .display_lines(80),
            );
            assert!(rendered.contains("root worker"), "{rendered}");
            assert!(!rendered.contains("leaf 0"), "{rendered}");
            assert!(rendered.contains("Waiting for activity..."), "{rendered}");
            if closed_children {
                assert!(rendered.contains("previous work"), "{rendered}");
            } else {
                assert!(!rendered.contains("(empty)"), "{rendered}");
            }
        }
    }

    #[test]
    fn live_spawn_overlay_ticks_while_snapshot_copy_stays_static() {
        let mut state = SpineTreeViewState::new(true);
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("active"), SpineTreeNodeStatus::Live)],
        ));
        state.apply_spawn_progress(SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: "spawn-1".to_string(),
            tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "animated worker".to_string(),
                thread_id: "child-1".to_string(),
                agent_path: Some("/root/worker".to_string()),
                status: codex_app_server_protocol::CollabAgentStatus::PendingInit,
            }],
        });

        let live = state.render_cell().expect("live tree should render");
        let snapshot = state.snapshot_cell().expect("snapshot should render");

        assert!(live.transcript_animation_tick().is_some());
        assert_eq!(snapshot.transcript_animation_tick(), None);
        assert!(
            live.display_lines(80)
                .iter()
                .any(|line| line.to_string().contains("animated worker"))
        );
        assert!(
            snapshot
                .display_lines(80)
                .iter()
                .all(|line| !line.to_string().contains("animated worker"))
        );
    }

    #[test]
    fn live_tail_keeps_tree_static_while_spawn_overlay_animates() {
        let mut state = SpineTreeViewState::new(true);
        state.apply_tree_update(snapshot(
            "1.1",
            vec![
                node(
                    "1",
                    None,
                    Some("static parent"),
                    SpineTreeNodeStatus::Opened,
                ),
                node(
                    "1.1",
                    Some("1"),
                    Some("working summary"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        ));
        state.apply_spawn_progress(SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: "spawn-1".to_string(),
            tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "child".to_string(),
                thread_id: "child-1".to_string(),
                agent_path: Some("/root/child".to_string()),
                status: codex_app_server_protocol::CollabAgentStatus::PendingInit,
            }],
        });

        let live = state.render_cell().expect("live tree should render");
        assert!(live.transcript_animation_tick().is_some());
        let lines = live.display_lines(80);
        let active_line = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("working summary"))
            })
            .expect("active node line");
        let marker = active_line
            .spans
            .iter()
            .find(|span| span.content == "◉")
            .expect("active marker");
        let summary = active_line
            .spans
            .iter()
            .find(|span| span.content.contains("working summary"))
            .expect("active summary");

        assert_eq!(marker.style.fg, Some(Color::Cyan));
        assert!(marker.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(summary.style, Style::default());

        let snapshot = state.snapshot_cell().expect("snapshot should render");
        let static_lines = snapshot.display_lines(80);
        let static_active = static_lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("working summary"))
            })
            .expect("static active node line");
        let static_marker = static_active
            .spans
            .iter()
            .find(|span| span.content == "◉")
            .expect("static active marker");

        assert_eq!(static_marker.style.fg, Some(Color::Cyan));
        assert!(static_marker.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn initial_snapshot_alone_does_not_create_a_live_tail() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("active"), SpineTreeNodeStatus::Live)],
        ));

        assert!(state.render_cell().is_none());
        assert!(state.snapshot_cell().is_some());
        assert!(state.take_pending_history_cell().is_none());
    }

    #[test]
    fn display_tree_change_does_not_create_a_live_tail_without_an_overlay() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));

        let mut changed = snapshot(
            "1.1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Opened),
                node(
                    "1.1",
                    Some("1"),
                    Some("nested task"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );
        changed.snapshot_seq = 2;
        state.apply_tree_update(changed);

        assert!(
            state.render_cell().is_none(),
            "an ordinary tree edge belongs in history, not the bottom live tail"
        );
        let history = state
            .take_pending_history_cell()
            .expect("the ordinary edge should emit one history effect");
        assert!(history.is_automatic_history());
        assert!(
            render(&history.display_lines(80)).contains("nested task"),
            "history must capture the accepted semantic edge"
        );
        assert!(
            state.take_pending_history_cell().is_none(),
            "the edge effect must be consumed exactly once"
        );
    }

    #[test]
    fn automatic_history_uses_the_same_renderer_as_an_explicit_snapshot() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));
        let mut changed = snapshot(
            "1.1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Opened),
                node(
                    "1.1",
                    Some("1"),
                    Some("renderer equivalence"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );
        changed.snapshot_seq = 2;
        state.apply_tree_update(changed);

        let automatic = state
            .take_pending_history_cell()
            .expect("automatic history");
        let explicit = state.snapshot_cell().expect("explicit snapshot");
        for width in [20, 80] {
            assert_eq!(
                automatic.display_lines(width),
                explicit.display_lines(width)
            );
        }
        assert_eq!(automatic.raw_lines(), explicit.raw_lines());
        assert!(automatic.is_automatic_history());
        assert!(!explicit.is_automatic_history());
    }

    #[test]
    fn projection_only_updates_do_not_create_a_live_tail() {
        let mut state = SpineTreeViewState::default();
        let initial = snapshot(
            "1",
            vec![node("1", None, Some("active"), SpineTreeNodeStatus::Live)],
        );
        state.apply_tree_update(initial.clone());

        let mut projection_only = initial;
        projection_only.turn_id = "later-turn".to_string();
        projection_only.snapshot_seq = 2;
        projection_only.settled_spawn_call_ids = vec!["settled-call".to_string()];
        projection_only.nodes[0].context_pressure =
            Some(codex_app_server_protocol::SpineNodeContextPressure {
                open_input_tokens: Some(100),
                current_input_tokens: Some(200),
                context_tokens: Some(300),
                problem: None,
            });
        state.apply_tree_update(projection_only);

        assert!(state.render_cell().is_none());
        assert!(state.take_pending_history_cell().is_none());
    }

    #[test]
    fn projection_only_updates_do_not_restore_a_consumed_tree_edge() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));

        let mut changed = snapshot(
            "1.1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Opened),
                node(
                    "1.1",
                    Some("1"),
                    Some("nested task"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );
        changed.snapshot_seq = 2;
        state.apply_tree_update(changed.clone());

        changed.turn_id = "status-followup".to_string();
        changed.snapshot_seq = 3;
        changed.nodes[1].context_pressure =
            Some(codex_app_server_protocol::SpineNodeContextPressure {
                open_input_tokens: Some(100),
                current_input_tokens: Some(200),
                context_tokens: Some(300),
                problem: None,
            });
        state.apply_tree_update(changed);

        assert!(
            state.render_cell().is_none(),
            "a projection-only update must not restore bottom live ownership"
        );
        let history = state
            .take_pending_history_cell()
            .expect("the prior semantic edge should remain pending once");
        assert_eq!(
            history.snapshot_seq(),
            2,
            "projection-only updates must not rewrite the edge-time presentation"
        );
        assert_eq!(history.turn_id(), "turn");
        assert!(render(&history.display_lines(80)).contains("nested task"));
        assert!(state.take_pending_history_cell().is_none());
    }

    #[test]
    fn stale_tree_change_does_not_create_a_live_tail() {
        let mut state = SpineTreeViewState::default();
        let mut current = snapshot(
            "1",
            vec![node("1", None, Some("current"), SpineTreeNodeStatus::Live)],
        );
        current.snapshot_seq = 2;
        state.apply_tree_update(current);

        let stale = snapshot(
            "1.1",
            vec![
                node("1", None, Some("current"), SpineTreeNodeStatus::Opened),
                node(
                    "1.1",
                    Some("1"),
                    Some("stale task"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );
        state.apply_tree_update(stale);

        assert!(state.render_cell().is_none());
        assert_eq!(
            state
                .snapshot()
                .map(|snapshot| snapshot.active_node_id.as_str()),
            Some("1")
        );
        assert!(state.take_pending_history_cell().is_none());
    }

    #[test]
    fn spawn_outcome_change_creates_history_without_a_live_tail() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node(
                "1",
                None,
                Some("spawn result"),
                SpineTreeNodeStatus::Closed,
            )],
        ));

        let mut changed = snapshot(
            "1",
            vec![node(
                "1",
                None,
                Some("spawn result"),
                SpineTreeNodeStatus::Closed,
            )],
        );
        changed.snapshot_seq = 2;
        changed.nodes[0].spawn_outcome = Some(SpineSpawnOutcome::Completed);
        state.apply_tree_update(changed);

        assert!(state.render_cell().is_none());
        assert!(state.take_pending_history_cell().is_some());
    }

    #[test]
    fn settled_spawn_without_animation_commits_history() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));
        state.apply_spawn_progress(SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: "spawn-1".to_string(),
            tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "worker".to_string(),
                thread_id: "child-1".to_string(),
                agent_path: Some("/root/worker".to_string()),
                status: codex_app_server_protocol::CollabAgentStatus::Completed,
            }],
        });

        let mut committed = snapshot(
            "1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                node(
                    "1.1",
                    Some("1"),
                    Some("worker"),
                    SpineTreeNodeStatus::Closed,
                ),
            ],
        );
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-1".to_string()];
        state.apply_tree_update(committed);

        assert!(!state.has_spawn_call("spawn-1"));
        assert!(state.render_cell().is_none());
        let rendered = render(
            &state
                .take_pending_history_cell()
                .expect("settlement should commit visible history")
                .display_lines(80),
        );
        assert!(rendered.contains("worker"), "{rendered}");
    }

    #[test]
    fn settled_spawn_handoff_installs_authority_before_pretty_reveal() {
        let start = Instant::now();
        let mut state = SpineTreeViewState::new(true);
        state.apply_tree_update_at(
            snapshot(
                "1",
                vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
            ),
            start,
            /*await_terminal_activity*/ false,
        );
        state.apply_spawn_progress(spawn_progress(
            "spawn-animated",
            &[
                (
                    "child-animated",
                    "retiring worker",
                    codex_app_server_protocol::CollabAgentStatus::Completed,
                ),
                (
                    "child-errored",
                    "errored worker",
                    codex_app_server_protocol::CollabAgentStatus::Errored,
                ),
                (
                    "child-aborted",
                    "aborted worker",
                    codex_app_server_protocol::CollabAgentStatus::Shutdown,
                ),
            ],
        ));

        let settle_now = Instant::now();
        let mut committed = snapshot(
            "1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                node(
                    "1.1",
                    Some("1"),
                    Some("imported worker"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "1.2",
                    Some("1"),
                    Some("imported error"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "1.3",
                    Some("1"),
                    Some("imported abort"),
                    SpineTreeNodeStatus::Closed,
                ),
            ],
        );
        committed.nodes[1].spawn_outcome = Some(SpineSpawnOutcome::Completed);
        committed.nodes[2].spawn_outcome = Some(SpineSpawnOutcome::Errored);
        committed.nodes[3].spawn_outcome = Some(SpineSpawnOutcome::Aborted);
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-animated".to_string()];
        state.apply_tree_update_at(
            committed.clone(),
            settle_now,
            /*await_terminal_activity*/ false,
        );

        assert_eq!(state.snapshot(), Some(&committed));
        let pending = state
            .pending_handoff
            .clone()
            .expect("matching settlement should retain the presentation");
        assert_eq!(pending.snapshot.snapshot_seq, 1);

        let cell = state.render_cell().expect("live presentation");
        assert!(cell.next_frame_in(settle_now).is_some());
        let active_pretty = render(&cell.display_lines_at(80, settle_now));
        assert!(active_pretty.contains("retiring worker"), "{active_pretty}");
        assert!(active_pretty.contains("errored worker"), "{active_pretty}");
        assert!(active_pretty.contains("aborted worker"), "{active_pretty}");
        assert!(
            !active_pretty.contains("imported worker"),
            "{active_pretty}"
        );
        let raw = render(&cell.raw_lines());
        assert!(raw.contains("imported worker"), "{raw}");
        assert!(raw.contains("imported error"), "{raw}");
        assert!(raw.contains("imported abort"), "{raw}");
        assert!(!raw.contains("retiring worker"), "{raw}");

        let revealed = render(&cell.display_lines_at(80, pending.reveal_at));
        assert_eq!(cell.next_frame_in(pending.reveal_at), None);
        assert!(!revealed.contains("retiring worker"), "{revealed}");
        assert!(revealed.contains("imported worker"), "{revealed}");
        let promoted = state
            .take_due_handoff_history(pending.reveal_at)
            .expect("due handoff should promote the final tree once");
        assert!(!state.has_spawn_call("spawn-animated"));
        assert!(state.pending_handoff.is_none());
        assert!(state.render_cell().is_none());
        assert!(render(&promoted.display_lines(80)).contains("imported worker"));
        assert!(state.take_due_handoff_history(pending.reveal_at).is_none());
    }

    fn canonical_lines(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let spans = line
                    .spans
                    .iter()
                    .map(|span| format!("{:?}:{:?}", span.content, span.style))
                    .collect::<Vec<_>>()
                    .join("|");
                format!(
                    "alignment={:?};style={:?};spans={spans}",
                    line.alignment, line.style
                )
            })
            .collect()
    }

    #[test]
    fn spawn_presentation_frames_are_observationally_stable() {
        let mut state = SpineTreeViewState::new(true);
        let start = Instant::now();
        state.apply_tree_update_at(
            snapshot(
                "root",
                vec![node("root", None, Some("root"), SpineTreeNodeStatus::Live)],
            ),
            start,
            /*await_terminal_activity*/ false,
        );
        state.apply_spawn_progress(spawn_progress(
            "spawn-golden",
            &[(
                "child-golden",
                "golden worker",
                codex_app_server_protocol::CollabAgentStatus::Completed,
            )],
        ));
        assert!(state.overlays[0].set_activity_word_for_test("child-golden", "Calibrate"));
        let deadline = state.overlays[0]
            .completion_deadline("child-golden")
            .expect("completed child should have a deterministic deadline");
        let t0 = deadline - Duration::from_millis(850);
        let t849 = t0 + Duration::from_millis(849);
        let t850 = deadline;

        let mut committed = snapshot(
            "root",
            vec![
                node("root", None, Some("root"), SpineTreeNodeStatus::Live),
                node(
                    "root.1",
                    Some("root"),
                    Some("golden worker"),
                    SpineTreeNodeStatus::Closed,
                ),
            ],
        );
        committed.nodes[1].spawn_outcome = Some(SpineSpawnOutcome::Completed);
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-golden".to_string()];
        state.apply_tree_update_at(committed, t0, /*await_terminal_activity*/ false);

        let cell = state
            .render_cell()
            .expect("settlement should retain the live handoff");
        let pending = cell
            .pending_handoff
            .as_ref()
            .expect("settlement should preserve the prior overlay until reveal");
        let overlay = pending
            .overlays
            .first()
            .expect("the matching settled overlay should drive the handoff");
        let at_t0 = overlay.display_lines_at("  │ ", true, 80, true, t0);
        let at_t849 = overlay.display_lines_at("  │ ", true, 80, true, t849);
        let at_t850 = cell.display_lines_at(80, t850);
        assert_eq!(at_t850.len(), 3);
        assert!(render(&at_t0).contains("golden worker"));
        assert!(render(&at_t849).contains("golden worker"));
        assert!(render(&at_t850).contains("golden worker"));
        assert_ne!(canonical_lines(&at_t0), canonical_lines(&at_t849));
        assert_eq!(
            cell.next_frame_in(t849),
            Some(Duration::from_millis(1)),
            "the prior frame remains authoritative until the exact reveal deadline"
        );
        assert_eq!(cell.next_frame_in(t850), None);
        assert_eq!(
            at_t850[0].spans[1].style.fg,
            Some(SPINE_BRAND_COLOR),
            "header style is part of the stable presentation contract"
        );
        assert!(
            render(&at_t0).contains("Calibrate"),
            "the deterministic activity word must remain visible in the pre-reveal frame"
        );
    }

    #[test]
    fn spawn_presentation_typed_activity_is_observationally_stable() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "root",
            vec![node("root", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));
        state.apply_spawn_progress(spawn_progress(
            "spawn-typed",
            &[(
                "child-typed",
                "typed worker",
                codex_app_server_protocol::CollabAgentStatus::Running,
            )],
        ));
        assert!(state.overlays[0].set_activity_word_for_test("child-typed", "Observe"));

        let typed_notifications = [
            ServerNotification::ItemStarted(codex_app_server_protocol::ItemStartedNotification {
                thread_id: "child-typed".to_string(),
                turn_id: "child-turn".to_string(),
                started_at_ms: 0,
                item: codex_app_server_protocol::ThreadItem::CommandExecution {
                    id: "command".to_string(),
                    plugin_id: None,
                    script_path: None,
                    command: "printf typed".to_string(),
                    cwd: codex_utils_path_uri::LegacyAppPathString::from_path(
                        std::path::Path::new("/tmp"),
                    ),
                    process_id: None,
                    source: codex_app_server_protocol::CommandExecutionSource::Agent,
                    status: codex_app_server_protocol::CommandExecutionStatus::InProgress,
                    command_actions: Vec::new(),
                    aggregated_output: None,
                    exit_code: None,
                    duration_ms: None,
                },
            }),
            ServerNotification::AgentMessageDelta(
                codex_app_server_protocol::AgentMessageDeltaNotification {
                    thread_id: "child-typed".to_string(),
                    turn_id: "child-turn".to_string(),
                    item_id: "message".to_string(),
                    delta: "agent message".to_string(),
                },
            ),
            ServerNotification::PlanDelta(codex_app_server_protocol::PlanDeltaNotification {
                thread_id: "child-typed".to_string(),
                turn_id: "child-turn".to_string(),
                item_id: "plan".to_string(),
                delta: "plan update".to_string(),
            }),
            ServerNotification::ReasoningSummaryTextDelta(
                codex_app_server_protocol::ReasoningSummaryTextDeltaNotification {
                    thread_id: "child-typed".to_string(),
                    turn_id: "child-turn".to_string(),
                    item_id: "reasoning".to_string(),
                    delta: "reasoning summary".to_string(),
                    summary_index: 0,
                },
            ),
        ];
        for notification in typed_notifications {
            assert!(state.apply_activity(
                "turn",
                "spawn-typed",
                "child-typed",
                &notification,
                None,
            ));
        }

        let cell = state
            .render_cell()
            .expect("live typed overlay should render");
        let static_lines = cell.display_lines_at(100, Instant::now());
        let repeated_lines = cell.display_lines_at(100, Instant::now());
        let rendered = render(&static_lines);
        assert_eq!(rendered, render(&repeated_lines));
        for expected in [
            "$ printf typed",
            "agent message",
            "plan update",
            "reasoning summary",
        ] {
            assert!(rendered.contains(expected), "{rendered}");
        }
        assert!(
            static_lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.contains("Observe")
                    && span.style.fg == Some(SPINE_BRAND_COLOR)),
            "typed activity must preserve the brand-styled deterministic word"
        );
        assert_eq!(
            canonical_lines(&static_lines),
            canonical_lines(&repeated_lines)
        );
    }

    #[test]
    fn clearing_turn_during_handoff_reveals_authoritative_tree() {
        let start = Instant::now();
        let mut state = SpineTreeViewState::new(true);
        state.apply_tree_update_at(
            snapshot(
                "1",
                vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
            ),
            start,
            /*await_terminal_activity*/ false,
        );
        state.apply_spawn_progress(spawn_progress(
            "spawn-clear",
            &[(
                "child-clear",
                "retiring worker",
                codex_app_server_protocol::CollabAgentStatus::Completed,
            )],
        ));
        let mut committed = snapshot(
            "1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                node(
                    "1.1",
                    Some("1"),
                    Some("imported worker"),
                    SpineTreeNodeStatus::Closed,
                ),
            ],
        );
        committed.nodes[1].spawn_outcome = Some(SpineSpawnOutcome::Completed);
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-clear".to_string()];
        state.apply_tree_update_at(committed, start, /*await_terminal_activity*/ false);
        assert!(state.pending_handoff.is_some());

        assert!(state.clear_incomplete_spawn_overlays(Some("turn")));
        assert!(state.pending_handoff.is_none());
        assert!(state.render_cell().is_none());
        let rendered = render(
            &state
                .take_pending_history_cell()
                .expect("clearing a handoff should retain the authoritative tree")
                .display_lines_at(80, start),
        );
        assert!(rendered.contains("imported worker"), "{rendered}");
        assert!(!rendered.contains("retiring worker"), "{rendered}");
    }

    #[test]
    fn handoff_mismatch_and_disabled_motion_reveal_immediately() {
        for animations_enabled in [true, false] {
            let start = Instant::now();
            let mut state = SpineTreeViewState::new(animations_enabled);
            state.apply_tree_update_at(
                snapshot(
                    "1",
                    vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
                ),
                start,
                /*await_terminal_activity*/ false,
            );
            state.apply_spawn_progress(spawn_progress(
                "spawn-reveal",
                &[(
                    "child-reveal",
                    "retiring worker",
                    codex_app_server_protocol::CollabAgentStatus::Completed,
                )],
            ));

            let mut committed = snapshot(
                "1",
                vec![
                    node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                    node(
                        "1.1",
                        Some("1"),
                        Some("imported worker"),
                        SpineTreeNodeStatus::Closed,
                    ),
                ],
            );
            committed.nodes[1].spawn_outcome = Some(if animations_enabled {
                SpineSpawnOutcome::Errored
            } else {
                SpineSpawnOutcome::Completed
            });
            committed.snapshot_seq = 2;
            committed.settled_spawn_call_ids = vec!["spawn-reveal".to_string()];
            state.apply_tree_update_at(
                committed,
                Instant::now(),
                /*await_terminal_activity*/ false,
            );

            assert!(state.pending_handoff.is_none());
            assert!(!state.has_spawn_call("spawn-reveal"));
            assert!(state.render_cell().is_none());
            let rendered = render(
                &state
                    .take_pending_history_cell()
                    .expect("non-animated settlement should commit history")
                    .display_lines(80),
            );
            assert!(rendered.contains("imported worker"), "{rendered}");
            assert!(!rendered.contains("retiring worker"), "{rendered}");
        }
    }

    #[test]
    fn missing_settled_overlay_does_not_supersede_existing_handoff() {
        let mut state = SpineTreeViewState::new(true);
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));
        state.apply_spawn_progress(spawn_progress(
            "spawn-present",
            &[(
                "child-present",
                "retiring worker",
                codex_app_server_protocol::CollabAgentStatus::Completed,
            )],
        ));
        let mut committed = snapshot(
            "1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                node(
                    "1.1",
                    Some("1"),
                    Some("imported worker"),
                    SpineTreeNodeStatus::Closed,
                ),
            ],
        );
        committed.nodes[1].spawn_outcome = Some(SpineSpawnOutcome::Completed);
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-present".to_string()];
        state.apply_tree_update(committed.clone());
        assert!(state.pending_handoff.is_some());

        committed.snapshot_seq = 3;
        committed
            .settled_spawn_call_ids
            .push("spawn-missing".to_string());
        state.apply_tree_update(committed);

        assert!(state.pending_handoff.is_some());
        assert!(!state.has_spawn_call("spawn-present"));
        assert!(state.render_cell().is_some());
        assert!(state.take_pending_history_cell().is_none());
    }

    #[test]
    fn settled_call_order_controls_handoff() {
        for (settled_calls, expect_handoff) in [
            (["spawn-completed", "spawn-errored"], true),
            (["spawn-errored", "spawn-completed"], false),
        ] {
            let mut state = SpineTreeViewState::new(true);
            state.apply_tree_update(snapshot(
                "1",
                vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
            ));
            state.apply_spawn_progress(spawn_progress(
                "spawn-completed",
                &[(
                    "child-completed",
                    "completed worker",
                    codex_app_server_protocol::CollabAgentStatus::Completed,
                )],
            ));
            state.apply_spawn_progress(spawn_progress(
                "spawn-errored",
                &[(
                    "child-errored",
                    "errored worker",
                    codex_app_server_protocol::CollabAgentStatus::Errored,
                )],
            ));
            let mut committed = snapshot(
                "1",
                vec![
                    node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                    node(
                        "1.1",
                        Some("1"),
                        Some("imported completion"),
                        SpineTreeNodeStatus::Closed,
                    ),
                    node(
                        "1.2",
                        Some("1"),
                        Some("imported error"),
                        SpineTreeNodeStatus::Closed,
                    ),
                ],
            );
            committed.nodes[1].spawn_outcome = Some(SpineSpawnOutcome::Completed);
            committed.nodes[2].spawn_outcome = Some(SpineSpawnOutcome::Errored);
            committed.snapshot_seq = 2;
            committed.settled_spawn_call_ids =
                settled_calls.map(str::to_string).into_iter().collect();
            state.apply_tree_update(committed);

            assert_eq!(state.pending_handoff.is_some(), expect_handoff);
            assert!(!state.has_spawn_call("spawn-completed"));
            assert!(!state.has_spawn_call("spawn-errored"));
        }
    }

    #[test]
    fn changed_authoritative_outcome_finishes_an_active_handoff_fail_open() {
        let start = Instant::now();
        let mut state = SpineTreeViewState::new(true);
        state.apply_tree_update_at(
            snapshot(
                "1",
                vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
            ),
            start,
            /*await_terminal_activity*/ false,
        );
        state.apply_spawn_progress(spawn_progress(
            "spawn-generation",
            &[(
                "child-generation",
                "retiring worker",
                codex_app_server_protocol::CollabAgentStatus::Completed,
            )],
        ));
        let mut committed = snapshot(
            "1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                node(
                    "1.1",
                    Some("1"),
                    Some("imported worker"),
                    SpineTreeNodeStatus::Closed,
                ),
            ],
        );
        committed.nodes[1].spawn_outcome = Some(SpineSpawnOutcome::Completed);
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-generation".to_string()];
        state.apply_tree_update_at(
            committed.clone(),
            Instant::now(),
            /*await_terminal_activity*/ false,
        );
        assert!(state.pending_handoff.is_some());

        committed.snapshot_seq = 3;
        committed.nodes[1].spawn_outcome = Some(SpineSpawnOutcome::Errored);
        state.apply_tree_update_at(
            committed,
            Instant::now(),
            /*await_terminal_activity*/ false,
        );

        assert!(state.pending_handoff.is_none());
        assert!(!state.has_spawn_call("spawn-generation"));
        assert!(state.render_cell().is_none());
        let rendered = render(
            &state
                .take_pending_history_cell()
                .expect("superseding authority should commit history")
                .display_lines(80),
        );
        assert!(rendered.contains("imported worker"), "{rendered}");
    }

    #[test]
    fn inactive_tree_edges_coalesce_to_the_latest_history_effect() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));

        let mut first = snapshot(
            "1.1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Opened),
                node(
                    "1.1",
                    Some("1"),
                    Some("first inactive edge"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );
        first.snapshot_seq = 2;
        state.apply_tree_update(first);

        let mut latest = snapshot(
            "1.2",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Opened),
                node(
                    "1.2",
                    Some("1"),
                    Some("latest inactive edge"),
                    SpineTreeNodeStatus::Live,
                ),
            ],
        );
        latest.snapshot_seq = 3;
        state.apply_tree_update(latest);

        let history = state
            .take_pending_history_cell()
            .expect("inactive edges should retain one pending presentation");
        let rendered = render(&history.display_lines(80));
        assert!(!rendered.contains("first inactive edge"), "{rendered}");
        assert!(rendered.contains("latest inactive edge"), "{rendered}");
        assert_eq!(history.snapshot_seq(), 3);
        assert!(state.take_pending_history_cell().is_none());
    }

    #[test]
    fn tree_commit_removes_only_the_settled_spawn_overlays() {
        let progress = |call_id: &str, agent_path: &str| SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: call_id.to_string(),
            tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                ordinal: 0,
                summary: format!("task for {call_id}"),
                thread_id: format!("child-{call_id}"),
                agent_path: Some(agent_path.to_string()),
                status: codex_app_server_protocol::CollabAgentStatus::Running,
            }],
        };
        let mut state = SpineTreeViewState::default();
        state.apply_spawn_progress(progress("spawn-1", "/root/first"));
        state.apply_spawn_progress(progress("spawn-2", "/root/second"));

        let mut committed = snapshot(
            "1",
            vec![node("1", None, Some("active"), SpineTreeNodeStatus::Live)],
        );
        committed.settled_spawn_call_ids = vec!["spawn-1".to_string()];
        state.apply_tree_update(committed.clone());
        state.apply_tree_update(committed.clone());

        assert!(!state.has_spawn_call("spawn-1"));
        assert!(state.has_spawn_call("spawn-2"));
        assert!(state.render_cell().is_some());

        committed.snapshot_seq += 1;
        committed.settled_spawn_call_ids = vec!["spawn-2".to_string()];
        state.apply_tree_update(committed.clone());
        assert!(state.render_cell().is_none());
        state.apply_tree_update(committed);
    }

    #[test]
    fn spawn_progress_requires_a_well_formed_overlay_signature() {
        let malformed = [
            identified_spawn_progress(
                "turn",
                "empty",
                &[],
                codex_app_server_protocol::CollabAgentStatus::Running,
            ),
            identified_spawn_progress(
                "turn",
                "missing-zero",
                &[(1, "child-a")],
                codex_app_server_protocol::CollabAgentStatus::Running,
            ),
            identified_spawn_progress(
                "turn",
                "reordered",
                &[(1, "child-a"), (0, "child-b")],
                codex_app_server_protocol::CollabAgentStatus::Running,
            ),
            identified_spawn_progress(
                "turn",
                "duplicate-ordinal",
                &[(0, "child-a"), (0, "child-b")],
                codex_app_server_protocol::CollabAgentStatus::Running,
            ),
            identified_spawn_progress(
                "turn",
                "empty-child",
                &[(0, "")],
                codex_app_server_protocol::CollabAgentStatus::Running,
            ),
            identified_spawn_progress(
                "turn",
                "duplicate-child",
                &[(0, "child-a"), (1, "child-a")],
                codex_app_server_protocol::CollabAgentStatus::Running,
            ),
        ];

        for progress in malformed {
            let mut state = SpineTreeViewState::default();
            state.apply_spawn_progress(progress.clone());
            assert!(
                state.overlays.is_empty(),
                "malformed signature must fail closed: {progress:?}"
            );
        }
    }

    #[test]
    fn conflicting_live_overlay_signature_cannot_replace_the_owner() {
        let mut state = SpineTreeViewState::default();
        state.apply_spawn_progress(identified_spawn_progress(
            "turn",
            "same",
            &[(0, "child-old")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));
        state.apply_spawn_progress(identified_spawn_progress(
            "turn",
            "same",
            &[(0, "child-new")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));

        assert_eq!(state.overlays.len(), 1);
        assert!(state.overlays[0].has_child_thread("child-old"));
        assert!(!state.overlays[0].has_child_thread("child-new"));
    }

    #[test]
    fn settlement_is_scoped_to_the_snapshot_turn_and_transaction() {
        let mut state = SpineTreeViewState::default();
        let mut initial = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        initial.turn_id = "turn-a".to_string();
        state.apply_tree_update(initial);
        state.apply_spawn_progress(identified_spawn_progress(
            "turn-a",
            "same",
            &[(0, "child-a")],
            codex_app_server_protocol::CollabAgentStatus::Completed,
        ));
        state.apply_spawn_progress(identified_spawn_progress(
            "turn-b",
            "same",
            &[(0, "child-b")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));

        let mut committed = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        committed.turn_id = "turn-a".to_string();
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["same".to_string()];
        state.apply_tree_update(committed);

        assert_eq!(state.overlays.len(), 1);
        assert_eq!(state.overlays[0].turn_id(), "turn-b");
        assert!(state.overlays[0].has_child_thread("child-b"));

        state.apply_spawn_progress(identified_spawn_progress(
            "turn-a",
            "same",
            &[(0, "child-a")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));
        assert_eq!(
            state.overlays.len(),
            1,
            "exact late progress must be rejected"
        );

        state.apply_spawn_progress(identified_spawn_progress(
            "turn-a",
            "same",
            &[(0, "child-new")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));
        assert_eq!(state.overlays.len(), 1);
        assert!(!state.overlays[0].has_child_thread("child-new"));
    }

    #[test]
    fn zero_match_settlement_does_not_guess_a_duplicate_guard() {
        let mut state = SpineTreeViewState::default();
        let mut committed = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-missing".to_string()];
        state.apply_tree_update(committed);

        state.apply_spawn_progress(identified_spawn_progress(
            "turn",
            "spawn-missing",
            &[(0, "child-late")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));

        assert_eq!(state.overlays.len(), 1);
        assert!(state.overlays[0].has_child_thread("child-late"));
    }

    #[test]
    fn settlement_preserves_pulse_order_and_leaves_wrong_turn_overlay_live() {
        let start = Instant::now();
        let mut state = SpineTreeViewState::new(true);
        let mut initial = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        initial.turn_id = "turn-a".to_string();
        state.apply_tree_update_at(initial, start, /*await_terminal_activity*/ false);
        for (turn_id, call_id, child_id) in [
            ("turn-a", "spawn-b", "child-b"),
            ("turn-b", "spawn-a", "child-wrong-turn"),
            ("turn-a", "spawn-a", "child-a"),
        ] {
            state.apply_spawn_progress(identified_spawn_progress(
                turn_id,
                call_id,
                &[(0, child_id)],
                codex_app_server_protocol::CollabAgentStatus::Completed,
            ));
        }

        let mut committed = snapshot(
            "1",
            vec![
                node("1", None, Some("root"), SpineTreeNodeStatus::Live),
                node(
                    "1.1",
                    Some("1"),
                    Some("first import"),
                    SpineTreeNodeStatus::Closed,
                ),
                node(
                    "1.2",
                    Some("1"),
                    Some("second import"),
                    SpineTreeNodeStatus::Closed,
                ),
            ],
        );
        committed.turn_id = "turn-a".to_string();
        committed.nodes[1].spawn_outcome = Some(SpineSpawnOutcome::Completed);
        committed.nodes[2].spawn_outcome = Some(SpineSpawnOutcome::Completed);
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["spawn-a".to_string(), "spawn-b".to_string()];
        state.apply_tree_update_at(committed, start, /*await_terminal_activity*/ false);

        assert_eq!(
            state
                .pending_handoff
                .as_ref()
                .expect("matching turn overlays should animate")
                .overlays
                .iter()
                .map(SpineSpawnOverlay::call_id)
                .collect::<Vec<_>>(),
            vec!["spawn-a", "spawn-b"]
        );
        assert_eq!(state.overlays.len(), 1);
        assert_eq!(state.overlays[0].turn_id(), "turn-b");
        assert!(state.overlays[0].has_child_thread("child-wrong-turn"));
    }

    #[test]
    fn ambiguous_settlement_fails_closed_without_guessing_an_owner() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));
        state
            .overlays
            .push(SpineSpawnOverlay::new(identified_spawn_progress(
                "turn",
                "same",
                &[(0, "child-a")],
                codex_app_server_protocol::CollabAgentStatus::Completed,
            )));
        state
            .overlays
            .push(SpineSpawnOverlay::new(identified_spawn_progress(
                "turn",
                "same",
                &[(0, "child-b")],
                codex_app_server_protocol::CollabAgentStatus::Completed,
            )));

        let mut committed = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["same".to_string()];
        state.apply_tree_update(committed);

        assert_eq!(state.overlays.len(), 2);
        assert!(state.pending_handoff.is_none());
    }

    #[test]
    fn settled_transaction_rejects_activity_from_old_or_reused_child() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));
        state.apply_spawn_progress(identified_spawn_progress(
            "turn",
            "same",
            &[(0, "child-old")],
            codex_app_server_protocol::CollabAgentStatus::Completed,
        ));
        let mut committed = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["same".to_string()];
        state.apply_tree_update(committed);
        state.apply_spawn_progress(identified_spawn_progress(
            "turn",
            "same",
            &[(0, "child-new")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));

        let activity = ServerNotification::AgentMessageDelta(
            codex_app_server_protocol::AgentMessageDeltaNotification {
                thread_id: "child".to_string(),
                turn_id: "child-turn".to_string(),
                item_id: "message".to_string(),
                delta: "only the current child".to_string(),
            },
        );
        assert!(!state.apply_activity("turn", "same", "child-old", &activity, None));
        assert!(!state.apply_activity("turn", "same", "child-new", &activity, None));
    }

    #[test]
    fn terminal_cleanup_releases_turn_local_duplicate_guards() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        ));
        let progress = identified_spawn_progress(
            "turn",
            "same",
            &[(0, "child")],
            codex_app_server_protocol::CollabAgentStatus::Completed,
        );
        state.apply_spawn_progress(progress.clone());
        let mut committed = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["same".to_string()];
        state.apply_tree_update(committed);
        state.apply_spawn_progress(progress.clone());
        assert!(state.overlays.is_empty());

        state.clear_incomplete_spawn_overlays(Some("turn"));
        state.apply_spawn_progress(progress);
        assert_eq!(state.overlays.len(), 1);
    }

    #[test]
    fn completed_turn_cleanup_removes_live_state_but_preserves_handoff() {
        let mut state = SpineTreeViewState::default();
        let turn_a = identified_spawn_progress(
            "turn-a",
            "spawn-a",
            &[(0, "child-a")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        );
        let turn_b = identified_spawn_progress(
            "turn-b",
            "spawn-b",
            &[(0, "child-b")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        );
        state.apply_spawn_progress(turn_a.clone());
        state.apply_spawn_progress(turn_b);
        let handoff_snapshot = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        state.pending_handoff = Some(PendingTreeHandoff {
            snapshot: handoff_snapshot.clone(),
            reveal_at: Instant::now(),
            overlays: vec![SpineSpawnOverlay::new(turn_a)],
            activity_pending: false,
        });
        let settled_signature = OverlaySignature::from_overlay(&state.overlays[0]);
        state.settled_spawn_signatures.insert(settled_signature);

        assert!(state.clear_completed_spawn_overlays("turn-a"));
        assert!(!state.has_spawn_call("spawn-a"));
        assert!(state.has_spawn_call("spawn-b"));
        assert!(state.settled_spawn_signatures.is_empty());
        assert_eq!(
            state
                .pending_handoff
                .as_ref()
                .map(|handoff| &handoff.snapshot),
            Some(&handoff_snapshot)
        );
        assert!(state.pending_history.is_none());
    }

    #[test]
    fn completed_turn_cleanup_keeps_settled_transaction_guard_for_late_attempts() {
        let progress = identified_spawn_progress(
            "turn",
            "same",
            &[(0, "child-old")],
            codex_app_server_protocol::CollabAgentStatus::Completed,
        );
        let mut state = SpineTreeViewState::default();
        state.apply_spawn_progress(progress.clone());

        let mut committed = snapshot(
            "1",
            vec![node("1", None, Some("root"), SpineTreeNodeStatus::Live)],
        );
        committed.snapshot_seq = 2;
        committed.settled_spawn_call_ids = vec!["same".to_string()];
        state.apply_tree_update(committed);
        assert!(state.clear_completed_spawn_overlays("turn"));

        state.apply_spawn_progress(identified_spawn_progress(
            "turn",
            "same",
            &[(0, "child-old")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));
        state.apply_spawn_progress(identified_spawn_progress(
            "turn",
            "same",
            &[(0, "child-retry")],
            codex_app_server_protocol::CollabAgentStatus::Running,
        ));

        assert!(!state.has_spawn_call("same"));
        assert!(state.render_cell().is_none());
    }

    #[test]
    fn settled_spawn_progress_cannot_recreate_a_transient_overlay() {
        let progress = SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: "spawn-settled".to_string(),
            tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "completed worker".to_string(),
                thread_id: "child-settled".to_string(),
                agent_path: Some("/root/completed-worker".to_string()),
                status: codex_app_server_protocol::CollabAgentStatus::Completed,
            }],
        };
        let mut state = SpineTreeViewState::default();
        state.apply_spawn_progress(progress.clone());
        assert!(state.has_spawn_call("spawn-settled"));

        let mut committed = snapshot(
            "1",
            vec![node(
                "1",
                None,
                Some("completed worker"),
                SpineTreeNodeStatus::Closed,
            )],
        );
        committed.settled_spawn_call_ids = vec!["spawn-settled".to_string()];
        state.apply_tree_update(committed);
        assert!(!state.has_spawn_call("spawn-settled"));

        state.apply_spawn_progress(progress);
        assert!(!state.has_spawn_call("spawn-settled"));
        assert!(state.render_cell().is_none());
    }

    #[test]
    fn zero_match_settlement_allows_later_progress_without_guessing_identity() {
        let mut state = SpineTreeViewState::default();
        let mut committed = snapshot(
            "1",
            vec![node(
                "1",
                None,
                Some("completed worker"),
                SpineTreeNodeStatus::Closed,
            )],
        );
        committed.settled_spawn_call_ids = vec!["spawn-settled".to_string()];
        state.apply_tree_update(committed);

        state.apply_spawn_progress(SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: "spawn-settled".to_string(),
            tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "completed worker".to_string(),
                thread_id: "child-settled".to_string(),
                agent_path: Some("/root/completed-worker".to_string()),
                status: codex_app_server_protocol::CollabAgentStatus::Completed,
            }],
        });

        assert!(state.has_spawn_call("spawn-settled"));
        assert!(state.render_cell().is_some());
    }

    #[test]
    fn stale_tree_update_cannot_replace_snapshot_or_settle_overlay() {
        let mut state = SpineTreeViewState::default();
        let mut current = snapshot(
            "1",
            vec![node("1", None, Some("current"), SpineTreeNodeStatus::Live)],
        );
        current.snapshot_seq = 2;
        state.apply_tree_update(current);
        state.apply_spawn_progress(SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn-2".to_string(),
            call_id: "spawn-live".to_string(),
            tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "worker".to_string(),
                thread_id: "child-live".to_string(),
                agent_path: Some("/root/worker".to_string()),
                status: codex_app_server_protocol::CollabAgentStatus::Running,
            }],
        });

        let mut stale = snapshot(
            "1",
            vec![node("1", None, Some("stale"), SpineTreeNodeStatus::Live)],
        );
        stale.settled_spawn_call_ids = vec!["spawn-live".to_string()];
        state.apply_tree_update(stale);

        assert_eq!(
            state.snapshot().map(|snapshot| snapshot.snapshot_seq),
            Some(2)
        );
        assert!(state.has_spawn_call("spawn-live"));
    }

    #[test]
    fn activity_update_targets_one_turn_and_call() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1",
            vec![node("1", None, Some("active"), SpineTreeNodeStatus::Live)],
        ));
        for (turn_id, call_id, summary) in [
            ("turn-1", "spawn-1", "first worker"),
            ("turn-2", "spawn-2", "second worker"),
        ] {
            state.apply_spawn_progress(SpineSpawnProgressUpdatedNotification {
                thread_id: "thread".to_string(),
                turn_id: turn_id.to_string(),
                call_id: call_id.to_string(),
                tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                    ordinal: 0,
                    summary: summary.to_string(),
                    thread_id: format!("child-{call_id}"),
                    agent_path: Some("/root/shared".to_string()),
                    status: codex_app_server_protocol::CollabAgentStatus::Running,
                }],
            });
        }

        assert!(state.apply_activity(
            "turn-2",
            "spawn-2",
            "child-spawn-2",
            &ServerNotification::AgentMessageDelta(
                codex_app_server_protocol::AgentMessageDeltaNotification {
                    thread_id: "child".to_string(),
                    turn_id: "child-turn".to_string(),
                    item_id: "message".to_string(),
                    delta: "second only".to_string(),
                },
            ),
            None,
        ));

        let rendered = render(
            &state
                .render_cell()
                .expect("tree state should render")
                .display_lines(80),
        );
        assert_eq!(rendered.matches("second only").count(), 1, "{rendered}");
        assert!(rendered.contains("Waiting for activity..."), "{rendered}");
    }

    #[test]
    fn mounts_spawn_overlay_under_the_active_node() {
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1.1",
            vec![
                node("1", None, Some("parent"), SpineTreeNodeStatus::Opened),
                node("1.1", Some("1"), Some("active"), SpineTreeNodeStatus::Live),
            ],
        ));
        state.apply_spawn_progress(SpineSpawnProgressUpdatedNotification {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: "spawn-1".to_string(),
            tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                ordinal: 0,
                summary: "inspect events".to_string(),
                thread_id: "child-inspector".to_string(),
                agent_path: Some("/root/inspector".to_string()),
                status: codex_app_server_protocol::CollabAgentStatus::Running,
            }],
        });
        let cell = state.render_cell().expect("tree snapshot should render");

        let rendered = render(&cell.display_lines(80));
        let task_line = rendered
            .lines()
            .find(|line| line.contains("inspect events"))
            .expect("spawn task line should render");
        assert!(task_line.starts_with("      └ "), "{rendered}");
        assert!(!task_line.contains('•'), "{rendered}");
        assert!(!task_line.contains('◦'), "{rendered}");
        assert!(!task_line.contains("leaf 0"), "{rendered}");
        assert!(!rendered.contains("spine.spawn"));
        assert!(!rendered.contains("/root/inspector"));
    }

    #[test]
    fn multiple_spawn_overlays_share_direct_task_sibling_branches() {
        let spawn_progress = |call_id: &str, summary: &str, agent_path: &str| {
            SpineSpawnProgressUpdatedNotification {
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                call_id: call_id.to_string(),
                tasks: vec![codex_app_server_protocol::SpineSpawnTaskProgress {
                    ordinal: 0,
                    summary: summary.to_string(),
                    thread_id: format!("child-{call_id}"),
                    agent_path: Some(agent_path.to_string()),
                    status: codex_app_server_protocol::CollabAgentStatus::Running,
                }],
            }
        };
        let mut state = SpineTreeViewState::default();
        state.apply_tree_update(snapshot(
            "1.1",
            vec![
                node("1", None, Some("parent"), SpineTreeNodeStatus::Opened),
                node("1.1", Some("1"), Some("active"), SpineTreeNodeStatus::Live),
            ],
        ));
        state.apply_spawn_progress(spawn_progress("spawn-1", "first task", "/root/first"));
        state.apply_spawn_progress(spawn_progress("spawn-2", "second task", "/root/second"));
        let cell = state.render_cell().expect("tree snapshot should render");

        let task_lines = cell
            .display_lines(80)
            .into_iter()
            .map(|line| line.to_string())
            .filter(|line| line.contains("first task") || line.contains("second task"))
            .collect::<Vec<_>>();
        assert_eq!(task_lines.len(), 2, "{task_lines:?}");
        assert!(task_lines[0].starts_with("      ├ "), "{task_lines:?}");
        assert!(task_lines[1].starts_with("      └ "), "{task_lines:?}");
    }
}

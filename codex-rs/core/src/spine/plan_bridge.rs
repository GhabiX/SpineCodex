use super::ids::NodeId;
use codex_protocol::plan_tool::SpineAllocationArg;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::plan_tool::UpdatePlanArgs;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct PlanSnapshot {
    pub(crate) node_id: String,
    pub(crate) revision: u64,
    pub(crate) explanation: Option<String>,
    pub(crate) items: Vec<PlanSnapshotItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scope_allocation: Option<PlanScopeAllocationSnapshot>,
    pub(crate) source_turn_id: String,
    pub(crate) event_seq: u64,
}

impl PlanSnapshot {
    pub(crate) fn from_update(
        node_id: &NodeId,
        revision: u64,
        event_seq: u64,
        source_turn_id: impl Into<String>,
        args: UpdatePlanArgs,
        scope_allocation: Option<PlanScopeAllocationSnapshot>,
        previous: Option<&PlanSnapshot>,
    ) -> Self {
        let mut id_allocator = StableTaskIdAllocator::new(previous);
        Self {
            node_id: node_id.to_string(),
            revision,
            explanation: args.explanation,
            items: args
                .plan
                .into_iter()
                .map(|item| PlanSnapshotItem {
                    stable_task_id: id_allocator.id_for_step(&item.step),
                    step: item.step,
                    status: step_status_label(&item.status).to_string(),
                })
                .collect(),
            scope_allocation,
            source_turn_id: source_turn_id.into(),
            event_seq,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct PlanSnapshotItem {
    pub(crate) stable_task_id: String,
    pub(crate) step: String,
    pub(crate) status: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct PlanScopeAllocationSnapshot {
    pub(crate) anchor_node_id: String,
    pub(crate) scopes: Vec<PlanAllocationScope>,
}

impl PlanScopeAllocationSnapshot {
    pub(crate) fn from_update(anchor_node_id: &NodeId, allocation: SpineAllocationArg) -> Self {
        Self {
            anchor_node_id: anchor_node_id.to_string(),
            scopes: allocation
                .scopes
                .into_iter()
                .map(|scope| PlanAllocationScope {
                    existing_node_id: scope.node,
                    summary: scope.summary,
                    checkpoints: scope.checkpoints,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct PlanAllocationScope {
    pub(crate) existing_node_id: Option<String>,
    pub(crate) summary: String,
    pub(crate) checkpoints: Vec<String>,
}

fn step_status_label(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::InProgress => "in_progress",
        StepStatus::Completed => "completed",
    }
}

struct StableTaskIdAllocator<'a> {
    previous_items: &'a [PlanSnapshotItem],
    used_previous_items: Vec<bool>,
    next_task_number: u64,
}

impl<'a> StableTaskIdAllocator<'a> {
    fn new(previous: Option<&'a PlanSnapshot>) -> Self {
        let previous_items = previous
            .map(|snapshot| snapshot.items.as_slice())
            .unwrap_or(&[]);
        let max_task_number = previous_items
            .iter()
            .filter_map(|item| item.stable_task_id.strip_prefix("step-"))
            .filter_map(|suffix| suffix.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        Self {
            previous_items,
            used_previous_items: vec![false; previous_items.len()],
            next_task_number: max_task_number + 1,
        }
    }

    fn id_for_step(&mut self, step: &str) -> String {
        if let Some((index, item)) = self
            .previous_items
            .iter()
            .enumerate()
            .find(|(index, item)| !self.used_previous_items[*index] && item.step == step)
        {
            self.used_previous_items[index] = true;
            return item.stable_task_id.clone();
        }

        let stable_task_id = format!("step-{}", self.next_task_number);
        self.next_task_number += 1;
        stable_task_id
    }
}

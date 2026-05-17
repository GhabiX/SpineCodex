use super::ids::NodeId;
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
    pub(crate) spine_plantree: Option<PlanTreeSnapshot>,
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
        spine_plantree: Option<PlanTreeSnapshot>,
    ) -> Self {
        Self {
            node_id: node_id.to_string(),
            revision,
            explanation: args.explanation,
            items: args
                .plan
                .into_iter()
                .enumerate()
                .map(|(index, item)| PlanSnapshotItem {
                    stable_task_id: format!("step-{}", index + 1),
                    step: item.step,
                    status: step_status_label(&item.status).to_string(),
                })
                .collect(),
            spine_plantree,
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
pub(crate) struct PlanTreeSnapshot {
    pub(crate) anchor_node_id: String,
    pub(crate) root: PlanTreeScope,
}

impl PlanTreeSnapshot {
    pub(crate) fn from_update(anchor_node_id: &NodeId, plantree: PlanTreeDraft) -> Self {
        Self {
            anchor_node_id: anchor_node_id.to_string(),
            root: PlanTreeScope::from_update(plantree.root),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PlanTreeDraft {
    pub(crate) anchor: NodeId,
    pub(crate) root: PlanTreeScopeDraft,
}

#[derive(Clone, Debug)]
pub(crate) struct PlanTreeScopeDraft {
    pub(crate) node: Option<String>,
    pub(crate) summary: String,
    pub(crate) status: Option<StepStatus>,
    pub(crate) checkpoints: Vec<PlanTreeCheckpointDraft>,
    pub(crate) children: Vec<PlanTreeScopeDraft>,
}

#[derive(Clone, Debug)]
pub(crate) struct PlanTreeCheckpointDraft {
    pub(crate) task: String,
    pub(crate) status: StepStatus,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct PlanTreeScope {
    pub(crate) existing_node_id: Option<String>,
    pub(crate) summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) checkpoints: Vec<PlanTreeCheckpoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) children: Vec<PlanTreeScope>,
}

impl PlanTreeScope {
    fn from_update(scope: PlanTreeScopeDraft) -> Self {
        Self {
            existing_node_id: scope.node,
            summary: scope.summary,
            status: scope
                .status
                .as_ref()
                .map(step_status_label)
                .map(str::to_string),
            checkpoints: scope
                .checkpoints
                .into_iter()
                .map(PlanTreeCheckpoint::from_update)
                .collect(),
            children: scope
                .children
                .into_iter()
                .map(PlanTreeScope::from_update)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct PlanTreeCheckpoint {
    pub(crate) task: String,
    pub(crate) status: String,
}

impl PlanTreeCheckpoint {
    fn from_update(checkpoint: PlanTreeCheckpointDraft) -> Self {
        Self {
            task: checkpoint.task,
            status: step_status_label(&checkpoint.status).to_string(),
        }
    }
}

fn step_status_label(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::InProgress => "in_progress",
        StepStatus::Completed => "completed",
    }
}

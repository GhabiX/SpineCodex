use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

// Types for the TODO tool arguments matching codex-vscode/todo-mcp/src/main.rs
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct PlanItemArg {
    pub step: String,
    pub status: StepStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlanArgs {
    /// Arguments for the `update_plan` todo/checklist tool (not plan mode).
    #[serde(default)]
    pub explanation: Option<String>,
    pub plan: Vec<PlanItemArg>,
    #[serde(default)]
    pub spine_plantree: Option<SpinePlanTreeArg>,
    #[serde(default)]
    pub clear_spine_plantree: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SpinePlanTreeArg {
    #[serde(default)]
    pub anchor: Option<String>,
    pub root: SpinePlanTreeScopeArg,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SpinePlanTreeScopeArg {
    #[serde(default)]
    pub node: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub status: Option<StepStatus>,
    #[serde(default)]
    pub checkpoints: Vec<SpinePlanTreeCheckpointArg>,
    #[serde(default)]
    pub children: Vec<SpinePlanTreeScopeArg>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SpinePlanTreeCheckpointArg {
    pub task: String,
    pub status: StepStatus,
}

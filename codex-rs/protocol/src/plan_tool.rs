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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SpineUpdatePlanArgs {
    /// Flat plan args shared with the normal upstream-compatible `update_plan` event.
    #[serde(flatten)]
    pub flat: UpdatePlanArgs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spine_plantree: Option<SpinePlanTreeArg>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn update_plan_args_omit_default_spine_fields_when_serialized() {
        let args = UpdatePlanArgs {
            explanation: Some("flat".to_string()),
            plan: vec![PlanItemArg {
                step: "inspect".to_string(),
                status: StepStatus::InProgress,
            }],
        };

        assert_eq!(
            serde_json::to_value(args).expect("serialize"),
            json!({
                "explanation": "flat",
                "plan": [
                    {
                        "step": "inspect",
                        "status": "in_progress",
                    },
                ],
            })
        );
    }

    #[test]
    fn flat_update_plan_args_reject_spine_fields() {
        let err = serde_json::from_value::<UpdatePlanArgs>(json!({
            "plan": [],
            "spine_plantree": {
                "root": {
                    "summary": "hidden"
                }
            }
        }))
        .expect_err("flat update_plan args should reject spine-only fields");

        assert!(err.to_string().contains("unknown field"));
    }
}

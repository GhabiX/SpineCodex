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

    #[test]
    fn flat_update_plan_args_reject_task_projection() {
        let err = serde_json::from_value::<UpdatePlanArgs>(json!({
            "task_projection": {
                "current": {
                    "node_id": "1.1",
                    "checklist": [],
                },
                "draft_nodes": [],
            },
        }))
        .expect_err("flat update_plan args should reject task_projection");

        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn update_plan_args_reject_spine_projection_inputs() {
        for value in [
            json!({
                "task_projection": {
                    "current": {
                        "node_id": "1.1",
                        "checklist": []
                    },
                    "draft_nodes": []
                }
            }),
            json!({
                "spine_plantree": {
                    "root": {
                        "summary": "hidden"
                    }
                }
            }),
            json!({
                "clear_spine_plantree": true
            }),
        ] {
            let err = serde_json::from_value::<UpdatePlanArgs>(value)
                .expect_err("Spine projection fields are not update_plan inputs");
            assert!(err.to_string().contains("unknown field"));
        }
    }
}

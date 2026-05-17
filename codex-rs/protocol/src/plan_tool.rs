use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de;
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

#[derive(Debug, Clone, Serialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct SpineUpdatePlanArgs {
    /// Flat plan args shared with the normal upstream-compatible `update_plan` event.
    #[serde(flatten)]
    pub flat: UpdatePlanArgs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_projection: Option<TaskProjectionArg>,
}

impl<'de> Deserialize<'de> for SpineUpdatePlanArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawSpineUpdatePlanArgs {
            #[serde(default)]
            explanation: Option<String>,
            #[serde(default)]
            plan: Option<Vec<PlanItemArg>>,
            #[serde(default)]
            task_projection: Option<TaskProjectionArg>,
        }

        let raw = RawSpineUpdatePlanArgs::deserialize(deserializer)?;
        let flat = if let Some(task_projection) = &raw.task_projection {
            if raw.plan.is_some() {
                return Err(de::Error::custom(
                    "task_projection cannot be combined with top-level plan",
                ));
            }
            UpdatePlanArgs {
                explanation: raw.explanation,
                plan: task_projection.current.checklist.clone(),
            }
        } else {
            UpdatePlanArgs {
                explanation: raw.explanation,
                plan: raw.plan.ok_or_else(|| de::Error::missing_field("plan"))?,
            }
        };

        Ok(Self {
            flat,
            task_projection: raw.task_projection,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TaskProjectionArg {
    pub current: TaskProjectionCurrentArg,
    #[serde(default)]
    pub draft_nodes: Vec<TaskProjectionDraftNodeArg>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TaskProjectionCurrentArg {
    pub node_id: String,
    #[serde(default)]
    pub checklist: Vec<PlanItemArg>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TaskProjectionDraftNodeArg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    pub parent: String,
    pub summary: String,
    #[serde(default)]
    pub checklist: Vec<PlanItemArg>,
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
    fn spine_update_plan_args_accept_task_projection_without_top_level_plan() {
        let args = serde_json::from_value::<SpineUpdatePlanArgs>(json!({
            "explanation": "projection",
            "task_projection": {
                "current": {
                    "node_id": "1.1",
                    "checklist": [
                        {"step": "inspect", "status": "in_progress"}
                    ]
                },
                "draft_nodes": [
                    {
                        "parent": "1.1",
                        "summary": "future scope",
                        "checklist": [
                            {"step": "verify", "status": "pending"}
                        ]
                    }
                ]
            }
        }))
        .expect("parse task_projection");

        assert_eq!(args.flat.explanation.as_deref(), Some("projection"));
        assert_eq!(args.flat.plan.len(), 1);
        assert_eq!(args.flat.plan[0].step, "inspect");
        assert!(args.task_projection.is_some());
    }

    #[test]
    fn spine_update_plan_args_reject_task_projection_with_top_level_plan() {
        let err = serde_json::from_value::<SpineUpdatePlanArgs>(json!({
            "plan": [],
            "task_projection": {
                "current": {
                    "node_id": "1.1",
                    "checklist": []
                }
            }
        }))
        .expect_err("task_projection should replace top-level plan");

        assert!(
            err.to_string()
                .contains("task_projection cannot be combined")
        );
    }

    #[test]
    fn spine_update_plan_args_reject_old_plantree_inputs() {
        let err = serde_json::from_value::<SpineUpdatePlanArgs>(json!({
            "plan": [],
            "spine_plantree": {
                "root": {
                    "summary": "duplicate"
                }
            }
        }))
        .expect_err("spine_plantree should no longer be an update_plan input");

        assert!(err.to_string().contains("unknown field"));

        let err = serde_json::from_value::<SpineUpdatePlanArgs>(json!({
            "plan": [],
            "clear_spine_plantree": true
        }))
        .expect_err("clear_spine_plantree should no longer be an update_plan input");

        assert!(err.to_string().contains("unknown field"));
    }
}

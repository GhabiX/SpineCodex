use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::plan_spec::UpdatePlanToolOptions;
use crate::tools::handlers::plan_spec::create_update_plan_tool_with_options;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::plan_tool::SpineUpdatePlanArgs;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde_json::Value as JsonValue;

#[derive(Default)]
pub struct PlanHandler {
    include_task_projection: bool,
}

impl PlanHandler {
    pub fn new(include_task_projection: bool) -> Self {
        Self {
            include_task_projection,
        }
    }
}

pub struct PlanToolOutput {
    spine_tree: Option<SpineTreeUpdateEvent>,
}

const PLAN_UPDATED_MESSAGE: &str = "Plan updated";

impl PlanToolOutput {
    fn flat() -> Self {
        Self { spine_tree: None }
    }

    fn with_spine_tree(spine_tree: SpineTreeUpdateEvent) -> Self {
        Self {
            spine_tree: Some(spine_tree),
        }
    }

    fn output_json(&self) -> JsonValue {
        match &self.spine_tree {
            Some(spine_tree) => serde_json::json!({
                "status": "plan_updated",
                "spine_tree": spine_tree,
            }),
            None => serde_json::json!({
                "status": "plan_updated",
            }),
        }
    }

    fn output_text(&self) -> String {
        match &self.spine_tree {
            Some(_) => self.output_json().to_string(),
            None => PLAN_UPDATED_MESSAGE.to_string(),
        }
    }
}

impl ToolOutput for PlanToolOutput {
    fn log_preview(&self) -> String {
        PLAN_UPDATED_MESSAGE.to_string()
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, _payload: &ToolPayload) -> ResponseInputItem {
        let mut output = FunctionCallOutputPayload::from_text(self.output_text());
        output.success = Some(true);

        ResponseInputItem::FunctionCallOutput {
            call_id: call_id.to_string(),
            output,
        }
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        match &self.spine_tree {
            Some(_) => self.output_json(),
            None => JsonValue::Object(serde_json::Map::new()),
        }
    }
}

impl ToolHandler for PlanHandler {
    type Output = PlanToolOutput;

    fn tool_name(&self) -> ToolName {
        ToolName::plain("update_plan")
    }

    fn spec(&self) -> Option<ToolSpec> {
        if self.include_task_projection {
            Some(create_update_plan_tool_with_options(
                UpdatePlanToolOptions {
                    include_task_projection: true,
                },
            ))
        } else {
            Some(crate::tools::handlers::plan_spec::create_update_plan_tool())
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id: _,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "update_plan handler received unsupported payload".to_string(),
                ));
            }
        };

        if turn.collaboration_mode.mode == ModeKind::Plan {
            return Err(FunctionCallError::RespondToModel(
                "update_plan is a TODO/checklist tool and is not allowed in Plan mode".to_string(),
            ));
        }

        if self.include_task_projection {
            let args = parse_spine_update_plan_arguments(&arguments)?;
            let spine_tree = session
                .record_spine_plan_update_and_emit_progress(turn.as_ref(), args)
                .await
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            if let Some(spine_tree) = spine_tree {
                return Ok(PlanToolOutput::with_spine_tree(spine_tree));
            }
        } else {
            let args = parse_update_plan_arguments(&arguments)?;
            session
                .record_plan_update_and_emit_progress(turn.as_ref(), args)
                .await
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        }

        Ok(PlanToolOutput::flat())
    }
}

fn parse_update_plan_arguments(arguments: &str) -> Result<UpdatePlanArgs, FunctionCallError> {
    serde_json::from_str::<UpdatePlanArgs>(arguments).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {e}"))
    })
}

fn parse_spine_update_plan_arguments(
    arguments: &str,
) -> Result<SpineUpdatePlanArgs, FunctionCallError> {
    serde_json::from_str::<SpineUpdatePlanArgs>(arguments).map_err(|e| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {e}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::spine_tree::SpineTreeNodeSnapshot;
    use codex_protocol::spine_tree::SpineTreeNodeStatus;
    use codex_protocol::spine_tree::SpineTreePlanItemSnapshot;
    use codex_protocol::spine_tree::SpineTreePlanItemStatus;
    use codex_protocol::spine_tree::SpineTreePlanSnapshot;
    use codex_protocol::spine_tree::SpineTreePlanTreeScopeSnapshot;
    use codex_protocol::spine_tree::SpineTreePlanTreeSnapshot;
    use serde_json::json;

    #[test]
    fn flat_plan_handler_uses_upstream_compatible_schema() {
        let ToolSpec::Function(tool) = PlanHandler::default().spec().expect("spec") else {
            panic!("expected function spec");
        };
        let properties = tool
            .parameters
            .properties
            .as_ref()
            .expect("schema properties");

        assert!(!properties.contains_key("spine_plantree"));
        assert!(!properties.contains_key("clear_spine_plantree"));
        assert!(!properties.contains_key("task_projection"));
    }

    #[test]
    fn spine_plan_handler_schema_includes_task_projection_only() {
        let ToolSpec::Function(tool) = PlanHandler::new(true).spec().expect("spec") else {
            panic!("expected function spec");
        };
        let properties = tool
            .parameters
            .properties
            .as_ref()
            .expect("schema properties");

        assert!(!properties.contains_key("spine_plantree"));
        assert!(!properties.contains_key("clear_spine_plantree"));
        assert!(properties.contains_key("task_projection"));
    }

    #[test]
    fn flat_plan_tool_output_preserves_legacy_response_text() {
        let output = PlanToolOutput::flat();
        let response = output.to_response_item(
            "call-plan",
            &ToolPayload::Function {
                arguments: "{}".to_string(),
            },
        );
        let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
            panic!("expected function call output");
        };

        assert_eq!(output.text_content(), Some(PLAN_UPDATED_MESSAGE));
        assert_eq!(output.success, Some(true));
        assert_eq!(
            PlanToolOutput::flat().code_mode_result(&ToolPayload::Function {
                arguments: "{}".to_string(),
            }),
            json!({})
        );
    }

    #[test]
    fn spine_plan_tool_output_returns_updated_tree_json() {
        let output = PlanToolOutput::with_spine_tree(sample_spine_tree());
        let response = output.to_response_item(
            "call-plan",
            &ToolPayload::Function {
                arguments: "{}".to_string(),
            },
        );
        let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
            panic!("expected function call output");
        };
        let text = output
            .text_content()
            .expect("spine plan output should be text json");
        let value: serde_json::Value = serde_json::from_str(text).expect("parse output json");

        assert_eq!(value["status"], "plan_updated");
        assert_eq!(value["spine_tree"]["snapshotSeq"], 7);
        assert_eq!(value["spine_tree"]["activeNodeId"], "1.1");
        assert_eq!(
            value["spine_tree"]["nodes"][0]["plan"]["items"][0]["step"],
            "Inspect current node"
        );
        assert_eq!(
            value["spine_tree"]["nodes"][0]["plan"]["spinePlantree"]["root"]["children"][0]["checkpoints"]
                [0]["task"],
            "Run future validation"
        );
        assert_eq!(output.success, Some(true));
    }

    fn sample_spine_tree() -> SpineTreeUpdateEvent {
        SpineTreeUpdateEvent {
            snapshot_seq: 7,
            active_node_id: "1.1".to_string(),
            nodes: vec![SpineTreeNodeSnapshot {
                node_id: "1.1".to_string(),
                parent_id: Some("1".to_string()),
                summary: Some("Current scope".to_string()),
                status: SpineTreeNodeStatus::Live,
                plan: Some(SpineTreePlanSnapshot {
                    revision: 2,
                    explanation: Some("updated planning tree".to_string()),
                    spine_plantree: Some(SpineTreePlanTreeSnapshot {
                        anchor_node_id: "1.1".to_string(),
                        root: SpineTreePlanTreeScopeSnapshot {
                            existing_node_id: Some("1.1".to_string()),
                            summary: "Current scope".to_string(),
                            status: Some(SpineTreePlanItemStatus::InProgress),
                            checkpoints: Vec::new(),
                            children: vec![SpineTreePlanTreeScopeSnapshot {
                                existing_node_id: None,
                                summary: "Future validation".to_string(),
                                status: Some(SpineTreePlanItemStatus::Pending),
                                checkpoints: vec![
                                    codex_protocol::spine_tree::SpineTreePlanCheckpointSnapshot {
                                        task: "Run future validation".to_string(),
                                        status: SpineTreePlanItemStatus::Pending,
                                    },
                                ],
                                children: Vec::new(),
                            }],
                        },
                    }),
                    items: vec![SpineTreePlanItemSnapshot {
                        stable_task_id: "step-1".to_string(),
                        step: "Inspect current node".to_string(),
                        status: SpineTreePlanItemStatus::InProgress,
                    }],
                }),
            }],
        }
    }
}

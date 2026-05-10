use crate::function_tool::FunctionCallError;
use crate::spine::store::SpineOperation;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::spine_spec::create_spine_tool;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value as JsonValue;

pub struct SpineHandler;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpineArgs {
    op: SpineOperation,
    summary: String,
    worklog: String,
}

#[derive(Debug)]
pub struct SpineToolOutput {
    cursor: String,
    visible_spine: String,
}

impl ToolOutput for SpineToolOutput {
    fn log_preview(&self) -> String {
        self.output_text()
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
        serde_json::json!({
            "cursor": self.cursor.clone(),
            "visible_spine": self.visible_spine.clone(),
        })
    }
}

impl SpineToolOutput {
    fn output_text(&self) -> String {
        format!(
            "Spine updated.\nSpine cursor: {}\nVisible spine: {}\nNext: continue work in {}.",
            self.cursor, self.visible_spine, self.cursor
        )
    }
}

impl ToolHandler for SpineHandler {
    type Output = SpineToolOutput;

    fn tool_name(&self) -> ToolName {
        ToolName::plain("spine")
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(create_spine_tool())
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "spine handler received unsupported payload".to_string(),
                ));
            }
        };

        if turn.collaboration_mode.mode == ModeKind::Plan {
            return Err(FunctionCallError::RespondToModel(
                "spine is not allowed in Plan mode".to_string(),
            ));
        }

        let args: SpineArgs = parse_arguments(&arguments)?;
        let spine = session.spine.as_ref().ok_or_else(|| {
            FunctionCallError::RespondToModel("spine task tree is not enabled".to_string())
        })?;

        let (cursor, visible_spine) = {
            let mut runtime = spine.lock().await;
            let staged = runtime
                .stage_transition(
                    call_id,
                    turn.sub_id.clone(),
                    args.op,
                    args.summary,
                    args.worklog,
                )
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            (
                staged.to_node.bracketed(),
                format_visible_spine(&staged.visible_spine),
            )
        };

        Ok(SpineToolOutput {
            cursor,
            visible_spine,
        })
    }
}

fn format_visible_spine(visible_spine: &[crate::spine::ids::NodeId]) -> String {
    let nodes = visible_spine
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{nodes}]")
}

#[cfg(test)]
#[path = "spine_tests.rs"]
mod tests;

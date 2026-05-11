use crate::function_tool::FunctionCallError;
use crate::spine::store::SpineOperation;
use crate::spine::view::render_tool_output;
use crate::spine::view::render_tree;
use crate::tools::context::ToolCallSource;
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
}

#[derive(Debug)]
pub struct SpineToolOutput {
    op: SpineOperation,
    cursor: String,
    tree: String,
    text: String,
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
            "op": self.op,
            "cursor": self.cursor.clone(),
            "tree": self.tree.clone(),
        })
    }
}

impl SpineToolOutput {
    fn output_text(&self) -> String {
        self.text.clone()
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
            source,
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
        if matches!(source, ToolCallSource::CodeMode { .. }) {
            return Err(FunctionCallError::RespondToModel(
                "spine is not available as a Code Mode nested tool".to_string(),
            ));
        }

        let args: SpineArgs = parse_arguments(&arguments)?;
        let spine = session.spine.as_ref().ok_or_else(|| {
            FunctionCallError::RespondToModel("spine task tree is not enabled".to_string())
        })?;

        let (op, cursor, tree, text) = {
            let mut runtime = spine.lock().await;
            let mut preview_state = runtime.state().clone();
            args.op
                .apply(&mut preview_state, args.summary.clone())
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            let staged = runtime
                .stage_transition(call_id, turn.sub_id.clone(), args.op, args.summary)
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            if preview_state.node(&staged.to_node).is_none() {
                return Err(FunctionCallError::RespondToModel(format!(
                    "spine transition produced unknown node {}",
                    staged.to_node.bracketed()
                )));
            }
            (
                staged.op,
                staged.to_node.bracketed(),
                render_tree(&preview_state, &staged.to_node),
                render_tool_output(staged.op, &preview_state, &staged.to_node),
            )
        };

        Ok(SpineToolOutput {
            op,
            cursor,
            tree,
            text,
        })
    }
}

#[cfg(test)]
#[path = "spine_tests.rs"]
mod tests;

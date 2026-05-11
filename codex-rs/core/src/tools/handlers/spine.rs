use crate::function_tool::FunctionCallError;
use crate::spine::ids::NodeId;
use crate::spine::state::NodeStatus;
use crate::spine::state::SpineState;
use crate::spine::store::SpineOperation;
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
    cursor_status: String,
    tree: String,
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
        format!(
            "Spine updated: {}\n\ncurrent: {} {}\n\n{}",
            format_op(self.op),
            self.cursor,
            self.cursor_status,
            self.tree
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

        let (op, cursor, cursor_status, tree) = {
            let mut runtime = spine.lock().await;
            let mut preview_state = runtime.state().clone();
            args.op
                .apply(&mut preview_state, args.summary.clone())
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            let staged = runtime
                .stage_transition(call_id, turn.sub_id.clone(), args.op, args.summary)
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            let cursor_status = preview_state
                .node(&staged.to_node)
                .map(|node| format_status(&node.status).to_string())
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(format!(
                        "spine transition produced unknown node {}",
                        staged.to_node.bracketed()
                    ))
                })?;
            (
                staged.op,
                staged.to_node.bracketed(),
                cursor_status,
                format_tree(&preview_state, &staged.to_node),
            )
        };

        Ok(SpineToolOutput {
            op,
            cursor,
            cursor_status,
            tree,
        })
    }
}

fn format_tree(state: &SpineState, cursor: &NodeId) -> String {
    state
        .nodes()
        .iter()
        .filter(|(_, node)| node.parent_id.is_none())
        .map(|(node_id, _)| format_subtree(state, node_id, cursor, 0, true))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_subtree(
    state: &SpineState,
    node_id: &NodeId,
    cursor: &NodeId,
    depth: usize,
    is_root: bool,
) -> String {
    let node = state
        .node(node_id)
        .expect("formatting an existing spine node");
    let prefix = if is_root {
        String::new()
    } else {
        format!("{}|-- ", "    ".repeat(depth.saturating_sub(1)))
    };
    let summary = node
        .summary
        .as_deref()
        .or_else(|| (node_id == cursor).then_some("current"))
        .unwrap_or("");
    let mut line = format!(
        "{}{} {}",
        prefix,
        node_id.bracketed(),
        format_status(&node.status)
    );
    if !summary.is_empty() {
        line.push(' ');
        line.push_str(summary);
    }
    if node_id == cursor && summary != "current" {
        line.push_str(" current");
    }
    line.push_str(&format!(" ({})", relative_worklog_path(node_id)));

    let child_depth = depth + 1;
    let children = state
        .nodes()
        .iter()
        .filter(|(_, child)| child.parent_id.as_ref() == Some(node_id))
        .map(|(child_id, _)| format_subtree(state, child_id, cursor, child_depth, false))
        .collect::<Vec<_>>();
    if children.is_empty() {
        line
    } else {
        format!("{line}\n{}", children.join("\n"))
    }
}

fn format_status(status: &NodeStatus) -> &'static str {
    match status {
        NodeStatus::Live => "live",
        NodeStatus::Opened => "opened",
        NodeStatus::Finished => "finished",
        NodeStatus::Closed => "closed",
    }
}

fn format_op(op: SpineOperation) -> &'static str {
    match op {
        SpineOperation::Open => "open",
        SpineOperation::Next => "next",
        SpineOperation::Close => "close",
    }
}

fn relative_worklog_path(node_id: &NodeId) -> String {
    let mut parts = vec!["nodes".to_string()];
    parts.extend(node_id.segments().iter().map(ToString::to_string));
    parts.push("worklog.md".to_string());
    parts.join("/")
}

#[cfg(test)]
#[path = "spine_tests.rs"]
mod tests;

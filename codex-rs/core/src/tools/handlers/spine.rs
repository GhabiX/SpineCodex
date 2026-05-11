use crate::function_tool::FunctionCallError;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_NEXT;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use crate::spine::store::SpineOperation;
use crate::spine::view::render_tool_output;
use crate::spine::view::render_tree;
use crate::spine::view::render_tree_tool_output;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value as JsonValue;

pub struct SpineHandler {
    tool: SpineTool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpineTool {
    Tree,
    Open,
    Next,
    Close,
}

impl SpineHandler {
    pub(crate) fn all() -> Vec<Self> {
        vec![
            Self {
                tool: SpineTool::Tree,
            },
            Self {
                tool: SpineTool::Open,
            },
            Self {
                tool: SpineTool::Next,
            },
            Self {
                tool: SpineTool::Close,
            },
        ]
    }
}

impl SpineTool {
    fn name(self) -> &'static str {
        match self {
            SpineTool::Tree => SPINE_TOOL_TREE,
            SpineTool::Open => SPINE_TOOL_OPEN,
            SpineTool::Next => SPINE_TOOL_NEXT,
            SpineTool::Close => SPINE_TOOL_CLOSE,
        }
    }

    fn op(self) -> Option<SpineOperation> {
        match self {
            SpineTool::Tree => None,
            SpineTool::Open => Some(SpineOperation::Open),
            SpineTool::Next => Some(SpineOperation::Next),
            SpineTool::Close => Some(SpineOperation::Close),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpineTransitionArgs {
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpineTreeArgs {}

#[derive(Debug)]
pub struct SpineToolOutput {
    op: Option<SpineOperation>,
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
        ToolName::namespaced(SPINE_NAMESPACE, self.tool.name())
    }

    fn spec(&self) -> Option<ToolSpec> {
        None
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

        if self.tool != SpineTool::Tree && turn.collaboration_mode.mode == ModeKind::Plan {
            return Err(FunctionCallError::RespondToModel(
                "spine is not allowed in Plan mode".to_string(),
            ));
        }
        if matches!(source, ToolCallSource::CodeMode { .. }) {
            return Err(FunctionCallError::RespondToModel(
                "spine is not available as a Code Mode nested tool".to_string(),
            ));
        }

        let spine = session.spine.as_ref().ok_or_else(|| {
            FunctionCallError::RespondToModel("spine task tree is not enabled".to_string())
        })?;

        if self.tool == SpineTool::Tree {
            let _args: SpineTreeArgs = parse_arguments(&arguments)?;
            let (cursor, tree, text) = {
                let runtime = spine.lock().await;
                let cursor = runtime.cursor().clone();
                (
                    cursor.bracketed(),
                    render_tree(runtime.state(), &cursor),
                    render_tree_tool_output(runtime.state(), &cursor),
                )
            };
            return Ok(SpineToolOutput {
                op: None,
                cursor,
                tree,
                text,
            });
        }

        let args: SpineTransitionArgs = parse_arguments(&arguments)?;
        let op = self
            .tool
            .op()
            .expect("tree returned before transition handling");
        let (op, cursor, tree, text) = {
            let mut runtime = spine.lock().await;
            let mut preview_state = runtime.state().clone();
            op.apply(&mut preview_state, args.summary.clone())
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
            let staged = runtime
                .stage_transition(call_id, turn.sub_id.clone(), op, args.summary)
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
            op: Some(op),
            cursor,
            tree,
            text,
        })
    }
}

#[cfg(test)]
#[path = "spine_tests.rs"]
mod tests;

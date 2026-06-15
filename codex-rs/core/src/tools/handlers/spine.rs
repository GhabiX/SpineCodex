use crate::function_tool::FunctionCallError;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_NEXT;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use crate::spine::SPINE_TOOL_TRIM;
use crate::spine::SpineTrimOutcome;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::spine_spec::create_spine_namespace_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::config_types::ModeKind;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;

pub(crate) struct SpineHandler {
    tool: SpineTool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpineTool {
    Tree,
    Trim,
    Open,
    Close,
    Next,
}

impl SpineHandler {
    pub(crate) fn all() -> Vec<Self> {
        vec![
            Self {
                tool: SpineTool::Tree,
            },
            Self {
                tool: SpineTool::Trim,
            },
            Self {
                tool: SpineTool::Open,
            },
            Self {
                tool: SpineTool::Close,
            },
            Self {
                tool: SpineTool::Next,
            },
        ]
    }
}

impl SpineTool {
    fn name(self) -> &'static str {
        match self {
            SpineTool::Tree => SPINE_TOOL_TREE,
            SpineTool::Trim => SPINE_TOOL_TRIM,
            SpineTool::Open => SPINE_TOOL_OPEN,
            SpineTool::Close => SPINE_TOOL_CLOSE,
            SpineTool::Next => SPINE_TOOL_NEXT,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    #[serde(default)]
    instruction: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NextArgs {
    summary: String,
    #[serde(default)]
    instruction: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrimArgs {
    #[serde(rename = "TRIM_ID")]
    trim_id: String,
    op: String,
}

fn normalize_trim_args(mut args: TrimArgs) -> Result<TrimArgs, FunctionCallError> {
    args.trim_id = args.trim_id.trim().to_string();
    if args.trim_id.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "spine.trim requires a non-empty TRIM_ID.".to_string(),
        ));
    }
    Ok(args)
}

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for SpineHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(SPINE_NAMESPACE, self.tool.name())
    }

    fn spec(&self) -> Option<ToolSpec> {
        (self.tool == SpineTool::Tree).then(create_spine_namespace_tool)
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            payload,
            source,
            ..
        } = invocation;
        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(
                "spine handler received unsupported payload".to_string(),
            ));
        };
        if matches!(
            source,
            crate::tools::context::ToolCallSource::CodeMode { .. }
        ) {
            return Err(FunctionCallError::RespondToModel(
                "spine is not available as a Code Mode nested tool".to_string(),
            ));
        }
        if self.tool != SpineTool::Tree && turn.collaboration_mode.mode == ModeKind::Plan {
            return Err(FunctionCallError::RespondToModel(
                "spine.trim, spine.open, spine.close, and spine.next are not allowed in Plan mode"
                    .to_string(),
            ));
        }
        match self.tool {
            SpineTool::Tree => {
                let _args: EmptyArgs = parse_arguments(&arguments)?;
                let tree = session
                    .spine_tree()
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                session
                    .emit_spine_tree_snapshot(turn.as_ref())
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    tree,
                    Some(true),
                )))
            }
            SpineTool::Trim => {
                let args: TrimArgs = normalize_trim_args(parse_arguments(&arguments)?)?;
                if args.op != "snip" {
                    return Err(FunctionCallError::RespondToModel(
                        "spine.trim only supports op=\"snip\".".to_string(),
                    ));
                }
                let outcome = session
                    .trim_spine_tool_response(args.trim_id)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                let message = match outcome {
                    SpineTrimOutcome::Cleared { trim_id } => {
                        format!("Trimmed tool response {trim_id}.")
                    }
                    SpineTrimOutcome::AlreadyCleared { trim_id } => {
                        format!("Tool response {trim_id} was already cleared.")
                    }
                    SpineTrimOutcome::Miss { trim_id } => {
                        format!(
                            "Could not find trim id {trim_id} in the previous completed toolcall. Do not retry this TRIM_ID."
                        )
                    }
                };
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    message,
                    Some(true),
                )))
            }
            SpineTool::Open => {
                let args: OpenArgs = parse_arguments(&arguments)?;
                session
                    .stage_spine_open(call_id, args.summary)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    "Spine opened after this tool output is recorded.".to_string(),
                    Some(true),
                )))
            }
            SpineTool::Close => {
                let args: CloseArgs = parse_arguments(&arguments)?;
                session
                    .stage_spine_close(call_id, args.instruction)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    "Spine closed.".to_string(),
                    Some(true),
                )))
            }
            SpineTool::Next => {
                let args: NextArgs = parse_arguments(&arguments)?;
                session
                    .stage_spine_next(call_id, args.summary, args.instruction)
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    "Spine will close the current node, then continue in a new sibling after this tool output is recorded."
                        .to_string(),
                    Some(true),
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_args_reject_empty_trim_id() {
        let err = match normalize_trim_args(TrimArgs {
            trim_id: " \n\t ".to_string(),
            op: "snip".to_string(),
        }) {
            Ok(_) => panic!("empty TRIM_ID should be rejected"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-visible trim argument error");
        };
        assert!(message.contains("TRIM_ID"));
    }

    #[test]
    fn trim_args_trim_surrounding_whitespace() {
        let args = normalize_trim_args(TrimArgs {
            trim_id: " trim_0 \n".to_string(),
            op: "snip".to_string(),
        })
        .expect("non-empty TRIM_ID should be accepted");
        assert_eq!(args.trim_id, "trim_0");
    }
}

impl CoreToolRuntime for SpineHandler {}

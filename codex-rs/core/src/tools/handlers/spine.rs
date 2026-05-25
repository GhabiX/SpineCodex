use crate::function_tool::FunctionCallError;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
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
    Open,
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
            SpineTool::Close => SPINE_TOOL_CLOSE,
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
                "spine.open and spine.close are not allowed in Plan mode".to_string(),
            ));
        }
        match self.tool {
            SpineTool::Tree => {
                let _args: EmptyArgs = parse_arguments(&arguments)?;
                let tree = session
                    .spine_tree()
                    .await
                    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    tree,
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
        }
    }
}

impl CoreToolRuntime for SpineHandler {}

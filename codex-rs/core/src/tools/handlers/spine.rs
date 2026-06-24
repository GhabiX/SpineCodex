use crate::function_tool::FunctionCallError;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_NEXT;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use crate::spine::SPINE_TOOL_TRIM;
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
    pub(crate) fn all(include_jit_tools: bool, include_trim_tool: bool) -> Vec<Self> {
        let mut handlers = Vec::new();
        if include_jit_tools {
            handlers.extend([
                Self {
                    tool: SpineTool::Tree,
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
            ]);
        }
        if include_trim_tool {
            handlers.push(Self {
                tool: SpineTool::Trim,
            });
        }
        handlers
    }

    fn namespace_spec_options(&self) -> Option<(bool, bool)> {
        match self.tool {
            SpineTool::Tree => Some((true, false)),
            SpineTool::Trim => Some((false, true)),
            SpineTool::Open | SpineTool::Close | SpineTool::Next => None,
        }
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
struct TreeArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    memory: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NextArgs {
    summary: String,
    memory: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrimArgs {
    #[serde(rename = "TRIM_ID")]
    trim_id: String,
    op: String,
    #[serde(default)]
    head: Option<usize>,
    #[serde(default)]
    tail: Option<usize>,
    #[serde(default)]
    anchor: Option<String>,
    #[serde(default)]
    preceding: Option<usize>,
    #[serde(default)]
    following: Option<usize>,
    #[serde(skip)]
    request: TrimRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
enum TrimRequest {
    #[default]
    Snip,
    SliceHead {
        head: usize,
    },
    SliceTail {
        tail: usize,
    },
    SliceAnchor {
        anchor: String,
        preceding: usize,
        following: usize,
    },
}

fn normalize_trim_args(mut args: TrimArgs) -> Result<TrimArgs, FunctionCallError> {
    args.trim_id = args.trim_id.trim().to_string();
    if args.trim_id.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "spine.trim requires a non-empty TRIM_ID.".to_string(),
        ));
    }
    args.request = match args.op.as_str() {
        "snip" => {
            if args.head.is_some()
                || args.tail.is_some()
                || args.anchor.is_some()
                || args.preceding.is_some()
                || args.following.is_some()
            {
                return Err(FunctionCallError::RespondToModel(
                    "spine.trim op=\"snip\" does not accept slice parameters.".to_string(),
                ));
            }
            TrimRequest::Snip
        }
        "slice" => {
            let has_head = args.head.is_some();
            let has_tail = args.tail.is_some();
            let has_anchor = args.anchor.is_some();
            let shape_count =
                usize::from(has_head) + usize::from(has_tail) + usize::from(has_anchor);
            if shape_count != 1 {
                return Err(FunctionCallError::RespondToModel(
                    "spine.trim slice requires exactly one slice shape: head, tail, or anchor with preceding/following.".to_string(),
                ));
            }
            if let Some(head) = args.head {
                if args.preceding.is_some() || args.following.is_some() {
                    return Err(FunctionCallError::RespondToModel(
                        "spine.trim slice head must not include anchor window fields.".to_string(),
                    ));
                }
                TrimRequest::SliceHead { head }
            } else if let Some(tail) = args.tail {
                if args.preceding.is_some() || args.following.is_some() {
                    return Err(FunctionCallError::RespondToModel(
                        "spine.trim slice tail must not include anchor window fields.".to_string(),
                    ));
                }
                TrimRequest::SliceTail { tail }
            } else {
                let anchor = args.anchor.take().unwrap_or_default().trim().to_string();
                if anchor.is_empty() {
                    return Err(FunctionCallError::RespondToModel(
                        "spine.trim slice anchor must be non-empty.".to_string(),
                    ));
                }
                let Some(preceding) = args.preceding else {
                    return Err(FunctionCallError::RespondToModel(
                        "spine.trim slice anchor requires preceding.".to_string(),
                    ));
                };
                let Some(following) = args.following else {
                    return Err(FunctionCallError::RespondToModel(
                        "spine.trim slice anchor requires following.".to_string(),
                    ));
                };
                TrimRequest::SliceAnchor {
                    anchor,
                    preceding,
                    following,
                }
            }
        }
        other => {
            return Err(FunctionCallError::RespondToModel(format!(
                "spine.trim unsupported op={other:?}; use \"snip\" or \"slice\"."
            )));
        }
    };
    Ok(args)
}

fn normalize_memory_arg(memory: String, tool_name: &str) -> Result<String, FunctionCallError> {
    let memory = memory.trim().to_string();
    if memory.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "{tool_name} requires a non-empty memory."
        )));
    }
    Ok(memory)
}

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for SpineHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::namespaced(SPINE_NAMESPACE, self.tool.name())
    }

    fn spec(&self) -> Option<ToolSpec> {
        self.namespace_spec_options()
            .map(|(include_jit_tools, include_trim_tool)| {
                create_spine_namespace_tool(include_jit_tools, include_trim_tool)
            })
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id: _,
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
        if !matches!(self.tool, SpineTool::Tree) && turn.collaboration_mode.mode == ModeKind::Plan {
            return Err(FunctionCallError::RespondToModel(
                "spine.trim, spine.open, spine.close, and spine.next are not allowed in Plan mode"
                    .to_string(),
            ));
        }
        match self.tool {
            SpineTool::Tree => {
                let _args: TreeArgs = parse_arguments(&arguments)?;
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
                let outcome = match args.request {
                    TrimRequest::Snip => session.trim_spine_tool_response(args.trim_id).await,
                    TrimRequest::SliceHead { head } => {
                        session
                            .slice_spine_tool_response_head(args.trim_id, head)
                            .await
                    }
                    TrimRequest::SliceTail { tail } => {
                        session
                            .slice_spine_tool_response_tail(args.trim_id, tail)
                            .await
                    }
                    TrimRequest::SliceAnchor {
                        anchor,
                        preceding,
                        following,
                    } => {
                        session
                            .slice_spine_tool_response_anchor(
                                args.trim_id,
                                anchor,
                                preceding,
                                following,
                            )
                            .await
                    }
                }
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    outcome.model_response_message(),
                    Some(true),
                )))
            }
            SpineTool::Open => {
                let OpenArgs { summary: _summary } = parse_arguments(&arguments)?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    "Spine open accepted.".to_string(),
                    Some(true),
                )))
            }
            SpineTool::Close => {
                let args: CloseArgs = parse_arguments(&arguments)?;
                let _memory = normalize_memory_arg(args.memory, "spine.close")?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    "Spine close accepted.".to_string(),
                    Some(true),
                )))
            }
            SpineTool::Next => {
                let NextArgs {
                    summary: _summary,
                    memory,
                } = parse_arguments(&arguments)?;
                let _memory = normalize_memory_arg(memory, "spine.next")?;
                Ok(boxed_tool_output(FunctionToolOutput::from_text(
                    "Spine next accepted.".to_string(),
                    Some(true),
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trim_args(trim_id: &str, op: &str) -> TrimArgs {
        TrimArgs {
            trim_id: trim_id.to_string(),
            op: op.to_string(),
            head: None,
            tail: None,
            anchor: None,
            preceding: None,
            following: None,
            request: TrimRequest::Snip,
        }
    }

    #[test]
    fn trim_args_reject_empty_trim_id() {
        let err = match normalize_trim_args(trim_args(" \n\t ", "snip")) {
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
        let args = normalize_trim_args(trim_args(" trim_0 \n", "snip"))
            .expect("non-empty TRIM_ID should be accepted");
        assert_eq!(args.trim_id, "trim_0");
    }

    #[test]
    fn trim_args_accept_slice_head_shape() {
        let args = normalize_trim_args(TrimArgs {
            trim_id: "trim_0".to_string(),
            op: "slice".to_string(),
            head: Some(12),
            tail: None,
            anchor: None,
            preceding: None,
            following: None,
            request: TrimRequest::Snip,
        })
        .expect("head slice should be accepted");
        assert!(matches!(args.request, TrimRequest::SliceHead { head: 12 }));
    }

    #[test]
    fn trim_args_accept_slice_tail_shape() {
        let args = normalize_trim_args(TrimArgs {
            trim_id: "trim_0".to_string(),
            op: "slice".to_string(),
            head: None,
            tail: Some(8),
            anchor: None,
            preceding: None,
            following: None,
            request: TrimRequest::Snip,
        })
        .expect("tail slice should be accepted");
        assert!(matches!(args.request, TrimRequest::SliceTail { tail: 8 }));
    }

    #[test]
    fn trim_args_accept_slice_anchor_shape() {
        let args = normalize_trim_args(TrimArgs {
            trim_id: "trim_0".to_string(),
            op: "slice".to_string(),
            head: None,
            tail: None,
            anchor: Some("needle".to_string()),
            preceding: Some(3),
            following: Some(5),
            request: TrimRequest::Snip,
        })
        .expect("anchor slice should be accepted");
        assert!(matches!(
            args.request,
            TrimRequest::SliceAnchor {
                ref anchor,
                preceding: 3,
                following: 5
            } if anchor == "needle"
        ));
    }

    #[test]
    fn trim_args_reject_snip_with_slice_fields() {
        let err = match normalize_trim_args(TrimArgs {
            trim_id: "trim_0".to_string(),
            op: "snip".to_string(),
            head: Some(12),
            tail: None,
            anchor: None,
            preceding: None,
            following: None,
            request: TrimRequest::Snip,
        }) {
            Ok(_) => panic!("snip with slice fields should be rejected"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-visible trim argument error");
        };
        assert!(message.contains("does not accept slice parameters"));
    }

    #[test]
    fn trim_args_reject_mixed_slice_shape() {
        let err = match normalize_trim_args(TrimArgs {
            trim_id: "trim_0".to_string(),
            op: "slice".to_string(),
            head: Some(12),
            tail: Some(5),
            anchor: None,
            preceding: None,
            following: None,
            request: TrimRequest::Snip,
        }) {
            Ok(_) => panic!("mixed slice shape should be rejected"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-visible trim argument error");
        };
        assert!(message.contains("exactly one slice shape"));
    }

    #[test]
    fn trim_args_reject_anchor_slice_without_window() {
        let err = match normalize_trim_args(TrimArgs {
            trim_id: "trim_0".to_string(),
            op: "slice".to_string(),
            head: None,
            tail: None,
            anchor: Some("needle".to_string()),
            preceding: Some(3),
            following: None,
            request: TrimRequest::Snip,
        }) {
            Ok(_) => panic!("anchor slice without following should be rejected"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-visible trim argument error");
        };
        assert!(message.contains("requires following"));
    }

    #[test]
    fn trim_args_reject_empty_anchor_slice() {
        let err = match normalize_trim_args(TrimArgs {
            trim_id: "trim_0".to_string(),
            op: "slice".to_string(),
            head: None,
            tail: None,
            anchor: Some(" \n ".to_string()),
            preceding: Some(0),
            following: Some(0),
            request: TrimRequest::Snip,
        }) {
            Ok(_) => panic!("empty anchor slice should be rejected"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-visible trim argument error");
        };
        assert!(message.contains("anchor must be non-empty"));
    }

    #[test]
    fn trim_args_reject_unknown_op() {
        let err = match normalize_trim_args(TrimArgs {
            trim_id: "trim_0".to_string(),
            op: "delete".to_string(),
            head: None,
            tail: None,
            anchor: None,
            preceding: None,
            following: None,
            request: TrimRequest::Snip,
        }) {
            Ok(_) => panic!("unknown trim op should be rejected"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-visible trim argument error");
        };
        assert!(message.contains("unsupported op"));
    }

    #[test]
    fn close_memory_rejects_empty_content() {
        let err = match normalize_memory_arg(" \n\t ".to_string(), "spine.close") {
            Ok(_) => panic!("empty close memory should be rejected"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-visible close memory argument error");
        };
        assert!(message.contains("non-empty memory"));
    }

    #[test]
    fn close_memory_trims_surrounding_whitespace() {
        let memory = normalize_memory_arg(" node memory \n".to_string(), "spine.close")
            .expect("non-empty memory should be accepted");
        assert_eq!(memory, "node memory");
    }

    #[test]
    fn next_memory_rejects_empty_content() {
        let err = match normalize_memory_arg("".to_string(), "spine.next") {
            Ok(_) => panic!("empty next memory should be rejected"),
            Err(err) => err,
        };
        let FunctionCallError::RespondToModel(message) = err else {
            panic!("expected model-visible next memory argument error");
        };
        assert!(message.contains("non-empty memory"));
    }
}

impl CoreToolRuntime for SpineHandler {}

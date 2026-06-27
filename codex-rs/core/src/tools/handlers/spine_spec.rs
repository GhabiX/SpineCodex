use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_NEXT;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use crate::spine::SPINE_TOOL_TRIM;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn create_spine_namespace_tool(
    include_jit_tools: bool,
    include_trim_tool: bool,
) -> ToolSpec {
    let mut tools = Vec::new();
    if include_jit_tools {
        tools.push(ResponsesApiNamespaceTool::Function(spine_tree_tool()));
    }
    if include_trim_tool {
        tools.push(ResponsesApiNamespaceTool::Function(spine_trim_tool()));
    }
    if include_jit_tools {
        tools.extend([
            ResponsesApiNamespaceTool::Function(ResponsesApiTool {
                name: SPINE_TOOL_OPEN.to_string(),
                description: "Start a focused child node under the current Spine cursor."
                    .to_string(),
                strict: false,
                defer_loading: None,
                parameters: JsonSchema::object(
                    BTreeMap::from([(
                        "summary".to_string(),
                        JsonSchema::string(Some(
                            "Short label for the newly opened child node.".to_string(),
                        )),
                    )]),
                    Some(vec!["summary".to_string()]),
                    Some(false.into()),
                ),
                output_schema: None,
            }),
            ResponsesApiNamespaceTool::Function(spine_close_tool()),
            ResponsesApiNamespaceTool::Function(spine_next_tool()),
        ]);
    }
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: SPINE_NAMESPACE.to_string(),
        description: "Inspect and move the Spine task tree.".to_string(),
        tools,
    })
}

fn spine_tree_tool() -> ResponsesApiTool {
    ResponsesApiTool {
        name: SPINE_TOOL_TREE.to_string(),
        description: "Inspect the current Spine tree, cursor, and context status.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(BTreeMap::new(), Some(Vec::new()), Some(false.into())),
        output_schema: None,
    }
}

fn spine_trim_tool() -> ResponsesApiTool {
    let properties = BTreeMap::from([
        (
            "TRIM_ID".to_string(),
            JsonSchema::string(Some(
                "Trim id attached to a tool response in the immediately previous tool-result batch; it expires after your next assistant tool request."
                    .to_string(),
            )),
        ),
        (
            "op".to_string(),
            JsonSchema::string_enum(
                vec![json!("snip"), json!("slice")],
                Some("Use snip only when useful facts are preserved elsewhere; use slice to keep the needed head, tail, or anchor window.".to_string()),
            ),
        ),
        (
            "head".to_string(),
            JsonSchema::integer(Some(
                "For op=\"slice\", keep this many characters from the start of the current visible body. Mutually exclusive with tail and anchor."
                    .to_string(),
            )),
        ),
        (
            "tail".to_string(),
            JsonSchema::integer(Some(
                "For op=\"slice\", keep this many characters from the end of the current visible body. Mutually exclusive with head and anchor."
                    .to_string(),
            )),
        ),
        (
            "anchor".to_string(),
            JsonSchema::string(Some(
                "For op=\"slice\", locate this non-empty text in the current visible body and keep an anchor window. Mutually exclusive with head and tail."
                    .to_string(),
            )),
        ),
        (
            "preceding".to_string(),
            JsonSchema::integer(Some(
                "For anchor slice, keep this many characters before the anchor.".to_string(),
            )),
        ),
        (
            "following".to_string(),
            JsonSchema::integer(Some(
                "For anchor slice, keep this many characters after the anchor end.".to_string(),
            )),
        ),
    ]);
    ResponsesApiTool {
        name: SPINE_TOOL_TRIM.to_string(),
        description: "Conservatively clean up one tagged tool-response projection; this never changes the Spine tree or creates memory. A TRIM_ID is live only for the immediately previous tool-result batch, and only in your next assistant tool request. After any later tool request it expires; if trim misses, treat the id as expired and continue. Use slice for needed visible evidence, snip only when useful facts are preserved elsewhere, and leave untrimmed if the original may still be needed.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["TRIM_ID".to_string(), "op".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    }
}

fn spine_close_tool() -> ResponsesApiTool {
    let properties = BTreeMap::from([(
        "memory".to_string(),
        JsonSchema::string(Some(
            "Continuation memory produced when finishing the current Spine node. Summarize current progress, key decisions, evidence, constraints, unresolved risks, remaining next steps, critical files/tests/references, and the final state of relevant user requests. Use [U#] anchors when referring to preserved user messages.".to_string(),
        )),
    )]);
    ResponsesApiTool {
        name: SPINE_TOOL_CLOSE.to_string(),
        description:
            "Finish the current Spine node and return its continuation memory to the parent."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["memory".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    }
}

fn spine_next_tool() -> ResponsesApiTool {
    let properties = BTreeMap::from([
        (
            "summary".to_string(),
            JsonSchema::string(Some(
                "Short label for the newly opened sibling node.".to_string(),
            )),
        ),
        (
            "memory".to_string(),
            JsonSchema::string(Some(
                "Continuation memory produced when finishing the current Spine node before opening the sibling. Summarize current progress, key decisions, evidence, constraints, unresolved risks, remaining next steps, critical files/tests/references, and the final state of relevant user requests. Use [U#] anchors when referring to preserved user messages."
                    .to_string(),
            )),
        ),
    ]);
    ResponsesApiTool {
        name: SPINE_TOOL_NEXT.to_string(),
        description:
            "Finish the current Spine node and start a new sibling under the resumed parent."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["summary".to_string(), "memory".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    }
}

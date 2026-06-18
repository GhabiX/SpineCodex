use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_FEEDBACK;
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
    include_feedback_tool: bool,
) -> ToolSpec {
    let mut tools = Vec::new();
    if include_jit_tools {
        tools.push(ResponsesApiNamespaceTool::Function(ResponsesApiTool {
            name: SPINE_TOOL_TREE.to_string(),
            description: "Inspect the current Spine tree, cursor, current live-node context pressure, and overall context-window pressure without moving the cursor.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::new(),
                Some(Vec::new()),
                Some(false.into()),
            ),
            output_schema: None,
        }));
    }
    if include_trim_tool {
        tools.push(ResponsesApiNamespaceTool::Function(spine_trim_tool()));
    }
    if include_jit_tools {
        tools.extend([
            ResponsesApiNamespaceTool::Function(ResponsesApiTool {
                name: SPINE_TOOL_OPEN.to_string(),
                description: "Open a focused child task under the current Spine cursor."
                    .to_string(),
                strict: false,
                defer_loading: None,
                parameters: JsonSchema::object(
                    BTreeMap::from([(
                        "summary".to_string(),
                        JsonSchema::string(Some(
                            "Short label for the new Spine task node.".to_string(),
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
    if include_feedback_tool {
        tools.push(ResponsesApiNamespaceTool::Function(spine_feedback_tool()));
    }

    ToolSpec::Namespace(ResponsesApiNamespace {
        name: SPINE_NAMESPACE.to_string(),
        description: "Inspect and move the Spine task tree.".to_string(),
        tools,
    })
}

fn spine_feedback_tool() -> ResponsesApiTool {
    let properties = BTreeMap::from([(
        "content".to_string(),
        JsonSchema::string(Some(
            "Concise feedback for Spine developers about a problem, rough edge, confusing behavior, missing capability, or concrete improvement idea noticed during real work.".to_string(),
        )),
    )]);
    ResponsesApiTool {
        name: SPINE_TOOL_FEEDBACK.to_string(),
        description: "Record debug-only feedback for Spine developers when real work reveals a Spine problem, rough edge, confusing behavior, missing capability, or concrete improvement idea. When the feedback is relevant to the ongoing collaboration, also mention the same issue or improvement idea in your ordinary assistant reply; do not rely on the tool call or tool response as the user-visible communication. Do not use this for normal task notes, user-facing summaries, or project implementation logs.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["content".to_string()]), Some(false.into())),
        output_schema: None,
    }
}

fn spine_trim_tool() -> ResponsesApiTool {
    let properties = BTreeMap::from([
        (
            "TRIM_ID".to_string(),
            JsonSchema::string(Some(
                "Trim id attached to a tool response in the previous completed toolcall."
                    .to_string(),
            )),
        ),
        (
            "op".to_string(),
            JsonSchema::string_enum(
                vec![json!("snip"), json!("slice")],
                Some("Use snip to replace the tagged tool response body, or slice to keep a local part of it.".to_string()),
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
        description: "Conservatively clean up one tagged tool response from the previous completed toolcall: snip replaces it with a cleared placeholder; slice keeps only a sufficient local part.".to_string(),
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
            "Required Node Memory body for closing the current Spine task. Include stable continuation facts, decisions, evidence, unresolved risks, and next actions. Do not include runtime-owned Spine Memory/User Message/Child Memory headings.".to_string(),
        )),
    )]);
    ResponsesApiTool {
        name: SPINE_TOOL_CLOSE.to_string(),
        description: "Close the current Spine task node with model-authored Node Memory and resume its parent.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["memory".to_string()]), Some(false.into())),
        output_schema: None,
    }
}

fn spine_next_tool() -> ResponsesApiTool {
    let properties = BTreeMap::from([
        (
            "summary".to_string(),
            JsonSchema::string(Some(
                "Short label for the next sibling Spine task node.".to_string(),
            )),
        ),
        (
            "memory".to_string(),
            JsonSchema::string(Some(
                "Required Node Memory body for closing the current Spine task before opening the sibling. Do not include runtime-owned Spine Memory/User Message/Child Memory headings."
                    .to_string(),
            )),
        ),
    ]);
    ResponsesApiTool {
        name: SPINE_TOOL_NEXT.to_string(),
        description: "Close the current node with model-authored Node Memory, then continue in a new sibling under the resumed parent.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["summary".to_string(), "memory".to_string()]), Some(false.into())),
        output_schema: None,
    }
}

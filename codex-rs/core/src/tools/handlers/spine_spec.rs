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

pub(crate) fn create_spine_namespace_tool() -> ToolSpec {
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: SPINE_NAMESPACE.to_string(),
        description: "Inspect and move the Spine task tree.".to_string(),
        tools: vec![
            ResponsesApiNamespaceTool::Function(ResponsesApiTool {
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
            }),
            ResponsesApiNamespaceTool::Function(spine_trim_tool()),
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
        ],
    })
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
                vec![json!("snip")],
                Some("Use snip to replace the tagged tool response body.".to_string()),
            ),
        ),
    ]);
    ResponsesApiTool {
        name: SPINE_TOOL_TRIM.to_string(),
        description: "Replace one tagged tool response from the previous completed toolcall with a fixed cleared placeholder in future visible context.".to_string(),
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
        "instruction".to_string(),
        JsonSchema::string(Some(
            "Optional guidance for the runtime compact memory.".to_string(),
        )),
    )]);
    ResponsesApiTool {
        name: SPINE_TOOL_CLOSE.to_string(),
        description: "Close the current Spine task node and resume its parent.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(Vec::new()), Some(false.into())),
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
            "instruction".to_string(),
            JsonSchema::string(Some(
                "Optional compact guidance for closing the current node before opening the sibling."
                    .to_string(),
            )),
        ),
    ]);
    ResponsesApiTool {
        name: SPINE_TOOL_NEXT.to_string(),
        description: "Close the current node, preserve compact guidance as memory, then continue in a new sibling under the resumed parent.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, Some(vec!["summary".to_string()]), Some(false.into())),
        output_schema: None,
    }
}

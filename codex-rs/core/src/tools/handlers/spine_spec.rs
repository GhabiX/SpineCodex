use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub(crate) fn create_spine_namespace_tool() -> ToolSpec {
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: SPINE_NAMESPACE.to_string(),
        description: "Inspect and move the feature-gated Spine task tree.".to_string(),
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
        ],
    })
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

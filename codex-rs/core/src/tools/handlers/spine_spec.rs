use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn create_spine_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "op".to_string(),
            JsonSchema::string_enum(
                vec![json!("open"), json!("next"), json!("close")],
                Some("One of: open, next, close".to_string()),
            ),
        ),
        (
            "summary".to_string(),
            JsonSchema::string(Some(
                "Short Spine Tree display label for the transition".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "spine".to_string(),
        description: "Move the feature-gated task tree cursor with open, next, or close."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["op".to_string(), "summary".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

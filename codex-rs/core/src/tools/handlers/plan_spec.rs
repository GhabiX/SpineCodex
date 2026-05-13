use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub fn create_update_plan_tool() -> ToolSpec {
    let plan_item_properties = BTreeMap::from([
        ("step".to_string(), JsonSchema::string(/*description*/ None)),
        (
            "status".to_string(),
            JsonSchema::string(Some("One of: pending, in_progress, completed".to_string())),
        ),
    ]);
    let allocation_scope_properties = BTreeMap::from([
        (
            "node".to_string(),
            JsonSchema::string(Some(
                "Existing editable Spine node id, such as 1.2. Omit for a future scope proposal."
                    .to_string(),
            )),
        ),
        (
            "summary".to_string(),
            JsonSchema::string(Some("Scope summary".to_string())),
        ),
        (
            "checkpoints".to_string(),
            JsonSchema::array(
                JsonSchema::string(Some(
                    "Concrete checkpoint/task assigned to this scope".to_string(),
                )),
                Some("Concrete checkpoints/tasks assigned to this scope".to_string()),
            ),
        ),
    ]);
    let allocation_properties = BTreeMap::from([
        (
            "anchor".to_string(),
            JsonSchema::string(Some(
                "Editable Spine anchor node id. Omit to use the current editable scope."
                    .to_string(),
            )),
        ),
        (
            "scopes".to_string(),
            JsonSchema::array(
                JsonSchema::object(
                    allocation_scope_properties,
                    Some(vec!["summary".to_string(), "checkpoints".to_string()]),
                    Some(false.into()),
                ),
                Some("Upcoming scope allocation for Spine planning".to_string()),
            ),
        ),
    ]);

    let properties = BTreeMap::from([
        (
            "explanation".to_string(),
            JsonSchema::string(/*description*/ None),
        ),
        (
            "plan".to_string(),
            JsonSchema::array(
                JsonSchema::object(
                    plan_item_properties,
                    Some(vec!["step".to_string(), "status".to_string()]),
                    Some(false.into()),
                ),
                Some("The list of steps".to_string()),
            ),
        ),
        (
            "spine_allocation".to_string(),
            JsonSchema::object(
                allocation_properties,
                Some(vec!["scopes".to_string()]),
                Some(false.into()),
            ),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: "update_plan".to_string(),
        description: r#"Updates the task plan.
Provide an optional explanation and a list of plan items, each with a step and status.
At most one step can be in_progress at a time.
When Spine is enabled, optionally include spine_allocation to plan how upcoming checkpoints/tasks should be grouped into future execution scopes. This is planning only; it does not create or move Spine nodes.
"#
        .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["plan".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

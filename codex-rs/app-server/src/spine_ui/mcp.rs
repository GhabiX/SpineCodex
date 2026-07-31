use super::ENABLE_ENV;
use super::ITEM_ID_PREFIX;
use super::RESOURCE_HTML;
use super::RESOURCE_MIME_TYPE;
use super::RESOURCE_URI;
use super::SERVER_NAME;
use super::SpineUiState;
use super::TOOL_NAME;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::McpAuthStatus;
use codex_app_server_protocol::McpResourceContent;
use codex_app_server_protocol::McpResourceReadResponse;
use codex_app_server_protocol::McpServerStatus;
use codex_app_server_protocol::McpServerToolCallResponse;
use codex_app_server_protocol::McpToolCallStatus;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::items::McpToolCallItem;
use codex_protocol::items::McpToolCallStatus as CoreMcpToolCallStatus;
use codex_protocol::items::TurnItem as CoreTurnItem;
use codex_protocol::mcp::CallToolResult;
use codex_protocol::mcp::McpServerInfo;
use codex_protocol::mcp::Resource;
use codex_protocol::mcp::Tool;
use codex_protocol::models::ResponseItem;
use serde_json::json;
use std::collections::HashMap;

pub(crate) fn is_enabled() -> bool {
    enabled_from_env_value(std::env::var(ENABLE_ENV).ok().as_deref())
}

fn enabled_from_env_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on"
        )
    })
}

pub(crate) fn is_tree_tool_call(item: &ResponseItem) -> bool {
    let ResponseItem::FunctionCall {
        name, namespace, ..
    } = item
    else {
        return false;
    };
    let tool = match namespace.as_deref() {
        Some("spine") => name.as_str(),
        None => name.strip_prefix("spine.").unwrap_or_default(),
        Some(_) => return false,
    };
    matches!(tool, "open" | "next" | "close" | "spawn")
}

pub(crate) fn read_resource(server: &str, uri: &str) -> Option<McpResourceReadResponse> {
    (server == SERVER_NAME && uri == RESOURCE_URI).then(|| McpResourceReadResponse {
        contents: vec![McpResourceContent::Text {
            uri: uri.to_string(),
            mime_type: Some(RESOURCE_MIME_TYPE.to_string()),
            text: RESOURCE_HTML.to_string(),
            meta: Some(json!({
                "ui": {
                    "prefersBorder": true,
                    "csp": {
                        "connectDomains": [],
                        "resourceDomains": []
                    }
                },
                "openai/widgetHeightHint": 1,
                "openai/widgetMinFrameHeight": 1
            })),
        }],
    })
}

pub(crate) fn is_internal_tool(server: &str, tool: &str) -> bool {
    server == SERVER_NAME && tool == TOOL_NAME
}

pub(crate) fn tool_call_response(
    thread_id: &str,
    state: Option<&SpineUiState>,
) -> McpServerToolCallResponse {
    let Some(structured_content) = state.and_then(SpineUiState::structured_content) else {
        return McpServerToolCallResponse {
            content: vec![json!({
                "type": "text",
                "text": "No Spine Tree is active for this thread."
            })],
            structured_content: None,
            is_error: Some(true),
            meta: None,
        };
    };
    McpServerToolCallResponse {
        content: vec![json!({
            "type": "text",
            "text": "Spine Tree"
        })],
        structured_content: Some(structured_content),
        is_error: Some(false),
        meta: Some(json!({
            "openai/widgetSessionId": format!("spine-ui-tool-{thread_id}")
        })),
    }
}

pub(crate) fn server_status(include_resources: bool) -> McpServerStatus {
    let tool = Tool {
        name: TOOL_NAME.to_string(),
        title: Some("Spine Tree".to_string()),
        description: Some(
            "Host-managed read-only view of the current Spine task tree.".to_string(),
        ),
        input_schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
        output_schema: None,
        annotations: Some(json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "openWorldHint": false
        })),
        icons: None,
        meta: Some(json!({"ui": {"resourceUri": RESOURCE_URI}})),
    };
    McpServerStatus {
        name: SERVER_NAME.to_string(),
        server_info: Some(McpServerInfo {
            name: SERVER_NAME.to_string(),
            title: Some("Spine UI".to_string()),
            version: "1".to_string(),
            description: Some("Read-only Spine task tree UI.".to_string()),
            icons: None,
            website_url: None,
        }),
        tools: HashMap::from([(tool.name.clone(), tool)]),
        resources: include_resources
            .then(|| registered_resource(RESOURCE_URI, "spine-tree", "Spine Tree"))
            .into_iter()
            .collect(),
        resource_templates: Vec::new(),
        auth_status: McpAuthStatus::Unsupported,
    }
}

fn registered_resource(uri: &str, name: &str, title: &str) -> Resource {
    Resource {
        annotations: None,
        description: Some("Spine task tree component".to_string()),
        mime_type: Some(RESOURCE_MIME_TYPE.to_string()),
        name: name.to_string(),
        size: None,
        title: Some(title.to_string()),
        uri: uri.to_string(),
        icons: None,
        meta: None,
    }
}

pub(crate) fn snapshot_started_notification(
    thread_id: &str,
    turn_id: &str,
    state: &SpineUiState,
) -> Option<ItemStartedNotification> {
    Some(ItemStartedNotification {
        item: snapshot_item(turn_id, state, McpToolCallStatus::InProgress)?,
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        started_at_ms: state.started_at_ms()?,
    })
}

pub(crate) fn snapshot_completed_notification(
    thread_id: &str,
    turn_id: &str,
    state: &SpineUiState,
) -> Option<ItemCompletedNotification> {
    Some(ItemCompletedNotification {
        item: snapshot_item(turn_id, state, McpToolCallStatus::Completed)?,
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        completed_at_ms: state.completed_at_ms()?,
    })
}

fn snapshot_item(
    turn_id: &str,
    state: &SpineUiState,
    status: McpToolCallStatus,
) -> Option<ThreadItem> {
    Some(ThreadItem::from(snapshot_core_item(
        turn_id,
        state,
        match status {
            McpToolCallStatus::InProgress => CoreMcpToolCallStatus::InProgress,
            McpToolCallStatus::Completed => CoreMcpToolCallStatus::Completed,
            McpToolCallStatus::Failed => CoreMcpToolCallStatus::Failed,
        },
    )?))
}

fn snapshot_core_item(
    turn_id: &str,
    state: &SpineUiState,
    status: CoreMcpToolCallStatus,
) -> Option<CoreTurnItem> {
    let structured_content = state.structured_content()?;
    let snapshot_seq = state.snapshot.as_ref()?.snapshot_seq;
    // Codex supersedes older widgets that share a server/resource unless the host
    // gives each independently mounted turn a stable widget session.
    let item_id = format!("{ITEM_ID_PREFIX}{turn_id}");
    Some(CoreTurnItem::McpToolCall(McpToolCallItem {
        id: item_id.clone(),
        server: SERVER_NAME.to_string(),
        tool: TOOL_NAME.to_string(),
        status,
        arguments: json!({}),
        connector_id: Some(SERVER_NAME.to_string()),
        mcp_app_resource_uri: Some(RESOURCE_URI.to_string()),
        link_id: None,
        app_name: Some("Spine UI".to_string()),
        template_id: None,
        action_name: Some(TOOL_NAME.to_string()),
        plugin_id: None,
        result: Some(CallToolResult {
            content: vec![json!({
                "type": "text",
                "text": format!("Spine snapshot {snapshot_seq}"),
            })],
            structured_content: Some(structured_content),
            is_error: Some(false),
            meta: Some(json!({
                "ui/resourceUri": RESOURCE_URI,
                "openai/widgetSessionId": item_id,
            })),
        }),
        error: None,
        duration: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::enabled_from_env_value;

    #[test]
    fn enable_env_parser_is_strict() {
        for enabled in ["1", "true", "TRUE", " on "] {
            assert!(enabled_from_env_value(Some(enabled)), "{enabled}");
        }
        for disabled in ["", "0", "false", "off", "yes", "enabled"] {
            assert!(!enabled_from_env_value(Some(disabled)), "{disabled}");
        }
        assert!(!enabled_from_env_value(None));
    }
}

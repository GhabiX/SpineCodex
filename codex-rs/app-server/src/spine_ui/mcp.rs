use super::CODE_MODE_SPINE_CARRIER_MARKER;
use super::ENABLE_ENV;
use super::ITEM_ID_PREFIX;
use super::RESOURCE_HTML;
use super::RESOURCE_MIME_TYPE;
use super::RESOURCE_URI;
use super::SERVER_NAME;
use super::SpineUiState;
use super::TOOL_NAME;
use crate::config_manager::ConfigManager;
use crate::error_code::internal_error;
use crate::error_code::invalid_request;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::thread_state::ThreadStateManager;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::McpAuthStatus;
use codex_app_server_protocol::McpResourceContent;
use codex_app_server_protocol::McpResourceReadParams;
use codex_app_server_protocol::McpResourceReadResponse;
use codex_app_server_protocol::McpServerStatus;
use codex_app_server_protocol::McpServerToolCallParams;
use codex_app_server_protocol::McpServerToolCallResponse;
use codex_app_server_protocol::McpToolCallStatus;
use codex_app_server_protocol::ThreadItem;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_protocol::ThreadId;
use codex_protocol::items::McpToolCallItem;
use codex_protocol::items::McpToolCallStatus as CoreMcpToolCallStatus;
use codex_protocol::items::TurnItem as CoreTurnItem;
use codex_protocol::mcp::CallToolResult;
use codex_protocol::mcp::McpServerInfo;
use codex_protocol::mcp::Resource;
use codex_protocol::mcp::Tool;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnAbortReason;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

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

pub(crate) fn is_tree_affecting_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::FunctionCall {
            name, namespace, ..
        } => {
            let tool = match namespace.as_deref() {
                Some("spine") => name.as_str(),
                None => name.strip_prefix("spine.").unwrap_or_default(),
                Some(_) => return false,
            };
            matches!(tool, "open" | "next" | "close" | "spawn")
        }
        ResponseItem::CustomToolCallOutput { name, output, .. } => {
            if name.as_deref() != Some(CODE_MODE_SPINE_CARRIER_MARKER) {
                return false;
            }
            let FunctionCallOutputBody::Text(body) = &output.body else {
                return false;
            };
            let Ok(carrier) = serde_json::from_str::<CodeModeSpineCarrier>(body) else {
                return false;
            };
            carrier.schema == CODE_MODE_SPINE_CARRIER_MARKER
                && carrier
                    .nested_spine_calls
                    .iter()
                    .any(|call| matches!(call.name.as_str(), "open" | "next" | "close" | "spawn"))
        }
        _ => false,
    }
}

#[derive(Deserialize)]
struct CodeModeSpineCarrier {
    schema: String,
    nested_spine_calls: Vec<CodeModeSpineCall>,
}

#[derive(Deserialize)]
struct CodeModeSpineCall {
    name: String,
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

#[derive(Clone)]
pub(crate) struct SpineUiMcpHandler {
    thread_state_manager: ThreadStateManager,
    thread_manager: Arc<ThreadManager>,
    outgoing: Arc<OutgoingMessageSender>,
    config_manager: ConfigManager,
}

impl SpineUiMcpHandler {
    pub(crate) fn new(
        thread_state_manager: ThreadStateManager,
        thread_manager: Arc<ThreadManager>,
        outgoing: Arc<OutgoingMessageSender>,
        config_manager: ConfigManager,
    ) -> Self {
        Self {
            thread_state_manager,
            thread_manager,
            outgoing,
            config_manager,
        }
    }

    pub(crate) fn is_internal_tool(server: &str, tool: &str) -> bool {
        is_enabled() && is_internal_tool(server, tool)
    }

    pub(crate) fn ensure_reserved_name_available(config: &Config) -> Result<(), JSONRPCErrorError> {
        if is_enabled()
            && config
                .mcp_servers
                .get()
                .get(SERVER_NAME)
                .is_some_and(|server| server.enabled)
        {
            return Err(invalid_request(format!(
                "the configured MCP server name '{SERVER_NAME}' is reserved while {ENABLE_ENV} is enabled"
            )));
        }
        Ok(())
    }

    pub(crate) fn add_server_name(server_names: &mut Vec<String>) {
        if !is_enabled() {
            return;
        }
        server_names.push(SERVER_NAME.to_string());
        server_names.sort();
        server_names.dedup();
    }

    pub(crate) fn replace_server_statuses(
        statuses: &mut [McpServerStatus],
        include_resources: bool,
    ) {
        if !is_enabled() {
            return;
        }
        if let Some(status) = statuses
            .iter_mut()
            .find(|status| status.name == SERVER_NAME)
        {
            *status = server_status(include_resources);
        }
    }

    pub(crate) fn resource_response(server: &str, uri: &str) -> Option<McpResourceReadResponse> {
        is_enabled().then(|| read_resource(server, uri)).flatten()
    }

    pub(crate) async fn try_read_resource(
        &self,
        request_id: &ConnectionRequestId,
        params: &McpResourceReadParams,
    ) -> Result<bool, JSONRPCErrorError> {
        let Some(response) = Self::resource_response(&params.server, &params.uri) else {
            return Ok(false);
        };
        self.ensure_latest_config_has_no_collision(params.thread_id.as_deref())
            .await?;
        self.outgoing
            .send_response(request_id.clone(), response)
            .await;
        Ok(true)
    }

    pub(crate) async fn try_call_tool(
        &self,
        request_id: &ConnectionRequestId,
        params: &McpServerToolCallParams,
    ) -> Result<bool, JSONRPCErrorError> {
        if !Self::is_internal_tool(&params.server, &params.tool) {
            return Ok(false);
        }
        if params.arguments.as_ref().is_some_and(|arguments| {
            arguments
                .as_object()
                .is_none_or(|object| !object.is_empty())
        }) {
            return Err(invalid_request(format!(
                "{SERVER_NAME}/{TOOL_NAME} accepts only an empty object"
            )));
        }
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        self.ensure_latest_config_has_no_collision(Some(&params.thread_id))
            .await?;
        let state = self
            .thread_state_manager
            .spine_ui_state_for_thread(thread_id)
            .await;
        let response = tool_call_response(&params.thread_id, state.as_ref());
        self.outgoing
            .send_response(request_id.clone(), response)
            .await;
        Ok(true)
    }

    async fn ensure_latest_config_has_no_collision(
        &self,
        thread_id: Option<&str>,
    ) -> Result<(), JSONRPCErrorError> {
        let config = match thread_id {
            Some(thread_id) => {
                let thread_id = ThreadId::from_string(thread_id)
                    .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
                let thread = self
                    .thread_manager
                    .get_thread(thread_id)
                    .await
                    .map_err(|_| invalid_request(format!("thread not found: {thread_id}")))?;
                let thread_config = thread.config().await;
                self.config_manager
                    .load_latest_config_for_thread(thread_config.as_ref())
                    .await
            }
            None => {
                self.config_manager
                    .load_latest_config(/*fallback_cwd*/ None)
                    .await
            }
        }
        .map_err(|error| internal_error(format!("failed to reload config: {error}")))?;
        Self::ensure_reserved_name_available(&config)
    }
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

/// Builds the private Desktop carrier used to mount or refresh a live Spine card.
/// Repeated `item/started` notifications are upserts only for this reserved item identity;
/// generic app-server items must follow the normal started/delta/completed lifecycle.
pub(crate) fn snapshot_upsert_notification(
    thread_id: &str,
    turn_id: &str,
    state: &SpineUiState,
) -> Option<ItemStartedNotification> {
    Some(ItemStartedNotification {
        item: snapshot_item(turn_id, state, McpToolCallStatus::InProgress, None)?,
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        started_at_ms: state.started_at_ms()?,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpineUiTerminalOutcome {
    Completed,
    Aborted(TurnAbortReason),
    ListenerFailed(String),
}

impl SpineUiTerminalOutcome {
    fn abort_reason(reason: &TurnAbortReason) -> &'static str {
        match reason {
            TurnAbortReason::Interrupted => "interrupted",
            TurnAbortReason::Replaced => "replaced",
            TurnAbortReason::ReviewEnded => "review_ended",
            TurnAbortReason::BudgetLimited => "budget_limited",
        }
    }

    fn status(&self) -> McpToolCallStatus {
        match self {
            Self::Completed => McpToolCallStatus::Completed,
            Self::Aborted(_) | Self::ListenerFailed(_) => McpToolCallStatus::Failed,
        }
    }

    fn outcome(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Aborted(_) => "aborted",
            Self::ListenerFailed(_) => "failed",
        }
    }

    fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Completed => None,
            Self::Aborted(reason) => Some(Self::abort_reason(reason)),
            Self::ListenerFailed(_) => Some("listener_error"),
        }
    }

    fn error_message(&self) -> Option<String> {
        match self {
            Self::Completed => None,
            Self::Aborted(reason) => Some(format!(
                "Spine turn aborted: {}",
                Self::abort_reason(reason)
            )),
            Self::ListenerFailed(message) => {
                Some(format!("Spine listener stopped unexpectedly: {message}"))
            }
        }
    }
}

pub(crate) fn snapshot_terminal_notification(
    thread_id: &str,
    turn_id: &str,
    state: &SpineUiState,
    outcome: &SpineUiTerminalOutcome,
) -> Option<ItemCompletedNotification> {
    Some(ItemCompletedNotification {
        item: snapshot_item(turn_id, state, outcome.status(), Some(outcome))?,
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        completed_at_ms: state.completed_at_ms()?,
    })
}

fn snapshot_item(
    turn_id: &str,
    state: &SpineUiState,
    status: McpToolCallStatus,
    terminal_outcome: Option<&SpineUiTerminalOutcome>,
) -> Option<ThreadItem> {
    Some(ThreadItem::from(snapshot_core_item(
        turn_id,
        state,
        match status {
            McpToolCallStatus::InProgress => CoreMcpToolCallStatus::InProgress,
            McpToolCallStatus::Completed => CoreMcpToolCallStatus::Completed,
            McpToolCallStatus::Failed => CoreMcpToolCallStatus::Failed,
        },
        terminal_outcome,
    )?))
}

fn snapshot_core_item(
    turn_id: &str,
    state: &SpineUiState,
    status: CoreMcpToolCallStatus,
    terminal_outcome: Option<&SpineUiTerminalOutcome>,
) -> Option<CoreTurnItem> {
    let mut structured_content = state.structured_content()?;
    if let Some(terminal_outcome) = terminal_outcome
        && let Some(content) = structured_content.as_object_mut()
    {
        content.insert(
            "terminalOutcome".to_string(),
            json!(terminal_outcome.outcome()),
        );
        if let Some(reason) = terminal_outcome.reason() {
            content.insert("terminalReason".to_string(), json!(reason));
        }
    }
    let snapshot_seq = state.snapshot.as_ref()?.snapshot_seq;
    let is_error =
        terminal_outcome.is_some_and(|outcome| outcome.status() == McpToolCallStatus::Failed);
    let result_text = terminal_outcome
        .and_then(SpineUiTerminalOutcome::error_message)
        .unwrap_or_else(|| format!("Spine snapshot {snapshot_seq}"));
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
                "text": result_text,
            })],
            structured_content: Some(structured_content),
            is_error: Some(is_error),
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

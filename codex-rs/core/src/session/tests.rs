use super::spine_bridge::SpineToolCommit;
use super::turn_context::TurnEnvironment;
use super::*;
use crate::config::ConfigBuilder;
use crate::config::test_config;
use crate::context::ContextualUserFragment;
use crate::context::TurnAborted;
use crate::function_tool::FunctionCallError;
use crate::shell::default_user_shell;
use crate::skills::SkillRenderSideEffects;
use crate::skills::render::SkillMetadataBudget;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_NEXT;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use crate::spine::SPINE_TOOL_TRIM;
use crate::spine::SpineToolOutputRecording;
use crate::spine::bridge::ToolCallEvidence;
use crate::test_support::models_manager_with_provider;
use crate::tools::format_exec_output_str;
use codex_config::ConfigLayerStack;
use codex_config::ConfigLayerStackOrdering;
use codex_config::LoaderOverrides;
use codex_config::NetworkConstraints;
use codex_config::NetworkDomainPermissionToml;
use codex_config::NetworkDomainPermissionsToml;
use codex_config::RequirementSource;
use codex_config::Sourced;
use codex_config::loader::project_trust_key;
use codex_config::types::ToolSuggestDisabledTool;

use codex_features::Feature;
use codex_features::Features;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::bundled_models_response;
use codex_models_manager::model_info;
use codex_models_manager::test_support::construct_model_info_offline_for_tests;
use codex_models_manager::test_support::get_model_offline_for_tests;
use codex_protocol::AgentPath;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::TrustLevel;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use codex_protocol::models::FileSystemPermissions;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::SandboxEnforcement;
use codex_protocol::num_format::format_si_suffix;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::protocol::NonSteerableTurnKind;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_protocol::request_permissions::PermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::spine_tree::SpineNodeContextProblem;
use tracing::Span;

use crate::goals::ExternalGoalPreviousStatus;
use crate::goals::ExternalGoalSet;
use crate::goals::GoalRuntimeEvent;
use crate::goals::SetGoalRequest;
use crate::rollout::recorder::RolloutRecorder;
use crate::spine::SpineRuntime;
use crate::spine::SpineSessionState;
use crate::spine::SpineStore;
use crate::state::ActiveTurn;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;
use crate::tasks::UserShellCommandMode;
use crate::tasks::execute_user_shell_command;
use crate::tools::ToolRouter;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::CreateGoalHandler;
use crate::tools::handlers::ExecCommandHandler;
use crate::tools::handlers::ShellCommandHandler;
use crate::tools::handlers::UpdateGoalHandler;
use crate::tools::registry::ToolExecutor;
use crate::tools::router::ToolCallSource;
use crate::turn_diff_tracker::TurnDiffTracker;
use codex_app_server_protocol::AppInfo;
use codex_app_server_protocol::McpElicitationSchema;
use codex_config::config_toml::ConfigToml;
use codex_config::config_toml::ProjectConfig;
use codex_execpolicy::Decision;
use codex_execpolicy::NetworkRuleProtocol;
use codex_execpolicy::Policy;
use codex_network_proxy::NetworkProxyConfig;
use codex_otel::MetricsClient;
use codex_otel::MetricsConfig;
use codex_otel::THREAD_SKILLS_DESCRIPTION_TRUNCATED_CHARS_METRIC;
use codex_otel::THREAD_SKILLS_ENABLED_TOTAL_METRIC;
use codex_otel::THREAD_SKILLS_KEPT_TOTAL_METRIC;
use codex_otel::THREAD_SKILLS_TRUNCATED_METRIC;
use codex_otel::TelemetryAuthMode;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::image_close_tag_text;
use codex_protocol::models::image_open_tag_text;
use codex_protocol::openai_models::InputModality;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::ConversationAudioParams;
use codex_protocol::protocol::CreditsSnapshot;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::NetworkApprovalProtocol;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::RealtimeAudioFrame;
use codex_protocol::protocol::RealtimeConversationListVoicesResponseEvent;
use codex_protocol::protocol::RealtimeVoice;
use codex_protocol::protocol::RealtimeVoicesList;
use codex_protocol::protocol::ResumedHistory;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SkillScope;
use codex_protocol::protocol::Submission;
use codex_protocol::protocol::ThreadGoalStatus;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::UserMessageEvent;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::request_user_input::RequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_protocol::spine_tree::SpineNodeContextBaselineSource;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use codex_protocol::spine_tree::SpineTreeUpdateEvent;
use codex_rmcp_client::ElicitationAction;
use codex_thread_store::AppendThreadItemsParams;
use codex_thread_store::ArchiveThreadParams;
use codex_thread_store::ItemPage;
use codex_thread_store::ListItemsParams;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::ListTurnsParams;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadByRolloutPathParams;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ResumeThreadParams;
use codex_thread_store::StoredThread;
use codex_thread_store::StoredThreadHistory;
use codex_thread_store::ThreadPage;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreResult;
use codex_thread_store::TurnPage;
use codex_thread_store::UpdateThreadMetadataParams;
use core_test_support::PathBufExt;
use core_test_support::PathExt;
use core_test_support::context_snapshot;
use core_test_support::context_snapshot::ContextSnapshotOptions;
use core_test_support::context_snapshot::ContextSnapshotRenderMode;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::ev_shell_command_call;
use core_test_support::responses::ev_tool_search_call;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_failed;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::test_path_buf;
use core_test_support::tracing::install_test_tracing;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::trace::TraceId;
use opentelemetry_sdk::metrics::InMemoryMetricExporter;
use opentelemetry_sdk::metrics::data::AggregatedMetrics;
use opentelemetry_sdk::metrics::data::Metric;
use opentelemetry_sdk::metrics::data::MetricData;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use serial_test::serial;
use std::path::Path;
use std::time::Duration;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::Semaphore;
use tokio::time::sleep;
use tokio::time::timeout;
use tracing_opentelemetry::OpenTelemetrySpanExt;

fn spine_summary_sse(id: &str, text: &str) -> String {
    sse(vec![ev_assistant_message(id, text), ev_completed(id)])
}

fn spine_node_memory_summary_sse(id: &str, text: &str) -> String {
    spine_node_memory_summaries_sse(id, &[text])
}

fn spine_node_memory_summaries_sse(id: &str, texts: &[&str]) -> String {
    sse(vec![
        ev_assistant_message(id, &texts.join("\n")),
        ev_completed(id),
    ])
}

async fn prime_model_client_turn_state(
    client_session: &mut crate::client::ModelClientSession,
    turn_context: &TurnContext,
) {
    use crate::client::ResponsesToolChoice;
    use crate::client_common::ResponseEvent;
    use codex_rollout_trace::InferenceTraceContext;
    use futures::StreamExt as _;

    let prompt = crate::Prompt {
        input: vec![user_message("prime turn state")],
        base_instructions: BaseInstructions::default(),
        ..Default::default()
    };
    let mut stream = client_session
        .stream_responses_api(
            &prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort,
            turn_context.reasoning_summary,
            turn_context.config.service_tier.clone(),
            turn_context
                .turn_metadata_state
                .current_header_value()
                .as_deref(),
            &InferenceTraceContext::disabled(),
            ResponsesToolChoice::Auto,
        )
        .await
        .expect("prime request should start");
    while let Some(event) = stream.next().await {
        match event.expect("prime stream event should decode") {
            ResponseEvent::Completed { .. } => break,
            _ => {}
        }
    }
}
use wiremock::ResponseTemplate;

use codex_protocol::mcp::CallToolResult as McpCallToolResult;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration as StdDuration;

mod guardian_tests;

struct InstructionsTestCase {
    slug: &'static str,
    expects_apply_patch_description: bool,
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn developer_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn anchored_user_message(anchor: u64, text: &str) -> ResponseItem {
    user_message(&format!("[U{anchor}]\n{text}"))
}

fn contains_user_memory_block(text: &str, expected_body: &str) -> bool {
    let lines = text.lines().collect::<Vec<_>>();
    lines.iter().enumerate().any(|(index, line)| {
        if !(line == &"## User Message" || line.starts_with("## User Message [U")) {
            return false;
        }
        let block = lines[index + 1..]
            .iter()
            .take_while(|line| !line.starts_with("## ") && !line.starts_with("# Spine Memory "))
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        block.contains(expected_body)
    })
}

fn message_text_contains(item: &ResponseItem, expected: &str) -> bool {
    matches!(
        item,
        ResponseItem::Message { content, .. }
            if content.iter().any(|content| matches!(
                content,
                ContentItem::InputText { text } | ContentItem::OutputText { text }
                    if text.contains(expected)
            ))
    )
}

fn message_text_count(items: &[ResponseItem], expected: &str) -> usize {
    items
        .iter()
        .map(|item| match item {
            ResponseItem::Message { content, .. } => content
                .iter()
                .map(|content| match content {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        text.matches(expected).count()
                    }
                    ContentItem::InputImage { .. } => 0,
                })
                .sum(),
            _ => 0,
        })
        .sum()
}

fn variable_spine_items(items: &[ResponseItem]) -> Vec<ResponseItem> {
    items
        .iter()
        .filter(|item| !Session::is_spine_context_observation_fixed_prefix_item(item))
        .cloned()
        .collect()
}

fn fixed_spine_context_item_count(items: &[ResponseItem]) -> usize {
    items
        .iter()
        .filter(|item| Session::is_spine_context_observation_fixed_prefix_item(item))
        .count()
}

fn clone_spine_sidecar_for_test(source_rollout: &Path, target_rollout: &Path, raw_live: &[bool]) {
    let boundary = SpineStore::clone_boundary_for_rollout(
        source_rollout,
        u64::try_from(raw_live.len()).expect("raw live len"),
    )
    .expect("capture clone boundary")
    .expect("source sidecar exists");
    SpineStore::clone_for_rollout_with_raw_live(&boundary, target_rollout, raw_live)
        .expect("clone spine sidecar");
}

fn assistant_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn spine_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: default_spine_call_arguments(name),
        call_id: call_id.to_string(),
    }
}

fn default_spine_call_arguments(name: &str) -> String {
    match name {
        SPINE_TOOL_OPEN => r#"{"summary":"test spine open"}"#.to_string(),
        _ => "{}".to_string(),
    }
}

fn spine_call_with_args(name: &str, call_id: &str, arguments: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: arguments.to_string(),
        call_id: call_id.to_string(),
    }
}

fn function_call(name: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: call_id.to_string(),
    }
}

fn function_output(call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload::from_text("ok".to_string()),
    }
}

fn function_output_with_text(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        call_id: call_id.to_string(),
        output: FunctionCallOutputPayload::from_text(text.to_string()),
    }
}

fn trim_candidate_text(fragment: &str) -> String {
    assert!(!fragment.is_empty());
    let target_bytes = crate::spine::TOOL_RESPONSE_TRIM_THRESHOLD_BYTES as usize + 1_024;
    let repeat_count = (target_bytes / fragment.len()) + 1;
    fragment.repeat(repeat_count)
}

fn function_output_text(item: &ResponseItem) -> &str {
    let ResponseItem::FunctionCallOutput { output, .. } = item else {
        panic!("expected function call output: {item:?}");
    };
    output
        .text_content()
        .expect("function call output should be text")
}

fn function_output_text_by_call_id<'a>(items: &'a [ResponseItem], target_call_id: &str) -> &'a str {
    let output = items
        .iter()
        .find_map(|item| {
            let ResponseItem::FunctionCallOutput { call_id, output } = item else {
                return None;
            };
            (call_id == target_call_id).then_some(output)
        })
        .unwrap_or_else(|| panic!("missing function call output for {target_call_id}: {items:#?}"));
    output
        .text_content()
        .expect("function call output should be text")
}

fn custom_tool_call(call_id: &str) -> ResponseItem {
    ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: call_id.to_string(),
        name: "apply_patch".to_string(),
        input: "*** Begin Patch\n*** End Patch\n".to_string(),
    }
}

fn custom_tool_output(call_id: &str) -> ResponseItem {
    ResponseItem::CustomToolCallOutput {
        call_id: call_id.to_string(),
        name: None,
        output: FunctionCallOutputPayload::from_text("custom ok".to_string()),
    }
}

fn tool_search_call(call_id: &str) -> ResponseItem {
    tool_search_call_with_execution(call_id, "client")
}

fn tool_search_call_with_execution(call_id: &str, execution: &str) -> ResponseItem {
    ResponseItem::ToolSearchCall {
        id: None,
        status: None,
        call_id: Some(call_id.to_string()),
        execution: execution.to_string(),
        arguments: json!({"query":"spine"}),
    }
}

fn tool_search_output(call_id: &str) -> ResponseItem {
    tool_search_output_with_execution(call_id, "client")
}

fn tool_search_output_with_execution(call_id: &str, execution: &str) -> ResponseItem {
    ResponseItem::ToolSearchOutput {
        call_id: Some(call_id.to_string()),
        status: "completed".to_string(),
        execution: execution.to_string(),
        tools: Vec::new(),
    }
}

fn replace_function_output_text_for_test(item: &mut ResponseItem, text: String) {
    if let ResponseItem::FunctionCallOutput { output, .. } = item {
        output.body = FunctionCallOutputBody::Text(text);
    }
}

async fn commit_spine_output_and_record_raw_durable_for_test(
    session: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    response_item: ResponseItem,
) -> CodexResult<ResponseItem> {
    commit_spine_output_and_record_raw_durable_for_test_inner(session, turn_context, response_item)
        .await
}

async fn commit_spine_output_and_record_raw_durable_for_test_inner(
    session: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    response_item: ResponseItem,
) -> CodexResult<ResponseItem> {
    let output_recorded_before_spine_commit = session.enabled(Feature::SpineJit);
    if output_recorded_before_spine_commit {
        session
            .record_conversation_items_without_spine_observe(
                turn_context,
                std::slice::from_ref(&response_item),
            )
            .await?;
    }
    let mut commit = test_on_toolcall_single(session, turn_context, &response_item)
        .await
        .map_err(|err| CodexErr::SpineTerminalFailure {
            operation: "commit Spine tool output".to_string(),
            reason: err.to_string(),
        })?;
    if commit.skips_host_recording() && !output_recorded_before_spine_commit {
        return Ok(response_item);
    }
    if output_recorded_before_spine_commit {
        if let Some(snapshot) = commit.take_deferred_tree_update() {
            session
                .send_spine_tree_update(turn_context.as_ref(), snapshot)
                .await;
        }
    } else if commit.records_raw_only_durable_without_emission() {
        session
            .record_conversation_items_raw_only_durable_without_emission(
                turn_context,
                std::slice::from_ref(&response_item),
            )
            .await?;
        session
            .send_raw_response_items(turn_context, std::slice::from_ref(&response_item))
            .await;
        if let Some(snapshot) = commit.take_deferred_tree_update() {
            session
                .send_spine_tree_update(turn_context.as_ref(), snapshot)
                .await;
        }
    } else if commit.records_without_spine_observe() {
        session
            .record_conversation_items_without_spine_observe(
                turn_context,
                std::slice::from_ref(&response_item),
            )
            .await?;
    } else {
        session
            .record_conversation_items(turn_context, std::slice::from_ref(&response_item))
            .await?;
    }
    Ok(response_item)
}

async fn test_on_toolcall_single(
    session: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    item: &ResponseItem,
) -> Result<SpineToolCommit, SpineError> {
    session
        .test_on_toolcall(turn_context, ToolCallEvidence::single(item))
        .await
}

async fn assert_no_pending_spine_commit(session: &Session, call_id: &str) {
    let spine_slot = session.spine.as_ref().expect("spine enabled");
    let guard = spine_slot.lock().await;
    guard.ensure_valid().expect("spine runtime valid");
    let runtime = guard.runtime().expect("spine runtime present");
    assert!(
        runtime
            .pending_commit(call_id)
            .expect("pending lookup")
            .is_none(),
        "expected no pending Spine transition for {call_id}"
    );
}

async fn assert_pending_spine_commit(session: &Session, call_id: &str) {
    let spine_slot = session.spine.as_ref().expect("spine enabled");
    let guard = spine_slot.lock().await;
    guard.ensure_valid().expect("spine runtime valid");
    let runtime = guard.runtime().expect("spine runtime present");
    assert!(
        runtime
            .pending_commit(call_id)
            .expect("pending lookup")
            .is_some(),
        "expected pending Spine transition for {call_id}"
    );
}

fn assert_no_pending_spine_tree_update_matching(
    rx: &async_channel::Receiver<Event>,
    reason: &str,
    is_forbidden: impl Fn(&SpineTreeUpdateEvent) -> bool,
) {
    while let Ok(event) = rx.try_recv() {
        if let EventMsg::SpineTreeUpdate(snapshot) = &event.msg
            && is_forbidden(snapshot)
        {
            panic!("{reason}: unexpected SpineTreeUpdate event: {event:?}");
        }
    }
}

fn assert_no_event_matching(
    rx: &async_channel::Receiver<Event>,
    reason: &str,
    is_forbidden: impl Fn(&Event) -> bool,
) {
    while let Ok(event) = rx.try_recv() {
        if is_forbidden(&event) {
            panic!("{reason}: unexpected event: {event:?}");
        }
    }
}

async fn assert_session_history_matches_spine_materialization(
    session: &Session,
    rollout_path: &Path,
) {
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(rollout_path, &raw_items, &[])
        .expect("load spine runtime from rollout")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    assert_eq!(
        session.clone_history().await.raw_items(),
        materialized.as_slice()
    );
}

async fn assert_spine_visible_response_context_refs_strictly_increase(
    session: &Session,
    rollout_path: &Path,
) -> Vec<(u64, usize)> {
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(rollout_path, &raw_items, &[])
        .expect("load spine runtime from rollout")
        .expect("spine sidecar should exist");
    let refs = runtime.visible_response_context_refs_for_test();
    for pair in refs.windows(2) {
        let [previous, current] = pair else {
            unreachable!("windows(2) yields pairs")
        };
        assert!(
            current.1 > previous.1,
            "visible PS context_index must strictly increase: previous raw {} ctx {}, current raw {} ctx {}, all refs: {:?}",
            previous.0,
            previous.1,
            current.0,
            current.1,
            refs
        );
    }
    refs
}

#[derive(Clone, Copy)]
enum AppendEqualityHarnessCase {
    BaseConversation,
    BaseConversationWithoutRawEvent,
    RawOnlyBestEffort,
    RawOnlyDurableWithoutEmission,
    WithoutSpineObserve,
    SpineControlOverlayOnly,
}

impl AppendEqualityHarnessCase {
    fn name(self) -> &'static str {
        match self {
            AppendEqualityHarnessCase::BaseConversation => "base_conversation",
            AppendEqualityHarnessCase::BaseConversationWithoutRawEvent => {
                "base_conversation_without_raw_event"
            }
            AppendEqualityHarnessCase::RawOnlyBestEffort => "raw_only_best_effort",
            AppendEqualityHarnessCase::RawOnlyDurableWithoutEmission => {
                "raw_only_durable_without_emission"
            }
            AppendEqualityHarnessCase::WithoutSpineObserve => "without_spine_observe",
            AppendEqualityHarnessCase::SpineControlOverlayOnly => "spine_control_overlay_only",
        }
    }

    fn items(self) -> Vec<ResponseItem> {
        match self {
            AppendEqualityHarnessCase::BaseConversation
            | AppendEqualityHarnessCase::BaseConversationWithoutRawEvent => vec![
                user_message("append equality user"),
                function_call("append_eq_tool", "append-eq-tool"),
                function_output("append-eq-tool"),
                assistant_message("append equality assistant"),
            ],
            AppendEqualityHarnessCase::RawOnlyBestEffort
            | AppendEqualityHarnessCase::RawOnlyDurableWithoutEmission
            | AppendEqualityHarnessCase::WithoutSpineObserve => {
                vec![function_output("append-eq-raw-output")]
            }
            AppendEqualityHarnessCase::SpineControlOverlayOnly => vec![
                spine_call(SPINE_TOOL_TREE, "append-eq-tree"),
                function_call("ordinary_overlay_tool", "append-eq-ordinary"),
                function_output("append-eq-tree"),
                function_output("append-eq-ordinary"),
            ],
        }
    }

    fn setup_items(self) -> Vec<ResponseItem> {
        match self {
            AppendEqualityHarnessCase::RawOnlyBestEffort
            | AppendEqualityHarnessCase::RawOnlyDurableWithoutEmission
            | AppendEqualityHarnessCase::WithoutSpineObserve => {
                vec![function_call("append_eq_raw_tool", "append-eq-raw-output")]
            }
            _ => Vec::new(),
        }
    }
}

async fn run_append_equality_harness_case(
    session: &Session,
    turn_context: &TurnContext,
    case: AppendEqualityHarnessCase,
    items: &[ResponseItem],
) {
    match case {
        AppendEqualityHarnessCase::BaseConversation => {
            session
                .record_conversation_items(turn_context, items)
                .await
                .unwrap_or_else(|err| panic!("{} append failed: {err}", case.name()));
        }
        AppendEqualityHarnessCase::BaseConversationWithoutRawEvent => {
            session
                .record_conversation_items_without_raw_event(turn_context, items)
                .await
                .unwrap_or_else(|err| panic!("{} append failed: {err}", case.name()));
        }
        AppendEqualityHarnessCase::RawOnlyBestEffort => {
            session
                .record_conversation_items_raw_only(turn_context, items)
                .await
                .unwrap_or_else(|err| panic!("{} append failed: {err}", case.name()));
        }
        AppendEqualityHarnessCase::RawOnlyDurableWithoutEmission => {
            session
                .record_conversation_items_raw_only_durable_without_emission(turn_context, items)
                .await
                .unwrap_or_else(|err| panic!("{} append failed: {err}", case.name()));
        }
        AppendEqualityHarnessCase::WithoutSpineObserve => {
            session
                .record_conversation_items_without_spine_observe(turn_context, items)
                .await
                .unwrap_or_else(|err| panic!("{} append failed: {err}", case.name()));
        }
        AppendEqualityHarnessCase::SpineControlOverlayOnly => {
            session
                .record_conversation_items_spine_control_overlay_only(turn_context, items)
                .await
                .unwrap_or_else(|err| panic!("{} append failed: {err}", case.name()));
        }
    }
}

async fn reconstructed_rollout_history_for_append_harness(
    session: &Session,
    turn_context: &TurnContext,
    rollout_path: &Path,
) -> (Vec<ResponseItem>, bool, Vec<usize>) {
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let reconstructed = session
        .reconstruct_history_from_rollout(turn_context, &resumed.history)
        .await;
    (
        reconstructed.history,
        reconstructed.used_replacement_history,
        reconstructed.spine_rollback_cuts,
    )
}

async fn assert_append_case_history_matches_spine_materialization(
    session: &Session,
    rollout_path: &Path,
    case: AppendEqualityHarnessCase,
) {
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("{} expected resumed rollout history", case.name());
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(rollout_path, &raw_items, &[])
        .unwrap_or_else(|err| {
            panic!(
                "{} load spine runtime from rollout failed: {err}",
                case.name()
            )
        })
        .unwrap_or_else(|| panic!("{} spine sidecar should exist", case.name()));
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .unwrap_or_else(|err| panic!("{} materialize h(PS) failed: {err}", case.name()));
    assert_eq!(
        session.clone_history().await.raw_items(),
        materialized.as_slice(),
        "{} live history differs from replayed h(PS)",
        case.name()
    );
}

#[tokio::test]
async fn append_equality_harness_feature_off_matches_base_for_current_variants() {
    let cases = [
        AppendEqualityHarnessCase::BaseConversation,
        AppendEqualityHarnessCase::BaseConversationWithoutRawEvent,
        AppendEqualityHarnessCase::RawOnlyBestEffort,
        AppendEqualityHarnessCase::RawOnlyDurableWithoutEmission,
        AppendEqualityHarnessCase::WithoutSpineObserve,
        AppendEqualityHarnessCase::SpineControlOverlayOnly,
    ];

    for case in cases {
        let setup_items = case.setup_items();
        let items = case.items();
        let (mut base_session, base_turn_context, _base_rx) =
            make_session_and_context_with_auth_and_config_and_rx(
                CodexAuth::from_api_key("Test API Key"),
                Vec::new(),
                |_config| {},
            )
            .await;
        let base_rollout_path = attach_thread_persistence(
            Arc::get_mut(&mut base_session).expect("base session should be unique"),
        )
        .await;
        assert!(
            base_session.spine.is_none(),
            "{} base session should not have Spine state",
            case.name()
        );

        let (mut feature_off_session, feature_off_turn_context, _feature_off_rx) =
            make_session_and_context_with_auth_and_config_and_rx(
                CodexAuth::from_api_key("Test API Key"),
                Vec::new(),
                |config| {
                    config
                        .features
                        .disable(Feature::SpineJit)
                        .expect("disable spine feature");
                },
            )
            .await;
        let feature_off_rollout_path = attach_thread_persistence(
            Arc::get_mut(&mut feature_off_session).expect("feature-off session should be unique"),
        )
        .await;
        assert!(
            feature_off_session.spine.is_none(),
            "{} feature-off session should not have Spine state",
            case.name()
        );

        if !setup_items.is_empty() {
            base_session
                .record_conversation_items(&base_turn_context, &setup_items)
                .await
                .unwrap_or_else(|err| panic!("{} base setup append failed: {err}", case.name()));
            feature_off_session
                .record_conversation_items(&feature_off_turn_context, &setup_items)
                .await
                .unwrap_or_else(|err| {
                    panic!("{} feature-off setup append failed: {err}", case.name())
                });
        }
        run_append_equality_harness_case(&base_session, &base_turn_context, case, &items).await;
        run_append_equality_harness_case(
            &feature_off_session,
            &feature_off_turn_context,
            case,
            &items,
        )
        .await;

        let base_history = base_session.clone_history().await;
        let feature_off_history = feature_off_session.clone_history().await;
        assert_eq!(
            serde_json::to_vec(base_history.raw_items()).expect("base raw history json"),
            serde_json::to_vec(feature_off_history.raw_items())
                .expect("feature-off raw history json"),
            "{} raw history differs",
            case.name()
        );
        assert_eq!(
            serde_json::to_vec(
                &base_history
                    .clone()
                    .for_prompt(&base_turn_context.model_info.input_modalities)
            )
            .expect("base prompt history json"),
            serde_json::to_vec(
                &feature_off_history
                    .clone()
                    .for_prompt(&feature_off_turn_context.model_info.input_modalities)
            )
            .expect("feature-off prompt history json"),
            "{} prompt history differs",
            case.name()
        );

        let base_reconstructed = reconstructed_rollout_history_for_append_harness(
            &base_session,
            &base_turn_context,
            &base_rollout_path,
        )
        .await;
        let feature_off_reconstructed = reconstructed_rollout_history_for_append_harness(
            &feature_off_session,
            &feature_off_turn_context,
            &feature_off_rollout_path,
        )
        .await;
        assert_eq!(
            serde_json::to_vec(&base_reconstructed.0).expect("base reconstructed json"),
            serde_json::to_vec(&feature_off_reconstructed.0)
                .expect("feature-off reconstructed json"),
            "{} reconstructed history differs",
            case.name()
        );
        assert_eq!(
            base_reconstructed.1,
            feature_off_reconstructed.1,
            "{} replacement-history usage differs",
            case.name()
        );
        assert_eq!(
            base_reconstructed.2,
            feature_off_reconstructed.2,
            "{} rollback cuts differ",
            case.name()
        );
        assert!(
            !SpineStore::has_for_rollout(&base_rollout_path).expect("check base sidecar"),
            "{} base rollout should not have Spine sidecar",
            case.name()
        );
        assert!(
            !SpineStore::has_for_rollout(&feature_off_rollout_path)
                .expect("check feature-off sidecar"),
            "{} feature-off rollout should not have Spine sidecar",
            case.name()
        );
    }
}

#[tokio::test]
async fn append_equality_harness_spine_on_variants_replay_to_live_projection() {
    let cases = [
        AppendEqualityHarnessCase::BaseConversation,
        AppendEqualityHarnessCase::BaseConversationWithoutRawEvent,
    ];

    for case in cases {
        let setup_items = case.setup_items();
        let items = case.items();
        let (mut session, turn_context, _rx) =
            make_session_and_context_with_auth_and_config_and_rx(
                CodexAuth::from_api_key("Test API Key"),
                Vec::new(),
                |config| {
                    config
                        .features
                        .enable(Feature::SpineJit)
                        .expect("enable spine feature");
                },
            )
            .await;
        let rollout_path = attach_thread_persistence(
            Arc::get_mut(&mut session).expect("session should be unique"),
        )
        .await;
        assert!(
            session.spine.is_some(),
            "{} Spine-on session should have Spine state",
            case.name()
        );

        if !setup_items.is_empty() {
            session
                .record_conversation_items(&turn_context, &setup_items)
                .await
                .unwrap_or_else(|err| panic!("{} setup append failed: {err}", case.name()));
        }
        run_append_equality_harness_case(&session, &turn_context, case, &items).await;
        assert_append_case_history_matches_spine_materialization(&session, &rollout_path, case)
            .await;
    }
}

struct PostNextFixture {
    session: Arc<Session>,
    turn_context: Arc<TurnContext>,
    rx: async_channel::Receiver<Event>,
    rollout_path: PathBuf,
    rollout_items: Vec<RolloutItem>,
    raw_items: Vec<Option<ResponseItem>>,
    expected_history: Vec<ResponseItem>,
}

fn assert_post_next_tree(tree: &str) {
    assert!(tree.contains("Cursor: 1.1.2"), "{tree}");
    assert!(tree.contains("[1.1.1] Done"), "{tree}");
    assert!(tree.contains("[1.1.2] Current post next sibling"), "{tree}");
}

async fn assert_post_next_session_state(
    session: &Session,
    rollout_path: &Path,
    raw_items: &[Option<ResponseItem>],
    expected_history: &[ResponseItem],
) {
    assert_eq!(session.clone_history().await.raw_items(), expected_history);
    let runtime = SpineRuntime::load_for_rollout_items(rollout_path, raw_items, &[])
        .expect("load post-next spine runtime")
        .expect("post-next spine sidecar should exist");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(raw_items)
            .expect("materialize post-next h(PS)"),
        expected_history
    );
    assert_post_next_tree(&runtime.render_tree().expect("render post-next tree"));
}

async fn make_spine_session_after_next(summary_text: &str) -> PostNextFixture {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse("post-next-summary", summary_text),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("post-next prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record post-next prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "post-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record post-next open request");
    session
        .test_seed_spine_open_control_request(
            "post-next-open".to_string(),
            "post next child".to_string(),
        )
        .await
        .expect("stage post-next open");
    let open_output = function_output("post-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record post-next open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit post-next open");

    let child_body = assistant_message("post-next child body");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record post-next child body");
    let next_request = spine_call(SPINE_TOOL_NEXT, "post-next");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record post-next request");
    session
        .test_seed_spine_next_control_request(
            "post-next".to_string(),
            "post next sibling".to_string(),
            summary_text.to_string(),
        )
        .await
        .expect("stage post-next");
    let next_output = commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("post-next"),
    )
    .await
    .expect("commit post-next output and record raw evidence");
    assert!(matches!(
        next_output,
        ResponseItem::FunctionCallOutput { call_id, output }
            if call_id == "post-next"
                && output.text_content() == Some("ok")
    ));
    assert_eq!(
        compact_mock.requests().len(),
        0,
        "successful post-next commit should use direct memory without a secondary compact request"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read post-next rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load post-next runtime")
        .expect("post-next spine sidecar should exist");
    let expected_history = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize post-next h(PS)");
    assert_eq!(session.clone_history().await.raw_items(), expected_history);
    assert_post_next_tree(&runtime.render_tree().expect("render post-next tree"));
    assert!(
        expected_history.iter().any(|item| matches!(
            item,
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains(summary_text)
                )
        )),
        "post-next closed sibling memory should be visible"
    );
    assert!(expected_history.iter().any(
        |item| matches!(item, ResponseItem::FunctionCall { call_id, .. } if call_id == "post-next")
    ));
    assert!(expected_history.iter().any(
        |item| matches!(item, ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "post-next")
    ));

    PostNextFixture {
        session,
        turn_context,
        rx,
        rollout_path,
        rollout_items: resumed.history,
        raw_items,
        expected_history,
    }
}

struct MissingOutputCarrierFixture {
    rollout_path: PathBuf,
    raw_items: Vec<Option<ResponseItem>>,
}

fn assert_close_window_tree(tree: &str) {
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("[1.1.1] Done"), "{tree}");
}

async fn assert_close_window_session_state(
    session: &Session,
    rollout_path: &Path,
    raw_items: &[Option<ResponseItem>],
    expected_history: &[ResponseItem],
) {
    assert_eq!(session.clone_history().await.raw_items(), expected_history);
    let runtime = SpineRuntime::load_for_rollout_items(rollout_path, raw_items, &[])
        .expect("load close-window spine runtime")
        .expect("close-window spine sidecar should exist");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(raw_items)
            .expect("materialize close-window h(PS)"),
        expected_history
    );
    assert_close_window_tree(&runtime.render_tree().expect("render close-window tree"));
}

async fn make_spine_close_window_missing_output_carrier(
    summary_text: &str,
) -> MissingOutputCarrierFixture {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("close-window prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record close-window prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "close-window-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record close-window open request");
    session
        .test_seed_spine_open_control_request(
            "close-window-open".to_string(),
            "close window child".to_string(),
        )
        .await
        .expect("stage close-window open");
    let open_output = function_output("close-window-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record close-window open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit close-window open");

    let child_body = assistant_message("close-window child body");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record close-window child body");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "close-window");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close-window request");
    session
        .test_seed_spine_close_control_request("close-window".to_string(), summary_text.to_string())
        .await
        .expect("stage close-window");

    let commit = test_on_toolcall_single(&session, &turn_context, &function_output("close-window"))
        .await
        .expect("commit close-window sidecar only");
    assert_eq!(
        commit.recording(),
        SpineToolOutputRecording::Skip,
        "close reduce boundary should record raw output before returning success"
    );
    assert!(
        commit.has_deferred_tree_update(),
        "close should defer tree update until output carrier is durable"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read close-window rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(
        resumed.history.iter().any(|item| matches!(
            item,
            RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, .. })
                if call_id == "close-window"
        )),
        "reduce success must persist the close output carrier before this fixture corrupts raw-live evidence"
    );
    let mut raw_items = spine_raw_items_after_rollback(&resumed.history);
    let output_index = raw_items
        .iter()
        .position(|item| {
            matches!(
                item,
                Some(ResponseItem::FunctionCallOutput { call_id, .. }) if call_id == "close-window"
            )
        })
        .expect("test setup must include the close output carrier");
    raw_items[output_index] = None;

    MissingOutputCarrierFixture {
        rollout_path,
        raw_items,
    }
}

async fn make_spine_next_window_missing_output_carrier(
    summary_text: &str,
) -> MissingOutputCarrierFixture {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("next-window prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record next-window prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "next-window-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record next-window open request");
    session
        .test_seed_spine_open_control_request(
            "next-window-open".to_string(),
            "next window child".to_string(),
        )
        .await
        .expect("stage next-window open");
    let open_output = function_output("next-window-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record next-window open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit next-window open");

    let child_body = assistant_message("next-window child body");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record next-window child body");
    let next_request = spine_call(SPINE_TOOL_NEXT, "next-window");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next-window request");
    session
        .test_seed_spine_next_control_request(
            "next-window".to_string(),
            "post next sibling".to_string(),
            summary_text.to_string(),
        )
        .await
        .expect("stage next-window");

    let commit = test_on_toolcall_single(&session, &turn_context, &function_output("next-window"))
        .await
        .expect("commit next-window sidecar only");
    assert_eq!(
        commit.recording(),
        SpineToolOutputRecording::Skip,
        "next reduce boundary should record raw output before returning success"
    );
    assert!(
        commit.has_deferred_tree_update(),
        "next should defer tree update until output carrier is durable"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read next-window rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(
        resumed.history.iter().any(|item| matches!(
            item,
            RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, .. })
                if call_id == "next-window"
        )),
        "reduce success must persist the next output carrier before this fixture corrupts raw-live evidence"
    );
    let mut raw_items = spine_raw_items_after_rollback(&resumed.history);
    let output_index = raw_items
        .iter()
        .position(|item| {
            matches!(
                item,
                Some(ResponseItem::FunctionCallOutput { call_id, .. }) if call_id == "next-window"
            )
        })
        .expect("test setup must include the next output carrier");
    raw_items[output_index] = None;

    MissingOutputCarrierFixture {
        rollout_path,
        raw_items,
    }
}

async fn make_spine_session_with_closed_child(
    summary_text: &str,
) -> (Arc<Session>, Arc<TurnContext>, PathBuf, Vec<RolloutItem>) {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse("closed-child-summary", summary_text),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before spine");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");

    let open_request = spine_call_with_args(
        SPINE_TOOL_OPEN,
        "resume-open",
        r#"{"summary":"resumed child"}"#,
    );
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    let open_output = function_output("resume-open");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, open_output)
        .await
        .expect("commit and record open output");

    let child_body = assistant_message("child body before close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record conversation items");

    let close_request = spine_call_with_args(
        SPINE_TOOL_CLOSE,
        "resume-close",
        r#"{"memory":"test node memory"}"#,
    );
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    let close_output = function_output("resume-close");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, close_output)
        .await
        .expect("commit and record close output");

    assert_eq!(compact_mock.requests().len(), 0);
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    (session, turn_context, rollout_path, resumed.history)
}

fn test_session_telemetry_without_metadata() -> SessionTelemetry {
    let exporter = InMemoryMetricExporter::default();
    let metrics = MetricsClient::new(
        MetricsConfig::in_memory("test", "codex-core", env!("CARGO_PKG_VERSION"), exporter)
            .with_runtime_reader(),
    )
    .expect("in-memory metrics client");
    SessionTelemetry::new(
        ThreadId::new(),
        "gpt-5.4",
        "gpt-5.4",
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test_originator".to_string(),
        /*log_user_prompts*/ false,
        "tty".to_string(),
        SessionSource::Cli,
    )
    .with_metrics_without_metadata_tags(metrics)
}

fn find_metric<'a>(resource_metrics: &'a ResourceMetrics, name: &str) -> &'a Metric {
    for scope_metrics in resource_metrics.scope_metrics() {
        for metric in scope_metrics.metrics() {
            if metric.name() == name {
                return metric;
            }
        }
    }
    panic!("metric {name} missing");
}

fn histogram_sum(resource_metrics: &ResourceMetrics, name: &str) -> u64 {
    let metric = find_metric(resource_metrics, name);
    match metric.data() {
        AggregatedMetrics::F64(data) => match data {
            MetricData::Histogram(histogram) => {
                let points: Vec<_> = histogram.data_points().collect();
                assert_eq!(points.len(), 1);
                points[0].sum().round() as u64
            }
            _ => panic!("unexpected histogram aggregation"),
        },
        _ => panic!("unexpected metric data type"),
    }
}

fn skill_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

#[tokio::test]
async fn regular_turn_emits_turn_started_without_waiting_for_startup_prewarm() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let (_tx, startup_prewarm_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = startup_prewarm_rx.await;
        Ok(test_model_client_session())
    });

    sess.set_session_startup_prewarm(
        crate::session_startup_prewarm::SessionStartupPrewarmHandle::new(
            handle,
            std::time::Instant::now(),
            crate::client::WEBSOCKET_CONNECT_TIMEOUT,
        ),
    )
    .await;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        crate::tasks::RegularTask::new(),
    )
    .await;

    let first = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .expect("expected turn started event without waiting for startup prewarm")
        .expect("channel open");
    assert!(matches!(
        first.msg,
        EventMsg::TurnStarted(TurnStartedEvent { turn_id, .. }) if turn_id == tc.sub_id
    ));

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn request_mcp_server_elicitation_auto_accepts_when_auto_deny_is_enabled() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;
    session
        .services
        .mcp_connection_manager
        .read()
        .await
        .set_elicitations_auto_deny(/*auto_deny*/ true);

    let requested_schema: McpElicitationSchema = serde_json::from_value(json!({
        "type": "object",
        "properties": {},
    }))
    .expect("schema should deserialize");
    let response = session
        .request_mcp_server_elicitation(
            turn_context.as_ref(),
            RequestId::String("request-1".into()),
            McpServerElicitationRequestParams {
                thread_id: session.conversation_id.to_string(),
                turn_id: Some(turn_context.sub_id.clone()),
                server_name: "codex_apps".to_string(),
                request: McpServerElicitationRequest::Form {
                    meta: None,
                    message: "Allow this request?".to_string(),
                    requested_schema,
                },
            },
        )
        .await;

    assert_eq!(
        response,
        Some(ElicitationResponse {
            action: ElicitationAction::Accept,
            content: Some(json!({})),
            meta: None,
        })
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn interrupting_regular_turn_waiting_on_startup_prewarm_emits_turn_aborted() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let (_tx, startup_prewarm_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let _ = startup_prewarm_rx.await;
        Ok(test_model_client_session())
    });

    sess.set_session_startup_prewarm(
        crate::session_startup_prewarm::SessionStartupPrewarmHandle::new(
            handle,
            std::time::Instant::now(),
            crate::client::WEBSOCKET_CONNECT_TIMEOUT,
        ),
    )
    .await;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        crate::tasks::RegularTask::new(),
    )
    .await;

    let first = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .expect("expected turn started event without waiting for startup prewarm")
        .expect("channel open");
    assert!(matches!(
        first.msg,
        EventMsg::TurnStarted(TurnStartedEvent { turn_id, .. }) if turn_id == tc.sub_id
    ));

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected turn aborted event")
        .expect("channel open");
    let EventMsg::TurnAborted(TurnAbortedEvent {
        turn_id,
        reason,
        completed_at,
        duration_ms,
    }) = second.msg
    else {
        panic!("expected turn aborted event");
    };
    assert_eq!(turn_id, Some(tc.sub_id.clone()));
    assert_eq!(reason, TurnAbortReason::Interrupted);
    assert!(completed_at.is_some());
    assert!(duration_ms.is_some());
}

fn test_model_client_session() -> crate::client::ModelClientSession {
    let thread_id = ThreadId::try_from("00000000-0000-4000-8000-000000000001")
        .expect("test thread id should be valid");
    crate::client::ModelClient::new(
        /*auth_manager*/ None,
        thread_id.into(),
        thread_id,
        /*installation_id*/ "11111111-1111-4111-8111-111111111111".to_string(),
        ModelProviderInfo::create_openai_provider(/* base_url */ /*base_url*/ None),
        codex_protocol::protocol::SessionSource::Exec,
        /*model_verbosity*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*attestation_provider*/ None,
        /*debug_request_capture_dir*/ None,
    )
    .new_session()
}

fn developer_input_texts(items: &[ResponseItem]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "developer" => {
                Some(content.as_slice())
            }
            _ => None,
        })
        .flat_map(|content| content.iter())
        .filter_map(|item| match item {
            ContentItem::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn developer_message_texts(items: &[ResponseItem]) -> Vec<Vec<&str>> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "developer" => {
                Some(content.as_slice())
            }
            _ => None,
        })
        .map(|content| {
            content
                .iter()
                .filter_map(|item| match item {
                    ContentItem::InputText { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

fn user_input_texts(items: &[ResponseItem]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                Some(content.as_slice())
            }
            _ => None,
        })
        .flat_map(|content| content.iter())
        .filter_map(|item| match item {
            ContentItem::InputText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn write_project_hooks(dot_codex: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dot_codex)?;
    std::fs::write(
        dot_codex.join("hooks.json"),
        r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo hello from hook"
          }
        ]
      }
    ]
  }
}"#,
    )
}

async fn write_project_trust_config(
    codex_home: &Path,
    trusted_projects: &[(&Path, TrustLevel)],
) -> std::io::Result<()> {
    tokio::fs::write(
        codex_home.join(codex_config::CONFIG_TOML_FILE),
        toml::to_string(&ConfigToml {
            projects: Some(
                trusted_projects
                    .iter()
                    .map(|(project, trust_level)| {
                        (
                            project_trust_key(project),
                            ProjectConfig {
                                trust_level: Some(*trust_level),
                            },
                        )
                    })
                    .collect::<std::collections::HashMap<_, _>>(),
            ),
            ..Default::default()
        })
        .expect("serialize config"),
    )
    .await
}

async fn preview_session_start_hooks(
    config: &crate::config::Config,
) -> std::io::Result<Vec<codex_protocol::protocol::HookRunSummary>> {
    let hooks = Hooks::new(HooksConfig {
        feature_enabled: true,
        config_layer_stack: Some(config.config_layer_stack.clone()),
        ..HooksConfig::default()
    });

    Ok(
        hooks.preview_session_start(&codex_hooks::SessionStartRequest {
            session_id: ThreadId::new(),
            cwd: config.cwd.clone(),
            transcript_path: None,
            model: "gpt-5.2".to_string(),
            permission_mode: "default".to_string(),
            source: codex_hooks::SessionStartSource::Startup,
        }),
    )
}

fn test_tool_runtime(session: Arc<Session>, turn_context: Arc<TurnContext>) -> ToolCallRuntime {
    let router = Arc::new(
        ToolRouter::from_config(
            &turn_context.tools_config,
            crate::tools::router::ToolRouterParams {
                mcp_tools: None,
                deferred_mcp_tools: None,
                discoverable_tools: None,
                extension_tool_executors: Vec::new(),
                dynamic_tools: turn_context.dynamic_tools.as_slice(),
            },
        )
        .expect("build tool router"),
    );
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    ToolCallRuntime::new(router, session, turn_context, tracker)
}

fn make_connector(id: &str, name: &str) -> AppInfo {
    AppInfo {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        logo_url: None,
        logo_url_dark: None,
        distribution_channel: None,
        branding: None,
        app_metadata: None,
        labels: None,
        install_url: None,
        is_accessible: true,
        is_enabled: true,
        plugin_display_names: Vec::new(),
    }
}

#[test]
fn assistant_message_stream_parsers_can_be_seeded_from_output_item_added_text() {
    let mut parsers = AssistantMessageStreamParsers::new(/*plan_mode*/ false);
    let item_id = "msg-1";

    let seeded = parsers.seed_item_text(item_id, "hello <oai-mem-citation>doc");
    let parsed = parsers.parse_delta(item_id, "1</oai-mem-citation> world");
    let tail = parsers.finish_item(item_id);

    assert_eq!(seeded.visible_text, "hello ");
    assert_eq!(seeded.citations, Vec::<String>::new());
    assert_eq!(parsed.visible_text, " world");
    assert_eq!(parsed.citations, vec!["doc1".to_string()]);
    assert_eq!(tail.visible_text, "");
    assert_eq!(tail.citations, Vec::<String>::new());
}

#[test]
fn assistant_message_stream_parsers_seed_buffered_prefix_stays_out_of_finish_tail() {
    let mut parsers = AssistantMessageStreamParsers::new(/*plan_mode*/ false);
    let item_id = "msg-1";

    let seeded = parsers.seed_item_text(item_id, "hello <oai-mem-");
    let parsed = parsers.parse_delta(item_id, "citation>doc</oai-mem-citation> world");
    let tail = parsers.finish_item(item_id);

    assert_eq!(seeded.visible_text, "hello ");
    assert_eq!(seeded.citations, Vec::<String>::new());
    assert_eq!(parsed.visible_text, " world");
    assert_eq!(parsed.citations, vec!["doc".to_string()]);
    assert_eq!(tail.visible_text, "");
    assert_eq!(tail.citations, Vec::<String>::new());
}

#[test]
fn assistant_message_stream_parsers_seed_plan_parser_across_added_and_delta_boundaries() {
    let mut parsers = AssistantMessageStreamParsers::new(/*plan_mode*/ true);
    let item_id = "msg-1";

    let seeded = parsers.seed_item_text(item_id, "Intro\n<proposed");
    let parsed = parsers.parse_delta(item_id, "_plan>\n- step\n</proposed_plan>\nOutro");
    let tail = parsers.finish_item(item_id);

    assert_eq!(seeded.visible_text, "Intro\n");
    assert_eq!(
        seeded.plan_segments,
        vec![ProposedPlanSegment::Normal("Intro\n".to_string())]
    );
    assert_eq!(parsed.visible_text, "Outro");
    assert_eq!(
        parsed.plan_segments,
        vec![
            ProposedPlanSegment::ProposedPlanStart,
            ProposedPlanSegment::ProposedPlanDelta("- step\n".to_string()),
            ProposedPlanSegment::ProposedPlanEnd,
            ProposedPlanSegment::Normal("Outro".to_string()),
        ]
    );
    assert_eq!(tail.visible_text, "");
    assert!(tail.plan_segments.is_empty());
}

#[test]
fn validated_network_policy_amendment_host_allows_normalized_match() {
    let amendment = NetworkPolicyAmendment {
        host: "ExAmPlE.Com.:443".to_string(),
        action: NetworkPolicyRuleAction::Allow,
    };
    let context = NetworkApprovalContext {
        host: "example.com".to_string(),
        protocol: NetworkApprovalProtocol::Https,
    };

    let host = Session::validated_network_policy_amendment_host(&amendment, &context)
        .expect("normalized hosts should match");

    assert_eq!(host, "example.com");
}

#[test]
fn validated_network_policy_amendment_host_rejects_mismatch() {
    let amendment = NetworkPolicyAmendment {
        host: "evil.example.com".to_string(),
        action: NetworkPolicyRuleAction::Deny,
    };
    let context = NetworkApprovalContext {
        host: "api.example.com".to_string(),
        protocol: NetworkApprovalProtocol::Https,
    };

    let err = Session::validated_network_policy_amendment_host(&amendment, &context)
        .expect_err("mismatched hosts should be rejected");

    let message = err.to_string();
    assert!(message.contains("does not match approved host"));
}

#[tokio::test]
async fn start_managed_network_proxy_applies_execpolicy_network_rules() -> anyhow::Result<()> {
    let permission_profile = PermissionProfile::workspace_write();
    let spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        /*requirements*/ None,
        &permission_profile,
    )?;
    let mut exec_policy = Policy::empty();
    exec_policy.add_network_rule(
        "example.com",
        NetworkRuleProtocol::Https,
        Decision::Allow,
        /*justification*/ None,
    )?;

    let (started_proxy, _) = Session::start_managed_network_proxy(
        &spec,
        &exec_policy,
        &permission_profile,
        /*network_policy_decider*/ None,
        /*blocked_request_observer*/ None,
        /*managed_network_requirements_enabled*/ false,
        crate::config::NetworkProxyAuditMetadata::default(),
    )
    .await?;

    let current_cfg = started_proxy.proxy().current_cfg().await?;
    assert_eq!(
        current_cfg.network.allowed_domains(),
        Some(vec!["example.com".to_string()])
    );
    Ok(())
}

#[tokio::test]
async fn start_managed_network_proxy_ignores_invalid_execpolicy_network_rules() -> anyhow::Result<()>
{
    let permission_profile = PermissionProfile::workspace_write();
    let spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        Some(NetworkConstraints {
            domains: Some(NetworkDomainPermissionsToml {
                entries: std::collections::BTreeMap::from([(
                    "managed.example.com".to_string(),
                    NetworkDomainPermissionToml::Allow,
                )]),
            }),
            managed_allowed_domains_only: Some(true),
            ..Default::default()
        }),
        &permission_profile,
    )?;
    let mut exec_policy = Policy::empty();
    exec_policy.add_network_rule(
        "example.com",
        NetworkRuleProtocol::Https,
        Decision::Allow,
        /*justification*/ None,
    )?;

    let (started_proxy, _) = Session::start_managed_network_proxy(
        &spec,
        &exec_policy,
        &permission_profile,
        /*network_policy_decider*/ None,
        /*blocked_request_observer*/ None,
        /*managed_network_requirements_enabled*/ false,
        crate::config::NetworkProxyAuditMetadata::default(),
    )
    .await?;

    let current_cfg = started_proxy.proxy().current_cfg().await?;
    assert_eq!(
        current_cfg.network.allowed_domains(),
        Some(vec!["managed.example.com".to_string()])
    );
    Ok(())
}

#[tokio::test]
async fn managed_network_proxy_decider_survives_full_access_start() -> anyhow::Result<()> {
    let full_access_permission_profile = PermissionProfile::Disabled;
    let spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        Some(NetworkConstraints {
            enabled: Some(true),
            ..Default::default()
        }),
        &full_access_permission_profile,
    )?;
    let exec_policy = Policy::empty();
    let decider_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let network_policy_decider: Arc<dyn codex_network_proxy::NetworkPolicyDecider> = Arc::new({
        let decider_calls = Arc::clone(&decider_calls);
        move |_request| {
            decider_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { codex_network_proxy::NetworkDecision::ask("not_allowed") }
        }
    });

    let (started_proxy, _) = Session::start_managed_network_proxy(
        &spec,
        &exec_policy,
        &full_access_permission_profile,
        Some(network_policy_decider),
        /*blocked_request_observer*/ None,
        /*managed_network_requirements_enabled*/ true,
        crate::config::NetworkProxyAuditMetadata::default(),
    )
    .await?;

    let spec = spec.recompute_for_permission_profile(&PermissionProfile::workspace_write())?;
    spec.apply_to_started_proxy(&started_proxy).await?;
    let current_cfg = started_proxy.proxy().current_cfg().await?;
    assert_eq!(current_cfg.network.allowed_domains(), None);

    use tokio::io::AsyncReadExt as _;
    use tokio::io::AsyncWriteExt as _;

    let mut stream = tokio::net::TcpStream::connect(started_proxy.proxy().http_addr()).await?;
    stream
        .write_all(
            b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n",
        )
        .await?;
    let mut buffer = [0_u8; 4096];
    let bytes_read = tokio::time::timeout(StdDuration::from_secs(2), stream.read(&mut buffer))
        .await
        .expect("timed out waiting for proxy response")?;
    let response = String::from_utf8_lossy(&buffer[..bytes_read]);

    assert!(
        response.starts_with("HTTP/1.1 403 Forbidden"),
        "unexpected proxy response: {response}"
    );
    assert!(
        response.contains("x-proxy-error: blocked-by-allowlist"),
        "unexpected proxy response: {response}"
    );
    assert_eq!(
        decider_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "unexpected proxy response: {response}"
    );
    Ok(())
}

#[tokio::test]
async fn new_turn_refreshes_managed_network_proxy_for_sandbox_change() -> anyhow::Result<()> {
    let (mut session, _turn_context) = make_session_and_context().await;
    let initial_permission_profile = PermissionProfile::workspace_write();
    let initial_policy = SandboxPolicy::new_workspace_write_policy();

    let mut network_config = NetworkProxyConfig::default();
    network_config
        .network
        .set_allowed_domains(vec!["evil.com".to_string()]);
    let requirements = NetworkConstraints {
        domains: Some(NetworkDomainPermissionsToml {
            entries: std::collections::BTreeMap::from([(
                "*.example.com".to_string(),
                NetworkDomainPermissionToml::Allow,
            )]),
        }),
        ..Default::default()
    };
    let spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        network_config,
        Some(requirements),
        &initial_permission_profile,
    )?;
    let (started_proxy, _) = Session::start_managed_network_proxy(
        &spec,
        &Policy::empty(),
        &initial_permission_profile,
        /*network_policy_decider*/ None,
        /*blocked_request_observer*/ None,
        /*managed_network_requirements_enabled*/ false,
        crate::config::NetworkProxyAuditMetadata::default(),
    )
    .await?;
    assert_eq!(
        started_proxy
            .proxy()
            .current_cfg()
            .await?
            .network
            .allowed_domains(),
        Some(vec!["*.example.com".to_string(), "evil.com".to_string()])
    );

    {
        let mut state = session.state.lock().await;
        let mut config = (*state.session_configuration.original_config_do_not_use).clone();
        config.permissions.network = Some(spec);
        let cwd = config.cwd.clone();
        config
            .permissions
            .set_legacy_sandbox_policy(initial_policy.clone(), cwd.as_path())
            .expect("test setup should allow sandbox policy");
        state.session_configuration.original_config_do_not_use = Arc::new(config);
        state
            .session_configuration
            .set_permission_profile_for_tests(initial_permission_profile)
            .expect("test setup should allow permission profile");
    }
    session.services.network_proxy = Some(started_proxy);

    session
        .new_turn_with_sub_id(
            "sandbox-policy-change".to_string(),
            SessionSettingsUpdate {
                sandbox_policy: Some(SandboxPolicy::DangerFullAccess),
                ..Default::default()
            },
        )
        .await?;

    let started_proxy = session
        .services
        .network_proxy
        .as_ref()
        .expect("managed network proxy should be present");
    assert_eq!(
        started_proxy
            .proxy()
            .current_cfg()
            .await?
            .network
            .allowed_domains(),
        Some(vec!["*.example.com".to_string()])
    );

    Ok(())
}

#[tokio::test]
async fn danger_full_access_turns_do_not_expose_managed_network_proxy() -> anyhow::Result<()> {
    let network_spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        Some(NetworkConstraints {
            enabled: Some(true),
            ..Default::default()
        }),
        &PermissionProfile::Disabled,
    )?;

    let session = make_session_with_config(move |config| {
        let cwd = config.cwd.clone();
        config
            .permissions
            .set_legacy_sandbox_policy(SandboxPolicy::DangerFullAccess, cwd.as_path())
            .expect("test setup should allow sandbox policy");
        config.permissions.network = Some(network_spec);
    })
    .await?;

    let turn_context = session.new_default_turn().await;
    assert!(turn_context.network.is_none());
    Ok(())
}

#[tokio::test]
async fn danger_full_access_tool_attempts_do_not_enforce_managed_network() -> anyhow::Result<()> {
    #[derive(Default)]
    struct ProbeToolRuntime {
        enforce_managed_network: Vec<bool>,
    }

    impl crate::tools::sandboxing::Approvable<()> for ProbeToolRuntime {
        type ApprovalKey = String;

        fn approval_keys(&self, _req: &()) -> Vec<Self::ApprovalKey> {
            vec!["probe".to_string()]
        }

        fn start_approval_async<'a>(
            &'a mut self,
            _req: &'a (),
            _ctx: crate::tools::sandboxing::ApprovalCtx<'a>,
        ) -> futures::future::BoxFuture<'a, ReviewDecision> {
            Box::pin(async { ReviewDecision::Approved })
        }
    }

    impl crate::tools::sandboxing::Sandboxable for ProbeToolRuntime {
        fn sandbox_preference(&self) -> codex_sandboxing::SandboxablePreference {
            codex_sandboxing::SandboxablePreference::Auto
        }
    }

    impl crate::tools::sandboxing::ToolRuntime<(), ()> for ProbeToolRuntime {
        async fn run(
            &mut self,
            _req: &(),
            attempt: &crate::tools::sandboxing::SandboxAttempt<'_>,
            _ctx: &crate::tools::sandboxing::ToolCtx,
        ) -> Result<(), crate::tools::sandboxing::ToolError> {
            self.enforce_managed_network
                .push(attempt.enforce_managed_network);
            Ok(())
        }
    }

    let network_spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        Some(NetworkConstraints {
            enabled: Some(true),
            ..Default::default()
        }),
        &PermissionProfile::Disabled,
    )?;

    let session = make_session_with_config(move |config| {
        let cwd = config.cwd.clone();
        config
            .permissions
            .set_legacy_sandbox_policy(SandboxPolicy::DangerFullAccess, cwd.as_path())
            .expect("test setup should allow sandbox policy");
        config.permissions.network = Some(network_spec);

        let layers = config
            .config_layer_stack
            .get_layers(
                ConfigLayerStackOrdering::LowestPrecedenceFirst,
                /*include_disabled*/ true,
            )
            .into_iter()
            .cloned()
            .collect();
        let mut requirements = config.config_layer_stack.requirements().clone();
        requirements.network = Some(Sourced::new(
            NetworkConstraints {
                enabled: Some(true),
                ..Default::default()
            },
            RequirementSource::CloudRequirements,
        ));
        let mut requirements_toml = config.config_layer_stack.requirements_toml().clone();
        requirements_toml.network = Some(codex_config::NetworkRequirementsToml {
            enabled: Some(true),
            ..Default::default()
        });
        config.config_layer_stack = ConfigLayerStack::new(layers, requirements, requirements_toml)
            .expect("rebuild config layer stack with network requirements");
    })
    .await?;

    let turn = session.new_default_turn().await;
    assert!(turn.network.is_none());

    let mut orchestrator = crate::tools::orchestrator::ToolOrchestrator::new();
    let mut tool = ProbeToolRuntime::default();
    let tool_ctx = crate::tools::sandboxing::ToolCtx {
        session: Arc::clone(&session),
        turn: Arc::clone(&turn),
        call_id: "probe-call".to_string(),
        tool_name: codex_tools::ToolName::plain("probe"),
    };

    orchestrator
        .run(
            &mut tool,
            &(),
            &tool_ctx,
            turn.as_ref(),
            AskForApproval::Never,
        )
        .await
        .expect("probe runtime should succeed");

    assert_eq!(tool.enforce_managed_network, vec![false]);

    Ok(())
}

#[tokio::test]
async fn workspace_write_turns_continue_to_expose_managed_network_proxy() -> anyhow::Result<()> {
    let permission_profile = PermissionProfile::workspace_write();
    let sandbox_policy = SandboxPolicy::new_workspace_write_policy();
    let network_spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        Some(NetworkConstraints {
            enabled: Some(true),
            ..Default::default()
        }),
        &permission_profile,
    )?;

    let session = make_session_with_config(move |config| {
        let cwd = config.cwd.clone();
        config
            .permissions
            .set_legacy_sandbox_policy(sandbox_policy, cwd.as_path())
            .expect("test setup should allow sandbox policy");
        config.permissions.network = Some(network_spec);
    })
    .await?;

    let turn_context = session.new_default_turn().await;
    assert!(turn_context.network.is_some());
    Ok(())
}

#[tokio::test]
async fn user_shell_commands_do_not_inherit_managed_network_proxy() -> anyhow::Result<()> {
    let permission_profile = PermissionProfile::workspace_write();
    let sandbox_policy = SandboxPolicy::new_workspace_write_policy();
    let network_spec = crate::config::NetworkProxySpec::from_config_and_constraints(
        NetworkProxyConfig::default(),
        Some(NetworkConstraints {
            enabled: Some(true),
            ..Default::default()
        }),
        &permission_profile,
    )?;

    let (session, rx) = make_session_with_config_and_rx(move |config| {
        let cwd = config.cwd.clone();
        config
            .permissions
            .set_legacy_sandbox_policy(sandbox_policy, cwd.as_path())
            .expect("test setup should allow sandbox policy");
        config.permissions.network = Some(network_spec);
    })
    .await?;

    let turn_context = session.new_default_turn().await;
    assert!(turn_context.network.is_some());

    #[cfg(windows)]
    let command = r#"$val = $env:HTTP_PROXY; if ([string]::IsNullOrEmpty($val)) { $val = 'not-set' } ; [System.Console]::Write($val)"#.to_string();
    #[cfg(not(windows))]
    let command = r#"sh -c "printf '%s' \"${HTTP_PROXY:-not-set}\"""#.to_string();

    execute_user_shell_command(
        Arc::clone(&session),
        turn_context,
        command,
        CancellationToken::new(),
        UserShellCommandMode::StandaloneTurn,
    )
    .await;

    loop {
        let event = rx.recv().await.expect("channel open");
        if let EventMsg::ExecCommandEnd(event) = event.msg {
            assert_eq!(event.exit_code, 0);
            assert_eq!(event.stdout.trim(), "not-set");
            break;
        }
    }

    Ok(())
}

#[tokio::test]
async fn get_base_instructions_no_user_content() {
    let prompt_with_apply_patch_instructions =
        include_str!("../../prompt_with_apply_patch_instructions.md");
    let models_response = bundled_models_response()
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"));
    let model_info_for_slug = |slug: &str, config: &Config| {
        let model = models_response
            .models
            .iter()
            .find(|candidate| candidate.slug == slug)
            .cloned()
            .unwrap_or_else(|| panic!("model slug {slug} is missing from models.json"));
        model_info::with_config_overrides(model, &config.to_models_manager_config())
    };
    let test_cases = vec![
        InstructionsTestCase {
            slug: "gpt-5.4",
            expects_apply_patch_description: false,
        },
        InstructionsTestCase {
            slug: "gpt-5.4-mini",
            expects_apply_patch_description: false,
        },
        InstructionsTestCase {
            slug: "gpt-5.3-codex",
            expects_apply_patch_description: false,
        },
        InstructionsTestCase {
            slug: "gpt-5.2",
            expects_apply_patch_description: false,
        },
    ];

    let (session, _turn_context) = make_session_and_context().await;
    let config = test_config().await;

    for test_case in test_cases {
        let model_info = model_info_for_slug(test_case.slug, &config);
        if test_case.expects_apply_patch_description {
            assert_eq!(
                model_info.base_instructions.as_str(),
                prompt_with_apply_patch_instructions
            );
        }

        {
            let mut state = session.state.lock().await;
            state.session_configuration.base_instructions = model_info.base_instructions.clone();
        }

        let base_instructions = session.get_base_instructions().await;
        assert_eq!(base_instructions.text, model_info.base_instructions);
    }
}

#[tokio::test]
async fn reload_user_config_layer_updates_effective_apps_config() {
    let (session, _turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    let config_toml_path = codex_home.join(CONFIG_TOML_FILE);
    std::fs::write(
        &config_toml_path,
        "[apps.calendar]\nenabled = false\ndestructive_enabled = false\n",
    )
    .expect("write user config");

    session.reload_user_config_layer().await;

    let config = session.get_config().await;
    let apps_toml = config
        .config_layer_stack
        .effective_config()
        .as_table()
        .and_then(|table| table.get("apps"))
        .cloned()
        .expect("apps table");
    let apps = codex_config::types::AppsConfigToml::deserialize(apps_toml)
        .expect("deserialize apps config");
    let app = apps
        .apps
        .get("calendar")
        .expect("calendar app config exists");

    assert!(!app.enabled);
    assert_eq!(app.destructive_enabled, Some(false));
}

#[tokio::test]
async fn reload_user_config_layer_updates_base_and_selected_profile_layers() {
    let (session, _turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    let base_config_path = codex_home.join(CONFIG_TOML_FILE);
    let profile_config_path = codex_home.join("work.config.toml");
    std::fs::write(
        &base_config_path,
        "model = \"base\"\napproval_policy = \"on-failure\"\n",
    )
    .expect("write base user config");
    std::fs::write(&profile_config_path, "model = \"profile-old\"\n")
        .expect("write profile user config");
    let config = ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.to_path_buf())
        .loader_overrides(LoaderOverrides {
            user_config_path: Some(profile_config_path.abs()),
            user_config_profile: Some("work".parse().expect("profile-v2 name")),
            ..LoaderOverrides::without_managed_config_for_tests()
        })
        .build()
        .await
        .expect("load profile config");
    {
        let mut state = session.state.lock().await;
        state.session_configuration.original_config_do_not_use = Arc::new(config);
    }
    std::fs::write(
        &base_config_path,
        "model = \"base\"\napproval_policy = \"never\"\n",
    )
    .expect("update base user config");
    std::fs::write(&profile_config_path, "model = \"profile-new\"\n")
        .expect("update profile user config");

    session.reload_user_config_layer().await;

    let config = session.get_config().await;
    assert_eq!(
        config
            .config_layer_stack
            .get_user_config_file()
            .map(codex_utils_absolute_path::AbsolutePathBuf::as_path),
        Some(profile_config_path.as_path())
    );
    let effective_user_config = config
        .config_layer_stack
        .effective_user_config()
        .expect("merged user config");
    assert_eq!(
        effective_user_config
            .get("model")
            .and_then(toml::Value::as_str),
        Some("profile-new")
    );
    assert_eq!(
        effective_user_config
            .get("approval_policy")
            .and_then(toml::Value::as_str),
        Some("never")
    );
}

#[tokio::test]
async fn reload_user_config_layer_refreshes_hooks() -> anyhow::Result<()> {
    let session = make_session_with_config(|config| {
        config
            .features
            .enable(Feature::CodexHooks)
            .expect("enable Codex hooks");
    })
    .await?;
    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home)?;
    let config_toml_path = codex_home.join(CONFIG_TOML_FILE);
    let user_config: codex_config::TomlValue = serde_json::from_value(serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": "python3 /tmp/user.py",
                }],
            }],
        },
    }))?;

    let request = codex_hooks::SessionStartRequest {
        session_id: session.conversation_id,
        cwd: session.get_config().await.cwd.clone(),
        transcript_path: None,
        model: "gpt-5.2".to_string(),
        permission_mode: "default".to_string(),
        source: codex_hooks::SessionStartSource::Startup,
    };
    assert!(session.hooks().preview_session_start(&request).is_empty());

    let config = session.get_config().await;
    let hook_list = codex_hooks::list_hooks(codex_hooks::HooksConfig {
        feature_enabled: true,
        config_layer_stack: Some(
            config
                .config_layer_stack
                .with_user_config(&config_toml_path, user_config.clone()),
        ),
        ..codex_hooks::HooksConfig::default()
    });
    assert_eq!(hook_list.hooks.len(), 1);
    assert_eq!(
        hook_list.hooks[0].trust_status,
        codex_protocol::protocol::HookTrustStatus::Untrusted
    );

    let trusted_user_config: codex_config::TomlValue = serde_json::from_value(serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": "python3 /tmp/user.py",
                }],
            }],
            "state": {
                hook_list.hooks[0].key.clone(): {
                    "trusted_hash": hook_list.hooks[0].current_hash.clone(),
                },
            },
        },
    }))?;
    std::fs::write(&config_toml_path, toml::to_string(&trusted_user_config)?)?;

    session.reload_user_config_layer().await;

    assert_eq!(session.hooks().preview_session_start(&request).len(), 1);
    Ok(())
}

#[tokio::test]
async fn refresh_runtime_config_refreshes_hooks() -> anyhow::Result<()> {
    let (session, _turn_context) = make_session_and_context().await;
    {
        let mut state = session.state.lock().await;
        let mut config = (*state.session_configuration.original_config_do_not_use).clone();
        config
            .features
            .enable(Feature::CodexHooks)
            .expect("enable Codex hooks");
        state.session_configuration.original_config_do_not_use = Arc::new(config);
    }
    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home)?;
    let config_toml_path = codex_home.join(CONFIG_TOML_FILE);
    #[derive(serde::Serialize)]
    struct NormalizedHookIdentity {
        event_name: &'static str,
        #[serde(flatten)]
        group: codex_config::MatcherGroup,
    }
    let trusted_hash = {
        let identity = NormalizedHookIdentity {
            event_name: "session_start",
            group: codex_config::MatcherGroup {
                matcher: None,
                hooks: vec![codex_config::HookHandlerConfig::Command {
                    command: "python3 /tmp/user.py".to_string(),
                    command_windows: None,
                    timeout_sec: Some(600),
                    r#async: false,
                    status_message: None,
                }],
            },
        };
        let identity = codex_config::TomlValue::try_from(identity)?;
        codex_config::version_for_toml(&identity)
    };
    let hook_key = format!("{}:session_start:0:0", config_toml_path.display());
    let trusted_user_config: codex_config::TomlValue = serde_json::from_value(serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": "python3 /tmp/user.py",
                }],
            }],
            "state": {
                hook_key: {
                    "trusted_hash": trusted_hash,
                },
            },
        },
    }))?;
    std::fs::write(&config_toml_path, toml::to_string(&trusted_user_config)?)?;

    let request = codex_hooks::SessionStartRequest {
        session_id: session.conversation_id,
        cwd: session.get_config().await.cwd.clone(),
        transcript_path: None,
        model: "gpt-5.2".to_string(),
        permission_mode: "default".to_string(),
        source: codex_hooks::SessionStartSource::Startup,
    };
    assert!(session.hooks().preview_session_start(&request).is_empty());

    let next_config = load_latest_config_for_session(&session).await;
    session.refresh_runtime_config(next_config).await;

    assert_eq!(session.hooks().preview_session_start(&request).len(), 1);
    Ok(())
}

#[tokio::test]
async fn reload_user_config_layer_updates_effective_tool_suggest_config() {
    let (session, _turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    let config_toml_path = codex_home.join(CONFIG_TOML_FILE);
    std::fs::write(
        &config_toml_path,
        r#"[tool_suggest]
disabled_tools = [
  { type = "connector", id = " calendar " },
  { type = "plugin", id = "slack@openai-curated" },
]
"#,
    )
    .expect("write user config");

    session.reload_user_config_layer().await;

    let config = session.get_config().await;
    assert_eq!(
        config.tool_suggest.disabled_tools,
        vec![
            ToolSuggestDisabledTool::connector("calendar"),
            ToolSuggestDisabledTool::plugin("slack@openai-curated"),
        ]
    );
}

#[tokio::test]
async fn refresh_runtime_config_updates_runtime_refreshable_fields_and_keeps_session_static_settings()
 {
    let (session, _turn_context) = make_session_and_context().await;
    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    std::fs::write(
        codex_home.join(CONFIG_TOML_FILE),
        r#"[apps.calendar]
enabled = false
destructive_enabled = false

[tool_suggest]
disabled_tools = [
  { type = "connector", id = " calendar " },
  { type = "plugin", id = "slack@openai-curated" },
]
"#,
    )
    .expect("write user config");

    let original = session.get_config().await;
    let mut next_config = load_latest_config_for_session(&session).await;
    next_config.model = Some("gpt-5.4".to_string());
    next_config.notify = Some(vec!["echo".to_string()]);

    session.refresh_runtime_config(next_config).await;

    let config = session.get_config().await;
    let apps_toml = config
        .config_layer_stack
        .effective_config()
        .as_table()
        .and_then(|table| table.get("apps"))
        .cloned()
        .expect("apps table");
    let apps = codex_config::types::AppsConfigToml::deserialize(apps_toml)
        .expect("deserialize apps config");
    let app = apps
        .apps
        .get("calendar")
        .expect("calendar app config exists");

    assert!(!app.enabled);
    assert_eq!(app.destructive_enabled, Some(false));
    assert_eq!(config.model, original.model);
    assert_eq!(config.notify, original.notify);
    assert_eq!(
        config.tool_suggest.disabled_tools,
        vec![
            ToolSuggestDisabledTool::connector("calendar"),
            ToolSuggestDisabledTool::plugin("slack@openai-curated"),
        ]
    );
}

#[test]
fn filter_connectors_for_input_skips_duplicate_slug_mentions() {
    let connectors = vec![
        make_connector("one", "Foo Bar"),
        make_connector("two", "Foo-Bar"),
    ];
    let input = vec![user_message("use $foo-bar")];
    let explicitly_enabled_connectors = HashSet::new();
    let skill_name_counts_lower = HashMap::new();

    let selected = filter_connectors_for_input(
        &connectors,
        &input,
        &explicitly_enabled_connectors,
        &skill_name_counts_lower,
    );

    assert_eq!(selected, Vec::new());
}

#[test]
fn filter_connectors_for_input_skips_when_skill_name_conflicts() {
    let connectors = vec![make_connector("one", "Todoist")];
    let input = vec![user_message("use $todoist")];
    let explicitly_enabled_connectors = HashSet::new();
    let skill_name_counts_lower = HashMap::from([("todoist".to_string(), 1)]);

    let selected = filter_connectors_for_input(
        &connectors,
        &input,
        &explicitly_enabled_connectors,
        &skill_name_counts_lower,
    );

    assert_eq!(selected, Vec::new());
}

#[test]
fn filter_connectors_for_input_skips_disabled_connectors() {
    let mut connector = make_connector("calendar", "Calendar");
    connector.is_enabled = false;
    let input = vec![user_message("use $calendar")];
    let explicitly_enabled_connectors = HashSet::new();
    let selected = filter_connectors_for_input(
        &[connector],
        &input,
        &explicitly_enabled_connectors,
        &HashMap::new(),
    );

    assert_eq!(selected, Vec::new());
}

#[test]
fn filter_connectors_for_input_skips_plugin_mentions() {
    let connectors = vec![make_connector("figma", "Figma")];
    let input = vec![user_message("use [@figma](plugin://figma@openai-curated)")];
    let explicitly_enabled_connectors = HashSet::new();
    let selected = filter_connectors_for_input(
        &connectors,
        &input,
        &explicitly_enabled_connectors,
        &HashMap::new(),
    );

    assert_eq!(selected, Vec::new());
}

#[test]
fn collect_explicit_app_ids_from_skill_items_includes_linked_mentions() {
    let connectors = vec![make_connector("calendar", "Calendar")];
    let skill_items = vec![skill_message(
        "<skill>\n<name>demo</name>\n<path>/tmp/skills/demo/SKILL.md</path>\nuse [$calendar](app://calendar)\n</skill>",
    )];

    let connector_ids =
        collect_explicit_app_ids_from_skill_items(&skill_items, &connectors, &HashMap::new());

    assert_eq!(connector_ids, HashSet::from(["calendar".to_string()]));
}

#[test]
fn collect_explicit_app_ids_from_skill_items_resolves_unambiguous_plain_mentions() {
    let connectors = vec![make_connector("calendar", "Calendar")];
    let skill_items = vec![skill_message(
        "<skill>\n<name>demo</name>\n<path>/tmp/skills/demo/SKILL.md</path>\nuse $calendar\n</skill>",
    )];

    let connector_ids =
        collect_explicit_app_ids_from_skill_items(&skill_items, &connectors, &HashMap::new());

    assert_eq!(connector_ids, HashSet::from(["calendar".to_string()]));
}

#[test]
fn collect_explicit_app_ids_from_skill_items_skips_plain_mentions_with_skill_conflicts() {
    let connectors = vec![make_connector("calendar", "Calendar")];
    let skill_items = vec![skill_message(
        "<skill>\n<name>demo</name>\n<path>/tmp/skills/demo/SKILL.md</path>\nuse $calendar\n</skill>",
    )];
    let skill_name_counts_lower = HashMap::from([("calendar".to_string(), 1)]);

    let connector_ids = collect_explicit_app_ids_from_skill_items(
        &skill_items,
        &connectors,
        &skill_name_counts_lower,
    );

    assert_eq!(connector_ids, HashSet::<String>::new());
}

#[tokio::test]
async fn reconstruct_history_matches_live_compactions() {
    let (session, turn_context) = make_session_and_context().await;
    let (rollout_items, expected) = sample_rollout(&session, &turn_context).await;

    let reconstruction_turn = session.new_default_turn().await;
    let reconstructed = session
        .reconstruct_history_from_rollout(reconstruction_turn.as_ref(), &rollout_items)
        .await;

    assert_eq!(expected, reconstructed.history);
}

#[tokio::test]
async fn reconstruct_history_uses_replacement_history_verbatim() {
    let (session, turn_context) = make_session_and_context().await;
    let summary_item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "summary".to_string(),
        }],
        phase: None,
    };
    let replacement_history = vec![
        summary_item.clone(),
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "stale developer instructions".to_string(),
            }],
            phase: None,
        },
    ];
    let rollout_items = vec![RolloutItem::Compacted(CompactedItem {
        message: String::new(),
        replacement_history: Some(replacement_history.clone()),
    })];

    let reconstructed = session
        .reconstruct_history_from_rollout(&turn_context, &rollout_items)
        .await;

    assert_eq!(reconstructed.history, replacement_history);
}

#[tokio::test]
async fn record_initial_history_reconstructs_resumed_transcript() {
    let (session, turn_context) = make_session_and_context().await;
    let (rollout_items, expected) = sample_rollout(&session, &turn_context).await;

    session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(PathBuf::from("/tmp/resume.jsonl")),
        }))
        .await
        .expect("record initial history");

    let history = session.state.lock().await.clone_history();
    assert_eq!(expected, history.raw_items());
}

#[tokio::test]
async fn record_initial_history_new_defers_initial_context_until_first_turn() {
    let (session, _turn_context) = make_session_and_context().await;

    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("record initial history");

    let history = session.clone_history().await;
    assert_eq!(history.raw_items().to_vec(), Vec::<ResponseItem>::new());
    assert!(session.reference_context_item().await.is_none());
    assert_eq!(session.previous_turn_settings().await, None);
}

#[tokio::test]
async fn record_initial_history_new_seeds_initial_spine_tree_snapshot() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_auto_compact_token_limit = Some(60_000);
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("record initial history");

    let event = timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("timeout waiting for initial Spine tree snapshot")
        .expect("event");
    let snapshot = match event.msg {
        EventMsg::SpineTreeUpdate(snapshot) => snapshot,
        msg => panic!("expected Spine tree update, got {msg:?}"),
    };
    assert_eq!(event.id, INITIAL_SUBMIT_ID);
    assert_eq!(snapshot.active_node_id, "1.1");
    assert_eq!(snapshot.nodes.len(), 2);
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1")
        .expect("projected root epoch node");
    assert_eq!(root.parent_id, None);
    assert_eq!(root.summary, None);
    assert_eq!(
        root.status,
        codex_protocol::spine_tree::SpineTreeNodeStatus::Opened
    );
    let child = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1")
        .expect("live initial root child");
    assert_eq!(child.parent_id.as_deref(), Some("1"));
    assert_eq!(child.summary, None);
    assert_eq!(
        child.status,
        codex_protocol::spine_tree::SpineTreeNodeStatus::Live
    );
}

#[tokio::test]
async fn spine_tools_hidden_until_sidecar_runtime_ready() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;

    let before_init = session.new_default_turn().await;
    assert!(before_init.tools_config.spine_jit);
    assert!(!before_init.tools_config.spine_trim);
    assert!(
        !before_init.tools_config.spine_jit_tools_visible,
        "feature-on alone must not expose Spine parser-control tools"
    );
    assert!(!before_init.tools_config.spine_trim_tools_visible);

    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;
    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("record initial history");

    let after_init = session.new_default_turn().await;
    assert!(after_init.tools_config.spine_jit);
    assert!(!after_init.tools_config.spine_trim);
    assert!(
        after_init.tools_config.spine_jit_tools_visible,
        "Spine parser-control tools become visible after sidecar/runtime initialization"
    );
    assert!(!after_init.tools_config.spine_trim_tools_visible);
}

#[tokio::test]
async fn review_turn_inherits_spine_tool_visibility_from_parent_turn() {
    let (mut session, hidden_parent_turn_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;

    assert!(
        !hidden_parent_turn_context
            .tools_config
            .spine_jit_tools_visible,
        "feature-on parent turn must stay hidden before sidecar/runtime readiness"
    );
    let hidden_review_tools = crate::session::review::apply_review_spine_tool_visibility(
        hidden_parent_turn_context.tools_config.clone(),
        hidden_parent_turn_context
            .tools_config
            .spine_jit_tools_visible,
        hidden_parent_turn_context
            .tools_config
            .spine_trim_tools_visible,
    );
    assert!(hidden_review_tools.spine_jit);
    assert!(
        !hidden_review_tools.spine_jit_tools_visible,
        "review turn must not expose Spine parser-control tools before parent readiness"
    );
    assert!(!hidden_review_tools.spine_trim_tools_visible);

    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;
    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("record initial history");

    let ready_parent_turn_context = session.new_default_turn().await;
    assert!(
        ready_parent_turn_context
            .tools_config
            .spine_jit_tools_visible,
        "parent turn should become visible after sidecar/runtime initialization"
    );
    let visible_review_tools = crate::session::review::apply_review_spine_tool_visibility(
        hidden_parent_turn_context.tools_config.clone(),
        ready_parent_turn_context
            .tools_config
            .spine_jit_tools_visible,
        ready_parent_turn_context
            .tools_config
            .spine_trim_tools_visible,
    );
    assert!(visible_review_tools.spine_jit);
    assert!(
        visible_review_tools.spine_jit_tools_visible,
        "review turn must inherit Spine parser-control visibility from a ready parent turn"
    );
    assert!(!visible_review_tools.spine_trim_tools_visible);
}

#[tokio::test]
async fn resumed_history_injects_initial_context_on_first_context_update_only() {
    let (session, turn_context) = make_session_and_context().await;
    let (rollout_items, mut expected) = sample_rollout(&session, &turn_context).await;

    session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(PathBuf::from("/tmp/resume.jsonl")),
        }))
        .await
        .expect("record initial history");

    let history_before_seed = session.state.lock().await.clone_history();
    assert_eq!(expected, history_before_seed.raw_items());

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record context updates");
    expected.extend(session.build_initial_context(&turn_context).await);
    let history_after_seed = session.clone_history().await;
    assert_eq!(expected, history_after_seed.raw_items());

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record context updates");
    let history_after_second_seed = session.clone_history().await;
    assert_eq!(
        history_after_seed.raw_items(),
        history_after_second_seed.raw_items()
    );
}

#[tokio::test]
async fn record_initial_history_seeds_token_info_from_rollout() {
    let (session, turn_context) = make_session_and_context().await;
    let (mut rollout_items, _expected) = sample_rollout(&session, &turn_context).await;

    let info1 = TokenUsageInfo {
        total_token_usage: TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 20,
            reasoning_output_tokens: 0,
            total_tokens: 30,
        },
        last_token_usage: TokenUsage {
            input_tokens: 3,
            cached_input_tokens: 0,
            output_tokens: 4,
            reasoning_output_tokens: 0,
            total_tokens: 7,
        },
        model_context_window: Some(1_000),
    };
    let info2 = TokenUsageInfo {
        total_token_usage: TokenUsage {
            input_tokens: 100,
            cached_input_tokens: 50,
            output_tokens: 200,
            reasoning_output_tokens: 25,
            total_tokens: 375,
        },
        last_token_usage: TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 20,
            reasoning_output_tokens: 5,
            total_tokens: 35,
        },
        model_context_window: Some(2_000),
    };

    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: Some(info1),
            rate_limits: None,
        },
    )));
    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: None,
            rate_limits: None,
        },
    )));
    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: Some(info2.clone()),
            rate_limits: None,
        },
    )));
    rollout_items.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: None,
            rate_limits: None,
        },
    )));

    session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(PathBuf::from("/tmp/resume.jsonl")),
        }))
        .await
        .expect("record initial history");

    let actual = session.state.lock().await.token_info();
    assert_eq!(actual, Some(info2));
}

#[tokio::test]
async fn recompute_token_usage_uses_session_base_instructions() {
    let (session, turn_context) = make_session_and_context().await;

    let override_instructions = "SESSION_OVERRIDE_INSTRUCTIONS_ONLY".repeat(120);
    {
        let mut state = session.state.lock().await;
        state.session_configuration.base_instructions = override_instructions.clone();
    }

    let item = user_message("hello");
    session
        .record_into_history(std::slice::from_ref(&item), &turn_context)
        .await;

    let history = session.clone_history().await;
    let session_base_instructions = BaseInstructions {
        text: override_instructions,
    };
    let expected_tokens = history
        .estimate_token_count_with_base_instructions(&session_base_instructions)
        .expect("estimate with session base instructions");
    let model_estimated_tokens = history
        .estimate_token_count(&turn_context)
        .expect("estimate with model instructions");
    assert_ne!(expected_tokens, model_estimated_tokens);

    session.recompute_token_usage(&turn_context).await;

    let actual_tokens = session
        .state
        .lock()
        .await
        .token_info()
        .expect("token info")
        .last_token_usage
        .total_tokens;
    assert_eq!(actual_tokens, expected_tokens.max(0));
}

#[tokio::test]
async fn recompute_token_usage_updates_model_context_window() {
    let (session, mut turn_context) = make_session_and_context().await;

    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage::default(),
            model_context_window: Some(258_400),
        }));
    }

    turn_context.model_info.context_window = Some(128_000);
    turn_context.model_info.effective_context_window_percent = 100;

    session.recompute_token_usage(&turn_context).await;

    let actual = session.state.lock().await.token_info().expect("token info");
    assert_eq!(actual.model_context_window, Some(128_000));
}

#[tokio::test]
async fn record_token_usage_info_notifies_extension_contributors() {
    struct SessionTokenUsageMarker;
    struct ThreadTokenUsageMarker;

    #[derive(Debug, PartialEq, Eq)]
    struct RecordedTokenUsage {
        session_level_id: String,
        thread_level_id: String,
        turn_level_id: String,
        token_usage: TokenUsageInfo,
        saw_session_store: bool,
        saw_thread_store: bool,
    }

    struct TokenUsageRecorder {
        records: Arc<std::sync::Mutex<Vec<RecordedTokenUsage>>>,
    }

    impl codex_extension_api::TokenUsageContributor for TokenUsageRecorder {
        fn on_token_usage(
            &self,
            session_store: &codex_extension_api::ExtensionData,
            thread_store: &codex_extension_api::ExtensionData,
            turn_store: &codex_extension_api::ExtensionData,
            token_usage: &TokenUsageInfo,
        ) {
            self.records
                .lock()
                .expect("token usage records lock")
                .push(RecordedTokenUsage {
                    session_level_id: session_store.level_id().to_string(),
                    thread_level_id: thread_store.level_id().to_string(),
                    turn_level_id: turn_store.level_id().to_string(),
                    token_usage: token_usage.clone(),
                    saw_session_store: session_store.get::<SessionTokenUsageMarker>().is_some(),
                    saw_thread_store: thread_store.get::<ThreadTokenUsageMarker>().is_some(),
                });
        }
    }

    let (mut session, turn_context) = make_session_and_context().await;
    let records = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    builder.token_usage_contributor(Arc::new(TokenUsageRecorder {
        records: Arc::clone(&records),
    }));
    session.services.extensions = Arc::new(builder.build());
    session
        .services
        .session_extension_data
        .insert(SessionTokenUsageMarker);
    session
        .services
        .thread_extension_data
        .insert(ThreadTokenUsageMarker);

    let first_usage = TokenUsage {
        input_tokens: 10,
        cached_input_tokens: 2,
        output_tokens: 20,
        reasoning_output_tokens: 3,
        total_tokens: 33,
    };
    let second_usage = TokenUsage {
        input_tokens: 7,
        cached_input_tokens: 1,
        output_tokens: 8,
        reasoning_output_tokens: 5,
        total_tokens: 20,
    };

    session
        .record_token_usage_info(&turn_context, Some(&first_usage))
        .await;
    session
        .record_token_usage_info(&turn_context, Some(&second_usage))
        .await;

    let mut expected_total_usage = first_usage.clone();
    expected_total_usage.add_assign(&second_usage);
    let expected = vec![
        RecordedTokenUsage {
            session_level_id: session.session_id().to_string(),
            thread_level_id: session.conversation_id.to_string(),
            turn_level_id: turn_context.sub_id.clone(),
            token_usage: TokenUsageInfo {
                total_token_usage: first_usage.clone(),
                last_token_usage: first_usage,
                model_context_window: turn_context.model_context_window(),
            },
            saw_session_store: true,
            saw_thread_store: true,
        },
        RecordedTokenUsage {
            session_level_id: session.session_id().to_string(),
            thread_level_id: session.conversation_id.to_string(),
            turn_level_id: turn_context.sub_id.clone(),
            token_usage: TokenUsageInfo {
                total_token_usage: expected_total_usage,
                last_token_usage: second_usage,
                model_context_window: turn_context.model_context_window(),
            },
            saw_session_store: true,
            saw_thread_store: true,
        },
    ];
    let actual = records
        .lock()
        .expect("token usage records lock")
        .drain(..)
        .collect::<Vec<_>>();
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn config_change_contributor_observes_effective_config_changes() {
    struct SessionConfigMarker;
    struct ThreadConfigMarker;

    #[derive(Debug, PartialEq)]
    struct RecordedConfigChange {
        previous_model: Option<String>,
        new_model: Option<String>,
        previous_disabled_tools: Vec<ToolSuggestDisabledTool>,
        new_disabled_tools: Vec<ToolSuggestDisabledTool>,
        saw_session_store: bool,
        saw_thread_store: bool,
    }

    struct ConfigRecorder {
        records: Arc<std::sync::Mutex<Vec<RecordedConfigChange>>>,
    }

    impl codex_extension_api::ConfigContributor<crate::config::Config> for ConfigRecorder {
        fn on_config_changed(
            &self,
            session_store: &codex_extension_api::ExtensionData,
            thread_store: &codex_extension_api::ExtensionData,
            previous_config: &crate::config::Config,
            new_config: &crate::config::Config,
        ) {
            self.records
                .lock()
                .expect("config change records lock")
                .push(RecordedConfigChange {
                    previous_model: previous_config.model.clone(),
                    new_model: new_config.model.clone(),
                    previous_disabled_tools: previous_config.tool_suggest.disabled_tools.clone(),
                    new_disabled_tools: new_config.tool_suggest.disabled_tools.clone(),
                    saw_session_store: session_store.get::<SessionConfigMarker>().is_some(),
                    saw_thread_store: thread_store.get::<ThreadConfigMarker>().is_some(),
                });
        }
    }

    let (mut session, _turn_context) = make_session_and_context().await;
    let records = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    builder.config_contributor(Arc::new(ConfigRecorder {
        records: Arc::clone(&records),
    }));
    session.services.extensions = Arc::new(builder.build());
    session
        .services
        .session_extension_data
        .insert(SessionConfigMarker);
    session
        .services
        .thread_extension_data
        .insert(ThreadConfigMarker);

    let original_model = session.collaboration_mode().await.model().to_string();
    let original_disabled_tools = session
        .get_config()
        .await
        .tool_suggest
        .disabled_tools
        .clone();
    let next_model = if original_model == "gpt-5.4" {
        "gpt-5.2"
    } else {
        "gpt-5.4"
    };
    let collaboration_mode = session.collaboration_mode().await.with_updates(
        Some(next_model.to_string()),
        /*effort*/ None,
        /*developer_instructions*/ None,
    );
    session
        .update_settings(SessionSettingsUpdate {
            collaboration_mode: Some(collaboration_mode),
            ..Default::default()
        })
        .await
        .expect("update settings");

    let codex_home = session.codex_home().await;
    std::fs::create_dir_all(&codex_home).expect("create codex home");
    std::fs::write(
        codex_home.join(CONFIG_TOML_FILE),
        r#"[tool_suggest]
disabled_tools = [
  { type = "connector", id = " calendar " },
  { type = "plugin", id = "slack@openai-curated" },
]
"#,
    )
    .expect("write user config");
    let next_config = load_latest_config_for_session(&session).await;
    session.refresh_runtime_config(next_config).await;

    let expected_disabled_tools = vec![
        ToolSuggestDisabledTool::connector("calendar"),
        ToolSuggestDisabledTool::plugin("slack@openai-curated"),
    ];
    let expected = vec![
        RecordedConfigChange {
            previous_model: Some(original_model),
            new_model: Some(next_model.to_string()),
            previous_disabled_tools: original_disabled_tools.clone(),
            new_disabled_tools: original_disabled_tools.clone(),
            saw_session_store: true,
            saw_thread_store: true,
        },
        RecordedConfigChange {
            previous_model: Some(next_model.to_string()),
            new_model: Some(next_model.to_string()),
            previous_disabled_tools: original_disabled_tools,
            new_disabled_tools: expected_disabled_tools,
            saw_session_store: true,
            saw_thread_store: true,
        },
    ];
    let actual = records
        .lock()
        .expect("config change records lock")
        .drain(..)
        .collect::<Vec<_>>();
    assert_eq!(expected, actual);
}

#[tokio::test]
async fn record_initial_history_reconstructs_forked_transcript() {
    let (session, turn_context) = make_session_and_context().await;
    let (rollout_items, expected) = sample_rollout(&session, &turn_context).await;

    session
        .record_initial_history(InitialHistory::Forked(rollout_items))
        .await
        .expect("record initial history");

    let history = session.state.lock().await.clone_history();
    assert_eq!(expected, history.raw_items());
}

#[tokio::test]
async fn clone_spine_sidecar_for_fork_replays_interrupted_child_suffix() {
    let (mut source_session, source_context, _source_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let source_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut source_session).expect("source session should be unique"),
    )
    .await;
    source_session
        .on_init()
        .await
        .expect("initialize source spine");
    let source_item = user_message("source-visible");
    source_session
        .record_conversation_items(&source_context, std::slice::from_ref(&source_item))
        .await
        .expect("record source context");
    source_session.ensure_rollout_materialized().await;
    source_session
        .flush_rollout()
        .await
        .expect("flush source rollout");

    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout_path, 1)
        .expect("capture boundary")
        .expect("source sidecar exists");

    let (mut child_session, _child_context, _child_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let child_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut child_session).expect("child session should be unique"),
    )
    .await;
    let marker = user_message("interrupted child suffix");
    let child_raw_items = vec![Some(source_item.clone()), Some(marker.clone())];
    child_session
        .clone_spine_sidecar_for_fork(&boundary, &child_raw_items)
        .await
        .expect("clone and replay child suffix");

    let runtime = SpineRuntime::load_for_rollout_items(&child_rollout_path, &child_raw_items, &[])
        .expect("load child sidecar")
        .expect("child sidecar exists");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&child_raw_items)
            .expect("materialize child h(PS)"),
        vec![
            anchored_user_message(1, "source-visible"),
            anchored_user_message(2, "interrupted child suffix"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_configured_reports_permission_profile_for_external_sandbox() -> anyhow::Result<()>
{
    let server = start_mock_server().await;
    let sandbox_policy = SandboxPolicy::ExternalSandbox {
        network_access: codex_protocol::protocol::NetworkAccess::Restricted,
    };
    let permission_profile = PermissionProfile::External {
        network: NetworkSandboxPolicy::Restricted,
    };
    let expected_permission_profile = permission_profile.clone();
    let mut builder = test_codex().with_config(move |config| {
        config
            .permissions
            .set_permission_profile(permission_profile.clone())
            .expect("set permission profile");
        config
            .set_legacy_sandbox_policy(sandbox_policy)
            .expect("set sandbox policy");
    });

    let test = builder.build(&server).await?;

    assert_eq!(
        test.session_configured.permission_profile, expected_permission_profile,
        "ExternalSandbox is represented explicitly instead of as a lossy root-write profile"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_permission_profile_rebinds_runtime_workspace_roots() -> anyhow::Result<()> {
    let codex_home = tempfile::TempDir::new()?;
    let cwd = tempfile::TempDir::new()?;
    let old_root = test_path_buf("/workspace/old").abs();
    let new_root = test_path_buf("/workspace/new").abs();
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .harness_overrides(crate::config::ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            default_permissions: Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE.to_string()),
            additional_writable_roots: vec![old_root.to_path_buf()],
            ..Default::default()
        })
        .build()
        .await?;

    let session_permission_profile_state = session_permission_profile_state_from_config(&config)?;
    let stored_file_system_policy = session_permission_profile_state
        .permission_profile()
        .file_system_sandbox_policy();
    assert!(
        !stored_file_system_policy
            .can_write_path_with_cwd(old_root.as_path(), config.cwd.as_path()),
        "session permission profile state should keep runtime workspace roots symbolic"
    );

    let mut session_configuration = make_session_configuration_for_tests().await;
    session_configuration.cwd = config.cwd.clone();
    session_configuration.workspace_roots = config.workspace_roots.clone();
    session_configuration.permission_profile_state = session_permission_profile_state;

    let initial_policy = session_configuration.file_system_sandbox_policy();
    assert!(initial_policy.can_write_path_with_cwd(old_root.as_path(), config.cwd.as_path()));

    let updated = session_configuration.apply(&SessionSettingsUpdate {
        workspace_roots: Some(vec![new_root.clone()]),
        ..Default::default()
    })?;
    let updated_policy = updated.file_system_sandbox_policy();
    assert!(updated_policy.can_write_path_with_cwd(new_root.as_path(), updated.cwd.as_path()));
    assert!(!updated_policy.can_write_path_with_cwd(old_root.as_path(), updated.cwd.as_path()));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_startup_context_then_first_turn_diff_snapshot() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let first_forked_request = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.permissions.approval_policy =
            codex_config::Constrained::allow_any(AskForApproval::OnRequest);
    });
    let initial = builder.build(&server).await?;
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    initial
        .codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "fork seed".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
    // Forking reads the persisted rollout JSONL, so force the completed source turn to disk
    // before snapshotting from it.
    initial.codex.ensure_rollout_materialized().await;
    initial
        .codex
        .flush_rollout()
        .await
        .expect("source rollout should flush before fork");

    let mut fork_config = initial.config.clone();
    fork_config.permissions.approval_policy =
        codex_config::Constrained::allow_any(AskForApproval::UnlessTrusted);
    let forked = initial
        .thread_manager
        .fork_thread(
            usize::MAX,
            fork_config.clone(),
            rollout_path,
            /*thread_source*/ None,
            /*persist_extended_history*/ false,
            /*parent_trace*/ None,
        )
        .await?;

    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Plan,
        settings: Settings {
            model: forked.session_configured.model.clone(),
            reasoning_effort: None,
            developer_instructions: Some("Fork turn collaboration instructions.".to_string()),
        },
    };
    forked
        .thread
        .submit(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: Some(AskForApproval::Never),
            approvals_reviewer: None,
            sandbox_policy: None,
            permission_profile: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: Some(collaboration_mode),
            personality: None,
        })
        .await?;

    forked
        .thread
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "after fork".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event(&forked.thread, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = first_forked_request.single_request();
    let snapshot = context_snapshot::format_labeled_requests_snapshot(
        "First request after fork when startup preserves the parent baseline, the fork changes approval policy, and the first forked turn enters plan mode.",
        &[("First Forked Turn Request", &request)],
        &ContextSnapshotOptions::default()
            .render_mode(ContextSnapshotRenderMode::KindWithTextPrefix { max_chars: 96 })
            .strip_capability_instructions()
            .strip_agents_md_user_context(),
    );

    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("snapshots");
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        insta::assert_snapshot!(
            "codex_core__codex_tests__fork_startup_context_then_first_turn_diff",
            snapshot
        );
    });

    Ok(())
}

#[tokio::test]
async fn record_initial_history_forked_hydrates_previous_turn_settings() {
    let (session, turn_context) = make_session_and_context().await;
    let previous_model = "forked-rollout-model";
    let previous_context_item = TurnContextItem {
        turn_id: Some(turn_context.sub_id.clone()),
        trace_id: turn_context.trace_id.clone(),
        #[allow(deprecated)]
        cwd: turn_context.cwd.to_path_buf(),
        current_date: turn_context.current_date.clone(),
        timezone: turn_context.timezone.clone(),
        approval_policy: turn_context.approval_policy.value(),
        sandbox_policy: turn_context.sandbox_policy(),
        permission_profile: None,
        network: None,
        file_system_sandbox_policy: None,
        model: previous_model.to_string(),
        personality: turn_context.personality,
        collaboration_mode: Some(turn_context.collaboration_mode.clone()),
        realtime_active: Some(turn_context.realtime_active),
        effort: turn_context.reasoning_effort,
        summary: turn_context.reasoning_summary,
        user_instructions: None,
        developer_instructions: None,
        final_output_json_schema: None,
        truncation_policy: Some(turn_context.truncation_policy),
    };
    let turn_id = previous_context_item
        .turn_id
        .clone()
        .expect("turn context should have turn_id");
    let rollout_items = vec![
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: turn_id.clone(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(
            codex_protocol::protocol::UserMessageEvent {
                message: "forked seed".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        )),
        RolloutItem::TurnContext(previous_context_item.clone()),
        RolloutItem::EventMsg(EventMsg::TurnComplete(
            codex_protocol::protocol::TurnCompleteEvent {
                turn_id,
                last_agent_message: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            },
        )),
    ];

    session
        .record_initial_history(InitialHistory::Forked(rollout_items))
        .await
        .expect("record initial history");

    let history = session.clone_history().await;
    assert_eq!(
        session.previous_turn_settings().await,
        Some(PreviousTurnSettings {
            model: previous_model.to_string(),
            realtime_active: Some(turn_context.realtime_active),
        })
    );
    assert_eq!(history.raw_items(), &[]);
    assert_eq!(
        serde_json::to_value(session.reference_context_item().await)
            .expect("serialize fork reference context item"),
        serde_json::to_value(Some(previous_context_item))
            .expect("serialize expected reference context item")
    );
}

#[tokio::test]
async fn thread_rollback_drops_last_turn_from_history() {
    let (mut sess, tc, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut sess).expect("session should not have additional references"),
    )
    .await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    let turn_1 = vec![
        user_message("turn 1 user"),
        assistant_message("turn 1 assistant"),
    ];
    let turn_2 = vec![
        user_message("turn 2 user"),
        assistant_message("turn 2 assistant"),
    ];
    let mut full_history = Vec::new();
    full_history.extend(initial_context.clone());
    full_history.extend(turn_1.clone());
    full_history.extend(turn_2);
    sess.replace_history(full_history.clone(), Some(tc.to_turn_context_item()))
        .await;
    let rollout_items: Vec<RolloutItem> = full_history
        .into_iter()
        .map(RolloutItem::ResponseItem)
        .collect();
    sess.persist_rollout_items(&rollout_items).await;
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: "stale-model".to_string(),
        realtime_active: Some(tc.realtime_active),
    }))
    .await;
    {
        let mut state = sess.state.lock().await;
        state.set_reference_context_item(Some(tc.to_turn_context_item()));
    }

    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 1).await;

    let rollback_event = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(rollback_event.num_turns, 1);

    let mut expected = Vec::new();
    expected.extend(initial_context);
    expected.extend(turn_1);

    let history = sess.clone_history().await;
    assert_eq!(expected, history.raw_items());
    assert_eq!(sess.previous_turn_settings().await, None);
    assert!(sess.reference_context_item().await.is_none());

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(resumed.history.iter().any(|item| {
        matches!(
            item,
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback))
            if rollback.num_turns == 1
        )
    }));
}

#[tokio::test]
async fn thread_rollback_clears_history_when_num_turns_exceeds_existing_turns() {
    let (mut sess, tc, rx) = make_session_and_context_with_rx().await;
    attach_thread_persistence(
        Arc::get_mut(&mut sess).expect("session should not have additional references"),
    )
    .await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    let turn_1 = vec![user_message("turn 1 user")];
    let mut full_history = Vec::new();
    full_history.extend(initial_context.clone());
    full_history.extend(turn_1);
    sess.replace_history(full_history.clone(), Some(tc.to_turn_context_item()))
        .await;
    let rollout_items: Vec<RolloutItem> = full_history
        .into_iter()
        .map(RolloutItem::ResponseItem)
        .collect();
    sess.persist_rollout_items(&rollout_items).await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 99).await;

    let rollback_event = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(rollback_event.num_turns, 99);

    let history = sess.clone_history().await;
    assert_eq!(initial_context, history.raw_items());
}

#[tokio::test]
async fn thread_rollback_fails_without_persisted_thread_history() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    sess.record_into_history(&initial_context, tc.as_ref())
        .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 1).await;

    let error_event = wait_for_thread_rollback_failed(&rx).await;
    assert_eq!(
        error_event.message,
        "thread rollback requires persisted thread history"
    );
    assert_eq!(
        error_event.codex_error_info,
        Some(CodexErrorInfo::ThreadRollbackFailed)
    );
    assert_eq!(sess.clone_history().await.raw_items(), initial_context);
}

#[tokio::test]
async fn thread_rollback_recomputes_previous_turn_settings_and_reference_context_from_replay() {
    let (mut sess, tc, rx) = make_session_and_context_with_rx().await;
    attach_thread_persistence(
        Arc::get_mut(&mut sess).expect("session should not have additional references"),
    )
    .await;

    let first_context_item = tc.to_turn_context_item();
    let first_turn_id = first_context_item
        .turn_id
        .clone()
        .expect("turn context should have turn_id");
    let mut rolled_back_context_item = first_context_item.clone();
    rolled_back_context_item.turn_id = Some("rolled-back-turn".to_string());
    rolled_back_context_item.model = "rolled-back-model".to_string();
    let rolled_back_turn_id = rolled_back_context_item
        .turn_id
        .clone()
        .expect("turn context should have turn_id");
    let turn_one_user = user_message("turn 1 user");
    let turn_one_assistant = assistant_message("turn 1 assistant");
    let turn_two_user = user_message("turn 2 user");
    let turn_two_assistant = assistant_message("turn 2 assistant");

    sess.persist_rollout_items(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: first_turn_id.clone(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(
            codex_protocol::protocol::UserMessageEvent {
                message: "turn 1 user".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        )),
        RolloutItem::TurnContext(first_context_item.clone()),
        RolloutItem::ResponseItem(turn_one_user.clone()),
        RolloutItem::ResponseItem(turn_one_assistant.clone()),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: first_turn_id,
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: rolled_back_turn_id.clone(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(
            codex_protocol::protocol::UserMessageEvent {
                message: "turn 2 user".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        )),
        RolloutItem::TurnContext(rolled_back_context_item),
        RolloutItem::ResponseItem(turn_two_user),
        RolloutItem::ResponseItem(turn_two_assistant),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: rolled_back_turn_id,
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ])
    .await;
    sess.replace_history(
        vec![assistant_message("stale history")],
        Some(first_context_item.clone()),
    )
    .await;
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: "stale-model".to_string(),
        realtime_active: None,
    }))
    .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 1).await;
    let rollback_event = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(rollback_event.num_turns, 1);

    assert_eq!(
        sess.clone_history().await.raw_items(),
        vec![turn_one_user, turn_one_assistant]
    );
    assert_eq!(
        sess.previous_turn_settings().await,
        Some(PreviousTurnSettings {
            model: tc.model_info.slug.clone(),
            realtime_active: Some(tc.realtime_active),
        })
    );
    assert_eq!(
        serde_json::to_value(sess.reference_context_item().await)
            .expect("serialize replay reference context item"),
        serde_json::to_value(Some(first_context_item))
            .expect("serialize expected reference context item")
    );
}

#[tokio::test]
async fn thread_rollback_restores_cleared_reference_context_item_after_compaction() {
    let (mut sess, tc, rx) = make_session_and_context_with_rx().await;
    attach_thread_persistence(
        Arc::get_mut(&mut sess).expect("session should not have additional references"),
    )
    .await;

    let first_context_item = tc.to_turn_context_item();
    let first_turn_id = first_context_item
        .turn_id
        .clone()
        .expect("turn context should have turn_id");
    let compact_turn_id = "compact-turn".to_string();
    let rolled_back_turn_id = "rolled-back-turn".to_string();
    let compacted_history = vec![
        user_message("turn 1 user"),
        user_message("summary after compaction"),
    ];

    sess.persist_rollout_items(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: first_turn_id.clone(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 1 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(first_context_item.clone()),
        RolloutItem::ResponseItem(user_message("turn 1 user")),
        RolloutItem::ResponseItem(assistant_message("turn 1 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: first_turn_id,
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: compact_turn_id.clone(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::Compacted(CompactedItem {
            message: "summary after compaction".to_string(),
            replacement_history: Some(compacted_history.clone()),
        }),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: compact_turn_id,
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: rolled_back_turn_id.clone(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 2 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(TurnContextItem {
            turn_id: Some(rolled_back_turn_id.clone()),
            model: "rolled-back-model".to_string(),
            ..first_context_item.clone()
        }),
        RolloutItem::ResponseItem(user_message("turn 2 user")),
        RolloutItem::ResponseItem(assistant_message("turn 2 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: rolled_back_turn_id,
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ])
    .await;
    sess.replace_history(
        vec![assistant_message("stale history")],
        Some(first_context_item),
    )
    .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 1).await;
    let rollback_event = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(rollback_event.num_turns, 1);

    assert_eq!(sess.clone_history().await.raw_items(), compacted_history);
    assert!(sess.reference_context_item().await.is_none());
}

#[tokio::test]
async fn thread_rollback_persists_marker_and_replays_cumulatively() {
    let (mut sess, tc, rx) = make_session_and_context_with_rx().await;
    let rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut sess).expect("session should not have additional references"),
    )
    .await;
    let turn_context_item = tc.to_turn_context_item();

    sess.persist_rollout_items(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: "turn-1".to_string(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 1 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(turn_context_item.clone()),
        RolloutItem::ResponseItem(user_message("turn 1 user")),
        RolloutItem::ResponseItem(assistant_message("turn 1 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: "turn-2".to_string(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 2 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(turn_context_item.clone()),
        RolloutItem::ResponseItem(user_message("turn 2 user")),
        RolloutItem::ResponseItem(assistant_message("turn 2 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-2".to_string(),
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnStarted(
            codex_protocol::protocol::TurnStartedEvent {
                turn_id: "turn-3".to_string(),
                started_at: None,
                model_context_window: Some(128_000),
                collaboration_mode_kind: ModeKind::Default,
            },
        )),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "turn 3 user".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        })),
        RolloutItem::TurnContext(turn_context_item),
        RolloutItem::ResponseItem(user_message("turn 3 user")),
        RolloutItem::ResponseItem(assistant_message("turn 3 assistant")),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-3".to_string(),
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ])
    .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 1).await;
    let first_rollback = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(first_rollback.num_turns, 1);
    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 1).await;
    let second_rollback = wait_for_thread_rolled_back(&rx).await;
    assert_eq!(second_rollback.num_turns, 1);

    assert_eq!(
        sess.clone_history().await.raw_items(),
        vec![
            user_message("turn 1 user"),
            assistant_message("turn 1 assistant")
        ]
    );

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let rollback_markers = resumed
        .history
        .iter()
        .filter(|item| matches!(item, RolloutItem::EventMsg(EventMsg::ThreadRolledBack(_))))
        .count();
    assert_eq!(rollback_markers, 2);
}

#[tokio::test]
async fn thread_rollback_fails_when_turn_in_progress() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    sess.record_into_history(&initial_context, tc.as_ref())
        .await;

    *sess.active_turn.lock().await = Some(crate::state::ActiveTurn::default());
    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 1).await;

    let error_event = wait_for_thread_rollback_failed(&rx).await;
    assert_eq!(
        error_event.codex_error_info,
        Some(CodexErrorInfo::ThreadRollbackFailed)
    );

    let history = sess.clone_history().await;
    assert_eq!(initial_context, history.raw_items());
}

#[tokio::test]
async fn thread_rollback_fails_when_num_turns_is_zero() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;

    let initial_context = sess.build_initial_context(tc.as_ref()).await;
    sess.record_into_history(&initial_context, tc.as_ref())
        .await;

    handlers::thread_rollback(&sess, "sub-1".to_string(), /*num_turns*/ 0).await;

    let error_event = wait_for_thread_rollback_failed(&rx).await;
    assert_eq!(error_event.message, "num_turns must be >= 1");
    assert_eq!(
        error_event.codex_error_info,
        Some(CodexErrorInfo::ThreadRollbackFailed)
    );

    let history = sess.clone_history().await;
    assert_eq!(initial_context, history.raw_items());
}

#[tokio::test]
async fn set_rate_limits_retains_previous_credits() {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let model = get_model_offline_for_tests(config.model.as_deref());
    let model_info =
        construct_model_info_offline_for_tests(model.as_str(), &config.to_models_manager_config());
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        permission_profile_state: config.permissions.permission_profile_state().clone(),
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        workspace_roots: config.workspace_roots.clone(),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        environments: Vec::new(),
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        app_server_client_version: None,
        session_source: SessionSource::Exec,
        thread_source: None,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };

    let mut state = SessionState::new(session_configuration);
    let initial = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 10.0,
            window_minutes: Some(15),
            resets_at: Some(1_700),
        }),
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("10.00".to_string()),
        }),
        plan_type: Some(codex_protocol::account::PlanType::Plus),
        rate_limit_reached_type: None,
    };
    state.set_rate_limits(initial.clone());

    let update = RateLimitSnapshot {
        limit_id: Some("codex_other".to_string()),
        limit_name: Some("codex_other".to_string()),
        primary: Some(RateLimitWindow {
            used_percent: 40.0,
            window_minutes: Some(30),
            resets_at: Some(1_800),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 5.0,
            window_minutes: Some(60),
            resets_at: Some(1_900),
        }),
        credits: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    state.set_rate_limits(update.clone());

    assert_eq!(
        state.latest_rate_limits,
        Some(RateLimitSnapshot {
            limit_id: Some("codex_other".to_string()),
            limit_name: Some("codex_other".to_string()),
            primary: update.primary.clone(),
            secondary: update.secondary,
            credits: initial.credits,
            plan_type: initial.plan_type,
            rate_limit_reached_type: None,
        })
    );
}

#[tokio::test]
async fn set_rate_limits_updates_plan_type_when_present() {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let model = get_model_offline_for_tests(config.model.as_deref());
    let model_info =
        construct_model_info_offline_for_tests(model.as_str(), &config.to_models_manager_config());
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        permission_profile_state: config.permissions.permission_profile_state().clone(),
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        workspace_roots: config.workspace_roots.clone(),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        environments: Vec::new(),
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        app_server_client_version: None,
        session_source: SessionSource::Exec,
        thread_source: None,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };

    let mut state = SessionState::new(session_configuration);
    let initial = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 15.0,
            window_minutes: Some(20),
            resets_at: Some(1_600),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 5.0,
            window_minutes: Some(45),
            resets_at: Some(1_650),
        }),
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("15.00".to_string()),
        }),
        plan_type: Some(codex_protocol::account::PlanType::Plus),
        rate_limit_reached_type: None,
    };
    state.set_rate_limits(initial.clone());

    let update = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 35.0,
            window_minutes: Some(25),
            resets_at: Some(1_700),
        }),
        secondary: None,
        credits: None,
        plan_type: Some(codex_protocol::account::PlanType::Pro),
        rate_limit_reached_type: None,
    };
    state.set_rate_limits(update.clone());

    assert_eq!(
        state.latest_rate_limits,
        Some(RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            primary: update.primary,
            secondary: update.secondary,
            credits: initial.credits,
            plan_type: update.plan_type,
            rate_limit_reached_type: None,
        })
    );
}

#[test]
fn prefers_structured_content_when_present() {
    let ctr = McpCallToolResult {
        // Content present but should be ignored because structured_content is set.
        content: vec![text_block("ignored")],
        is_error: None,
        structured_content: Some(json!({
            "ok": true,
            "value": 42
        })),
        meta: None,
    };

    let got = ctr.into_function_call_output_payload();
    let expected = FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(
            serde_json::to_string(&json!({
                "ok": true,
                "value": 42
            }))
            .unwrap(),
        ),
        success: Some(true),
    };

    assert_eq!(expected, got);
}

#[tokio::test]
async fn includes_timed_out_message() {
    let exec = ExecToolCallOutput {
        exit_code: 0,
        stdout: StreamOutput::new(String::new()),
        stderr: StreamOutput::new(String::new()),
        aggregated_output: StreamOutput::new("Command output".to_string()),
        duration: StdDuration::from_secs(1),
        timed_out: true,
    };
    let (_, turn_context) = make_session_and_context().await;

    let out = format_exec_output_str(&exec, turn_context.truncation_policy);

    assert_eq!(
        out,
        "command timed out after 1000 milliseconds\nCommand output"
    );
}

#[tokio::test]
async fn turn_context_with_model_updates_model_fields() {
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.reasoning_effort = Some(ReasoningEffortConfig::Minimal);
    let updated = turn_context
        .with_model("gpt-5.4".to_string(), &session.services.models_manager)
        .await;
    let expected_model_info = session
        .services
        .models_manager
        .get_model_info(
            "gpt-5.4",
            &updated.config.as_ref().to_models_manager_config(),
        )
        .await;

    assert_eq!(updated.config.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(updated.collaboration_mode.model(), "gpt-5.4");
    assert_eq!(updated.model_info, expected_model_info);
    assert_eq!(
        updated.reasoning_effort,
        Some(ReasoningEffortConfig::Medium)
    );
    assert_eq!(
        updated.collaboration_mode.reasoning_effort(),
        Some(ReasoningEffortConfig::Medium)
    );
    assert_eq!(
        updated.config.model_reasoning_effort,
        Some(ReasoningEffortConfig::Medium)
    );
    assert_eq!(
        updated.truncation_policy,
        expected_model_info.truncation_policy.into()
    );
}

#[test]
fn falls_back_to_content_when_structured_is_null() {
    let ctr = McpCallToolResult {
        content: vec![text_block("hello"), text_block("world")],
        is_error: None,
        structured_content: Some(serde_json::Value::Null),
        meta: None,
    };

    let got = ctr.into_function_call_output_payload();
    let expected = FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(
            serde_json::to_string(&vec![text_block("hello"), text_block("world")]).unwrap(),
        ),
        success: Some(true),
    };

    assert_eq!(expected, got);
}

#[test]
fn success_flag_reflects_is_error_true() {
    let ctr = McpCallToolResult {
        content: vec![text_block("unused")],
        is_error: Some(true),
        structured_content: Some(json!({ "message": "bad" })),
        meta: None,
    };

    let got = ctr.into_function_call_output_payload();
    let expected = FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(
            serde_json::to_string(&json!({ "message": "bad" })).unwrap(),
        ),
        success: Some(false),
    };

    assert_eq!(expected, got);
}

#[test]
fn success_flag_true_with_no_error_and_content_used() {
    let ctr = McpCallToolResult {
        content: vec![text_block("alpha")],
        is_error: Some(false),
        structured_content: None,
        meta: None,
    };

    let got = ctr.into_function_call_output_payload();
    let expected = FunctionCallOutputPayload {
        body: FunctionCallOutputBody::Text(
            serde_json::to_string(&vec![text_block("alpha")]).unwrap(),
        ),
        success: Some(true),
    };

    assert_eq!(expected, got);
}

async fn wait_for_thread_rolled_back(rx: &async_channel::Receiver<Event>) -> ThreadRolledBackEvent {
    let deadline = StdDuration::from_secs(10);
    let start = std::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        match evt.msg {
            EventMsg::ThreadRolledBack(payload) => return payload,
            EventMsg::Error(payload)
                if payload.codex_error_info == Some(CodexErrorInfo::ThreadRollbackFailed) =>
            {
                panic!("rollback failed while waiting for ThreadRolledBack: {payload:?}");
            }
            _ => continue,
        }
    }
}

async fn wait_for_thread_rollback_failed(rx: &async_channel::Receiver<Event>) -> ErrorEvent {
    let deadline = StdDuration::from_secs(10);
    let start = std::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        match evt.msg {
            EventMsg::Error(payload)
                if payload.codex_error_info == Some(CodexErrorInfo::ThreadRollbackFailed) =>
            {
                return payload;
            }
            _ => continue,
        }
    }
}

async fn attach_thread_persistence(session: &mut Session) -> PathBuf {
    let config = session.get_config().await;
    let live_thread = LiveThread::create(
        Arc::clone(&session.services.thread_store),
        CreateThreadParams {
            thread_id: session.conversation_id,
            forked_from_id: None,
            source: SessionSource::Exec,
            thread_source: None,
            base_instructions: BaseInstructions::default(),
            dynamic_tools: Vec::new(),
            metadata: ThreadPersistenceMetadata {
                cwd: Some(config.cwd.to_path_buf()),
                model_provider: config.model_provider_id.clone(),
                memory_mode: if config.memories.generate_memories {
                    ThreadMemoryMode::Enabled
                } else {
                    ThreadMemoryMode::Disabled
                },
            },
            event_persistence_mode: ThreadEventPersistenceMode::Limited,
        },
    )
    .await
    .expect("create thread persistence");
    session.services.live_thread = Some(live_thread);
    session.ensure_rollout_materialized().await;
    session
        .flush_rollout()
        .await
        .expect("attached rollout should flush");
    session
        .current_rollout_path()
        .await
        .expect("load rollout path")
        .expect("thread should have rollout path")
}

struct RawOutputFailingThreadStore {
    inner: Arc<dyn codex_thread_store::ThreadStore>,
    fail_call_id: String,
    fail_next_append: AtomicBool,
}

#[async_trait::async_trait]
impl codex_thread_store::ThreadStore for RawOutputFailingThreadStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self.inner.as_any()
    }

    async fn create_thread(&self, params: CreateThreadParams) -> ThreadStoreResult<()> {
        self.inner.create_thread(params).await
    }

    async fn resume_thread(&self, params: ResumeThreadParams) -> ThreadStoreResult<()> {
        self.inner.resume_thread(params).await
    }

    async fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreResult<()> {
        let should_fail = self.fail_next_append.load(Ordering::SeqCst)
            && params.items.iter().any(|item| {
                matches!(
                    item,
                    RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, .. })
                        if call_id == &self.fail_call_id
                )
            });
        if should_fail && self.fail_next_append.swap(false, Ordering::SeqCst) {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "injected raw output append failure for {}",
                    self.fail_call_id
                ),
            });
        }
        self.inner.append_items(params).await
    }

    async fn persist_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        self.inner.persist_thread(thread_id).await
    }

    async fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        self.inner.flush_thread(thread_id).await
    }

    async fn shutdown_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        self.inner.shutdown_thread(thread_id).await
    }

    async fn discard_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        self.inner.discard_thread(thread_id).await
    }

    async fn load_history(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreResult<StoredThreadHistory> {
        self.inner.load_history(params).await
    }

    async fn read_thread(&self, params: ReadThreadParams) -> ThreadStoreResult<StoredThread> {
        self.inner.read_thread(params).await
    }

    async fn read_thread_by_rollout_path(
        &self,
        params: ReadThreadByRolloutPathParams,
    ) -> ThreadStoreResult<StoredThread> {
        self.inner.read_thread_by_rollout_path(params).await
    }

    async fn list_threads(&self, params: ListThreadsParams) -> ThreadStoreResult<ThreadPage> {
        self.inner.list_threads(params).await
    }

    async fn list_turns(&self, params: ListTurnsParams) -> ThreadStoreResult<TurnPage> {
        self.inner.list_turns(params).await
    }

    async fn list_items(&self, params: ListItemsParams) -> ThreadStoreResult<ItemPage> {
        self.inner.list_items(params).await
    }

    async fn update_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreResult<StoredThread> {
        self.inner.update_thread_metadata(params).await
    }

    async fn archive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreResult<()> {
        self.inner.archive_thread(params).await
    }

    async fn unarchive_thread(
        &self,
        params: ArchiveThreadParams,
    ) -> ThreadStoreResult<StoredThread> {
        self.inner.unarchive_thread(params).await
    }
}

async fn attach_thread_persistence_with_raw_output_append_failure(
    session: &mut Session,
    fail_call_id: &str,
) -> PathBuf {
    let base_store = Arc::clone(&session.services.thread_store);
    let failing_store: Arc<dyn codex_thread_store::ThreadStore> =
        Arc::new(RawOutputFailingThreadStore {
            inner: base_store,
            fail_call_id: fail_call_id.to_string(),
            fail_next_append: AtomicBool::new(true),
        });
    session.services.thread_store = Arc::clone(&failing_store);
    let config = session.get_config().await;
    let live_thread = LiveThread::create(
        failing_store,
        CreateThreadParams {
            thread_id: session.conversation_id,
            forked_from_id: None,
            source: SessionSource::Exec,
            thread_source: None,
            base_instructions: BaseInstructions::default(),
            dynamic_tools: Vec::new(),
            metadata: ThreadPersistenceMetadata {
                cwd: Some(config.cwd.to_path_buf()),
                model_provider: config.model_provider_id.clone(),
                memory_mode: if config.memories.generate_memories {
                    ThreadMemoryMode::Enabled
                } else {
                    ThreadMemoryMode::Disabled
                },
            },
            event_persistence_mode: ThreadEventPersistenceMode::Limited,
        },
    )
    .await
    .expect("create thread persistence");
    session.services.live_thread = Some(live_thread);
    session.ensure_rollout_materialized().await;
    session
        .flush_rollout()
        .await
        .expect("attached rollout should flush");
    session
        .current_rollout_path()
        .await
        .expect("load rollout path")
        .expect("thread should have rollout path")
}

enum CompactThreadStoreFailureMode {
    Append,
    Metadata,
}

struct CompactFailingThreadStore {
    inner: Arc<dyn codex_thread_store::ThreadStore>,
    mode: CompactThreadStoreFailureMode,
    fail_next_metadata_update: AtomicBool,
}

#[async_trait::async_trait]
impl codex_thread_store::ThreadStore for CompactFailingThreadStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self.inner.as_any()
    }

    async fn create_thread(&self, params: CreateThreadParams) -> ThreadStoreResult<()> {
        self.inner.create_thread(params).await
    }

    async fn resume_thread(&self, params: ResumeThreadParams) -> ThreadStoreResult<()> {
        self.inner.resume_thread(params).await
    }

    async fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreResult<()> {
        if params
            .items
            .iter()
            .any(|item| matches!(item, RolloutItem::Compacted(_)))
        {
            match self.mode {
                CompactThreadStoreFailureMode::Append => {
                    return Err(ThreadStoreError::Internal {
                        message: "injected compact append failure".to_string(),
                    });
                }
                CompactThreadStoreFailureMode::Metadata => {
                    self.fail_next_metadata_update.store(true, Ordering::SeqCst);
                }
            }
        }
        self.inner.append_items(params).await
    }

    async fn persist_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        self.inner.persist_thread(thread_id).await
    }

    async fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        self.inner.flush_thread(thread_id).await
    }

    async fn shutdown_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        self.inner.shutdown_thread(thread_id).await
    }

    async fn discard_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        self.inner.discard_thread(thread_id).await
    }

    async fn load_history(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreResult<StoredThreadHistory> {
        self.inner.load_history(params).await
    }

    async fn read_thread(&self, params: ReadThreadParams) -> ThreadStoreResult<StoredThread> {
        self.inner.read_thread(params).await
    }

    async fn read_thread_by_rollout_path(
        &self,
        params: ReadThreadByRolloutPathParams,
    ) -> ThreadStoreResult<StoredThread> {
        self.inner.read_thread_by_rollout_path(params).await
    }

    async fn list_threads(&self, params: ListThreadsParams) -> ThreadStoreResult<ThreadPage> {
        self.inner.list_threads(params).await
    }

    async fn list_turns(&self, params: ListTurnsParams) -> ThreadStoreResult<TurnPage> {
        self.inner.list_turns(params).await
    }

    async fn list_items(&self, params: ListItemsParams) -> ThreadStoreResult<ItemPage> {
        self.inner.list_items(params).await
    }

    async fn update_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreResult<StoredThread> {
        if matches!(self.mode, CompactThreadStoreFailureMode::Metadata)
            && self.fail_next_metadata_update.swap(false, Ordering::SeqCst)
        {
            return Err(ThreadStoreError::Internal {
                message: "injected compact metadata failure".to_string(),
            });
        }
        self.inner.update_thread_metadata(params).await
    }

    async fn archive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreResult<()> {
        self.inner.archive_thread(params).await
    }

    async fn unarchive_thread(
        &self,
        params: ArchiveThreadParams,
    ) -> ThreadStoreResult<StoredThread> {
        self.inner.unarchive_thread(params).await
    }
}

async fn attach_thread_persistence_with_compact_append_failure(session: &mut Session) -> PathBuf {
    attach_thread_persistence_with_compact_failure(session, CompactThreadStoreFailureMode::Append)
        .await
}

async fn attach_thread_persistence_with_compact_metadata_failure(session: &mut Session) -> PathBuf {
    attach_thread_persistence_with_compact_failure(session, CompactThreadStoreFailureMode::Metadata)
        .await
}

async fn attach_thread_persistence_with_compact_failure(
    session: &mut Session,
    mode: CompactThreadStoreFailureMode,
) -> PathBuf {
    let base_store = Arc::clone(&session.services.thread_store);
    let failing_store: Arc<dyn codex_thread_store::ThreadStore> =
        Arc::new(CompactFailingThreadStore {
            inner: base_store,
            mode,
            fail_next_metadata_update: AtomicBool::new(false),
        });
    session.services.thread_store = Arc::clone(&failing_store);
    let config = session.get_config().await;
    let live_thread = LiveThread::create(
        failing_store,
        CreateThreadParams {
            thread_id: session.conversation_id,
            forked_from_id: None,
            source: SessionSource::Exec,
            thread_source: None,
            base_instructions: BaseInstructions::default(),
            dynamic_tools: Vec::new(),
            metadata: ThreadPersistenceMetadata {
                cwd: Some(config.cwd.to_path_buf()),
                model_provider: config.model_provider_id.clone(),
                memory_mode: if config.memories.generate_memories {
                    ThreadMemoryMode::Enabled
                } else {
                    ThreadMemoryMode::Disabled
                },
            },
            event_persistence_mode: ThreadEventPersistenceMode::Limited,
        },
    )
    .await
    .expect("create thread persistence");
    session.services.live_thread = Some(live_thread);
    session.ensure_rollout_materialized().await;
    session
        .flush_rollout()
        .await
        .expect("attached rollout should flush");
    session
        .current_rollout_path()
        .await
        .expect("load rollout path")
        .expect("thread should have rollout path")
}

fn text_block(s: &str) -> serde_json::Value {
    json!({
        "type": "text",
        "text": s,
    })
}

async fn build_test_config(codex_home: &Path) -> Config {
    ConfigBuilder::without_managed_config_for_tests()
        .codex_home(codex_home.to_path_buf())
        .build()
        .await
        .expect("load default test config")
}

fn session_telemetry(
    conversation_id: ThreadId,
    config: &Config,
    model_info: &ModelInfo,
    session_source: SessionSource,
) -> SessionTelemetry {
    SessionTelemetry::new(
        conversation_id,
        get_model_offline_for_tests(config.model.as_deref()).as_str(),
        model_info.slug.as_str(),
        /*account_id*/ None,
        Some("test@test.com".to_string()),
        Some(TelemetryAuthMode::Chatgpt),
        "test_originator".to_string(),
        /*log_user_prompts*/ false,
        "test".to_string(),
        session_source,
    )
}

#[test]
fn get_service_tier_defaults_enterprise_accounts_to_fast() {
    assert_eq!(
        get_service_tier(
            /*configured_service_tier*/ None,
            /*fast_default_opt_out*/ false,
            Some(AccountPlanType::Enterprise),
            /*fast_mode_enabled*/ true,
        ),
        Some(ServiceTier::Fast.request_value().to_string())
    );
    assert_eq!(
        get_service_tier(
            /*configured_service_tier*/ None,
            /*fast_default_opt_out*/ false,
            Some(AccountPlanType::EnterpriseCbpUsageBased),
            /*fast_mode_enabled*/ true,
        ),
        Some(ServiceTier::Fast.request_value().to_string())
    );
    assert_eq!(
        get_service_tier(
            /*configured_service_tier*/ None,
            /*fast_default_opt_out*/ false,
            Some(AccountPlanType::Business),
            /*fast_mode_enabled*/ true,
        ),
        Some(ServiceTier::Fast.request_value().to_string())
    );
    assert_eq!(
        get_service_tier(
            /*configured_service_tier*/ None,
            /*fast_default_opt_out*/ false,
            Some(AccountPlanType::Team),
            /*fast_mode_enabled*/ true,
        ),
        Some(ServiceTier::Fast.request_value().to_string())
    );
    assert_eq!(
        get_service_tier(
            /*configured_service_tier*/ None,
            /*fast_default_opt_out*/ false,
            Some(AccountPlanType::SelfServeBusinessUsageBased),
            /*fast_mode_enabled*/ true,
        ),
        Some(ServiceTier::Fast.request_value().to_string())
    );
}

#[test]
fn get_service_tier_respects_fast_default_opt_out() {
    assert_eq!(
        get_service_tier(
            /*configured_service_tier*/ None,
            /*fast_default_opt_out*/ true,
            Some(AccountPlanType::Enterprise),
            /*fast_mode_enabled*/ true,
        ),
        None
    );
}

#[test]
fn get_service_tier_does_not_default_non_enterprise_or_disabled_fast_mode() {
    assert_eq!(
        get_service_tier(
            /*configured_service_tier*/ None,
            /*fast_default_opt_out*/ false,
            Some(AccountPlanType::Pro),
            /*fast_mode_enabled*/ true,
        ),
        None
    );
    assert_eq!(
        get_service_tier(
            /*configured_service_tier*/ None,
            /*fast_default_opt_out*/ false,
            Some(AccountPlanType::Enterprise),
            /*fast_mode_enabled*/ false,
        ),
        None
    );
}

#[tokio::test]
async fn session_settings_null_service_tier_update_clears_service_tier() {
    let session_configuration = make_session_configuration_for_tests().await;

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            service_tier: Some(None),
            ..Default::default()
        })
        .expect("null service tier update should apply");

    assert_eq!(updated.service_tier, None);
}

#[tokio::test]
async fn session_settings_legacy_fast_service_tier_update_uses_priority_request_value() {
    let session_configuration = make_session_configuration_for_tests().await;

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            service_tier: Some(Some("fast".to_string())),
            ..Default::default()
        })
        .expect("legacy fast service tier update should apply");

    assert_eq!(
        updated.service_tier,
        Some(ServiceTier::Fast.request_value().to_string())
    );
}

pub(crate) async fn make_session_configuration_for_tests() -> SessionConfiguration {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let model = get_model_offline_for_tests(config.model.as_deref());
    let model_info =
        construct_model_info_offline_for_tests(model.as_str(), &config.to_models_manager_config());
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };

    SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        permission_profile_state: config.permissions.permission_profile_state().clone(),
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        workspace_roots: config.workspace_roots.clone(),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        environments: Vec::new(),
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        app_server_client_version: None,
        session_source: SessionSource::Exec,
        thread_source: None,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    }
}

fn turn_environments_for_tests(
    environment: &Arc<codex_exec_server::Environment>,
    cwd: &codex_utils_absolute_path::AbsolutePathBuf,
) -> crate::environment_selection::ResolvedTurnEnvironments {
    crate::environment_selection::ResolvedTurnEnvironments {
        turn_environments: vec![TurnEnvironment {
            environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
            environment: Arc::clone(environment),
            cwd: cwd.clone(),
            shell: None,
        }],
    }
}

#[tokio::test]
async fn session_configuration_apply_preserves_profile_file_system_policy_on_cwd_only_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let workspace = tempfile::tempdir().expect("create temp dir");
    let project_root = workspace.path().join("project");
    let original_cwd = project_root.join("subdir");
    let docs_dir = original_cwd.join("docs");
    std::fs::create_dir_all(&docs_dir).expect("create docs dir");
    let docs_dir = docs_dir.abs();

    session_configuration.cwd = original_cwd.abs();
    let sandbox_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: Vec::new(),
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
            },
            access: FileSystemAccessMode::Write,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: docs_dir },
            access: FileSystemAccessMode::Read,
        },
    ]);
    let network_sandbox_policy = NetworkSandboxPolicy::from(&sandbox_policy);
    session_configuration
        .set_permission_profile_for_tests(
            PermissionProfile::from_runtime_permissions_with_enforcement(
                SandboxEnforcement::from_legacy_sandbox_policy(&sandbox_policy),
                &file_system_sandbox_policy,
                network_sandbox_policy,
            ),
        )
        .expect("set permission profile");
    let expected_file_system_sandbox_policy = file_system_sandbox_policy
        .materialize_project_roots_with_workspace_roots(&session_configuration.workspace_roots);

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(project_root),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(
        updated.file_system_sandbox_policy(),
        expected_file_system_sandbox_policy
    );
}

#[tokio::test]
async fn session_configuration_apply_permission_profile_preserves_existing_deny_read_entries() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let cwd = tempfile::tempdir().expect("create temp dir");
    session_configuration.cwd = cwd.path().abs();

    let workspace_policy = SandboxPolicy::new_workspace_write_policy();
    let deny_entry = FileSystemSandboxEntry {
        path: FileSystemPath::GlobPattern {
            pattern: "**/*.env".to_string(),
        },
        access: FileSystemAccessMode::None,
    };
    let mut existing_file_system_policy =
        FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
            &workspace_policy,
            session_configuration.cwd.as_path(),
        );
    existing_file_system_policy.glob_scan_max_depth = Some(2);
    existing_file_system_policy.entries.push(deny_entry.clone());
    session_configuration
        .set_permission_profile_for_tests(
            PermissionProfile::from_runtime_permissions_with_enforcement(
                SandboxEnforcement::from_legacy_sandbox_policy(&workspace_policy),
                &existing_file_system_policy,
                NetworkSandboxPolicy::Restricted,
            ),
        )
        .expect("set permission profile");

    let requested_file_system_policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &workspace_policy,
        session_configuration.cwd.as_path(),
    );
    let permission_profile = codex_protocol::models::PermissionProfile::from_runtime_permissions(
        &requested_file_system_policy,
        NetworkSandboxPolicy::Restricted,
    );
    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            permission_profile: Some(permission_profile),
            ..Default::default()
        })
        .expect("permission profile update should succeed");

    let mut expected_file_system_policy = requested_file_system_policy
        .materialize_project_roots_with_workspace_roots(&session_configuration.workspace_roots);
    expected_file_system_policy.glob_scan_max_depth = Some(2);
    expected_file_system_policy.entries.push(deny_entry);
    assert_eq!(
        updated.file_system_sandbox_policy(),
        expected_file_system_policy
    );
}

#[tokio::test]
async fn session_configuration_apply_permission_profile_accepts_direct_write_roots() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let cwd = tempfile::tempdir().expect("create cwd");
    session_configuration.cwd = cwd.path().abs();
    let external_write_dir = tempfile::tempdir().expect("create external write root");
    let external_write_path = AbsolutePathBuf::from_absolute_path(
        codex_utils_absolute_path::canonicalize_preserving_symlinks(external_write_dir.path())
            .expect("canonical temp dir"),
    )
    .expect("canonical temp dir should be absolute");
    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: external_write_path.clone(),
            },
            access: FileSystemAccessMode::Write,
        }]);
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        NetworkSandboxPolicy::Restricted,
    );

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            permission_profile: Some(permission_profile.clone()),
            ..Default::default()
        })
        .expect("permission profile update should accept direct runtime permissions");

    assert_eq!(updated.permission_profile(), permission_profile);
    assert_eq!(
        updated.file_system_sandbox_policy(),
        file_system_sandbox_policy
    );
    assert_eq!(
        updated.sandbox_policy(),
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![external_write_path],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        }
    );
}

#[tokio::test]
async fn session_configuration_apply_rebinds_symbolic_profile_to_updated_workspace_roots() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let old_root = tempfile::tempdir().expect("create old root");
    let new_root = tempfile::tempdir().expect("create new root");
    let profile_root = tempfile::tempdir().expect("create profile root");
    let old_root = old_root.path().abs();
    let new_root = new_root.path().abs();
    let profile_root = profile_root.path().abs();
    session_configuration.workspace_roots = vec![old_root.clone()];

    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
            },
            access: FileSystemAccessMode::Write,
        }]);
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        NetworkSandboxPolicy::Restricted,
    );

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            workspace_roots: Some(vec![new_root.clone()]),
            permission_profile: Some(permission_profile),
            active_permission_profile: Some(ActivePermissionProfile::new("dev")),
            profile_workspace_roots: Some(vec![profile_root.clone()]),
            ..Default::default()
        })
        .expect("permission profile update should succeed");

    let updated_policy = updated.file_system_sandbox_policy();
    assert!(updated_policy.can_write_path_with_cwd(new_root.as_path(), updated.cwd.as_path()));
    assert!(!updated_policy.can_write_path_with_cwd(old_root.as_path(), updated.cwd.as_path()));
    assert_eq!(
        updated.active_permission_profile(),
        Some(ActivePermissionProfile::new("dev"))
    );
    assert_eq!(updated.profile_workspace_roots(), &[profile_root]);
}

#[tokio::test]
async fn session_configuration_apply_retargets_implicit_workspace_root_on_cwd_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let old_root = tempfile::tempdir().expect("create old root");
    let new_root = tempfile::tempdir().expect("create new root");
    let extra_root = tempfile::tempdir().expect("create extra root");
    let old_root = old_root.path().abs();
    let new_root = new_root.path().abs();
    let extra_root = extra_root.path().abs();
    session_configuration.cwd = old_root.clone();
    session_configuration.workspace_roots = vec![old_root.clone(), extra_root.clone()];

    let file_system_sandbox_policy =
        FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
            },
            access: FileSystemAccessMode::Write,
        }]);
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        NetworkSandboxPolicy::Restricted,
    );
    session_configuration
        .set_permission_profile_for_tests(permission_profile)
        .expect("set permission profile");

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(new_root.to_path_buf()),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(
        updated.workspace_roots,
        vec![new_root.clone(), extra_root.clone()]
    );
    let updated_policy = updated.file_system_sandbox_policy();
    assert!(updated_policy.can_write_path_with_cwd(new_root.as_path(), updated.cwd.as_path()));
    assert!(updated_policy.can_write_path_with_cwd(extra_root.as_path(), updated.cwd.as_path()));
    assert!(!updated_policy.can_write_path_with_cwd(old_root.as_path(), updated.cwd.as_path()));
}

#[cfg_attr(windows, ignore)]
#[tokio::test]
async fn new_default_turn_uses_config_aware_skills_for_role_overrides() {
    let (session, _turn_context) = make_session_and_context().await;
    let parent_config = session.get_config().await;
    let codex_home = parent_config.codex_home.clone();
    let skill_dir = codex_home.join("skills").join("demo");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    let skill_path = skill_dir.join("SKILL.md");
    std::fs::write(
        &skill_path,
        "---\nname: demo-skill\ndescription: demo description\n---\n\n# Body\n",
    )
    .expect("write skill");

    let skill_fs = session
        .services
        .environment_manager
        .default_environment()
        .map(|environment| environment.get_filesystem())
        .unwrap_or_else(|| std::sync::Arc::clone(&codex_exec_server::LOCAL_FS));
    let parent_outcome = session
        .services
        .skills_manager
        .skills_for_cwd(
            &crate::skills_load_input_from_config(&parent_config, Vec::new()),
            /*force_reload*/ true,
            Some(Arc::clone(&skill_fs)),
        )
        .await;
    let parent_skill = parent_outcome
        .skills
        .iter()
        .find(|skill| skill.name == "demo-skill")
        .expect("demo skill should be discovered");
    assert_eq!(parent_outcome.is_skill_enabled(parent_skill), true);

    let role_path = codex_home.join("skills-role.toml");
    std::fs::write(
        &role_path,
        format!(
            r#"developer_instructions = "Stay focused"

[[skills.config]]
path = "{}"
enabled = false
"#,
            skill_path.display()
        ),
    )
    .expect("write role config");

    let mut child_config = (*parent_config).clone();
    child_config.agent_roles.insert(
        "custom".to_string(),
        crate::config::AgentRoleConfig {
            description: None,
            config_file: Some(role_path.to_path_buf()),
            nickname_candidates: None,
        },
    );
    crate::agent::role::apply_role_to_config(&mut child_config, Some("custom"))
        .await
        .expect("custom role should apply");

    {
        let mut state = session.state.lock().await;
        state.session_configuration.original_config_do_not_use = Arc::new(child_config);
    }

    let child_turn = session
        .new_default_turn_with_sub_id("role-skill-turn".to_string())
        .await;
    let child_skill = child_turn
        .turn_skills
        .outcome
        .skills
        .iter()
        .find(|skill| skill.name == "demo-skill")
        .expect("demo skill should be discovered");
    assert_eq!(
        child_turn.turn_skills.outcome.is_skill_enabled(child_skill),
        false
    );
}

#[tokio::test]
async fn session_configuration_apply_retargets_legacy_workspace_root_on_cwd_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let workspace = tempfile::tempdir().expect("create temp dir");
    let original_cwd = workspace.path().join("repo-a").abs();
    let project_root = workspace.path().join("repo-b").abs();
    session_configuration.cwd = original_cwd.clone();
    session_configuration.workspace_roots = vec![session_configuration.cwd.clone()];
    let sandbox_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: Vec::new(),
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    let file_system_sandbox_policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &sandbox_policy,
        &session_configuration.cwd,
    );
    session_configuration
        .set_permission_profile_for_tests(
            PermissionProfile::from_runtime_permissions_with_enforcement(
                SandboxEnforcement::from_legacy_sandbox_policy(&sandbox_policy),
                &file_system_sandbox_policy,
                NetworkSandboxPolicy::from(&sandbox_policy),
            ),
        )
        .expect("set permission profile");

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(project_root.to_path_buf()),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(updated.workspace_roots, vec![project_root.clone()]);
    assert!(
        updated
            .file_system_sandbox_policy()
            .can_write_path_with_cwd(project_root.as_path(), updated.cwd.as_path()),
        "cwd-only update should keep the new cwd writable"
    );
    assert!(
        !updated
            .file_system_sandbox_policy()
            .can_write_path_with_cwd(original_cwd.as_path(), updated.cwd.as_path()),
        "cwd-only update should not keep the old implicit cwd writable"
    );
}

#[tokio::test]
async fn session_configuration_apply_preserves_absolute_cwd_write_root_on_cwd_update() {
    let mut session_configuration = make_session_configuration_for_tests().await;
    let workspace = tempfile::tempdir().expect("create temp dir");
    let original_cwd = workspace.path().join("repo-a");
    let next_cwd = workspace.path().join("repo-b");
    std::fs::create_dir_all(&original_cwd).expect("create original cwd");
    std::fs::create_dir_all(&next_cwd).expect("create next cwd");
    let original_cwd = original_cwd.abs();

    session_configuration.cwd = original_cwd.clone();
    let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: original_cwd.clone(),
            },
            access: FileSystemAccessMode::Write,
        },
    ]);
    session_configuration
        .set_permission_profile_for_tests(
            PermissionProfile::from_runtime_permissions_with_enforcement(
                SandboxEnforcement::Managed,
                &file_system_sandbox_policy,
                NetworkSandboxPolicy::Restricted,
            ),
        )
        .expect("set permission profile");

    let updated = session_configuration
        .apply(&SessionSettingsUpdate {
            cwd: Some(next_cwd.clone()),
            ..Default::default()
        })
        .expect("cwd-only update should succeed");

    assert_eq!(
        updated.file_system_sandbox_policy(),
        file_system_sandbox_policy
    );
    assert!(
        updated
            .file_system_sandbox_policy()
            .can_write_path_with_cwd(original_cwd.as_path(), updated.cwd.as_path()),
        "absolute grant to the old cwd must remain writable"
    );
    assert!(
        !updated
            .file_system_sandbox_policy()
            .can_write_path_with_cwd(next_cwd.as_path(), updated.cwd.as_path()),
        "cwd-only update must not reinterpret an absolute old-cwd grant as :workspace_roots"
    );
}

#[tokio::test]
async fn session_update_settings_does_not_rewrite_sticky_environment_cwds() {
    let (session, turn_context) = make_session_and_context().await;
    #[allow(deprecated)]
    let updated_cwd = turn_context.cwd.join("project");
    std::fs::create_dir_all(updated_cwd.as_path()).expect("create project dir");

    session
        .update_settings(SessionSettingsUpdate {
            cwd: Some(PathBuf::from("project")),
            ..Default::default()
        })
        .await
        .expect("cwd update should succeed");

    let session_cwd = {
        let state = session.state.lock().await;
        state.session_configuration.cwd.clone()
    };
    let config = session.get_config().await;
    let next_turn = session.new_default_turn().await;

    assert_eq!(session_cwd, updated_cwd);
    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    #[allow(deprecated)]
    let next_turn_cwd = next_turn.cwd.clone();
    assert_eq!(config.cwd, turn_cwd);
    assert_eq!(next_turn_cwd, updated_cwd);
    assert_eq!(next_turn.config.cwd, updated_cwd);
}

#[tokio::test]
async fn relative_cwd_update_without_environments_resolves_under_session_cwd() {
    let (session, _turn_context) = make_session_and_context().await;
    let original_cwd = {
        let mut state = session.state.lock().await;
        state.session_configuration.environments = Vec::new();
        state.session_configuration.cwd.clone()
    };
    let updated_cwd = original_cwd.join("project");
    std::fs::create_dir_all(updated_cwd.as_path()).expect("create project dir");

    session
        .update_settings(SessionSettingsUpdate {
            cwd: Some(PathBuf::from("project")),
            ..Default::default()
        })
        .await
        .expect("cwd update should succeed");

    let state = session.state.lock().await;
    assert_eq!(state.session_configuration.cwd, updated_cwd);
    assert!(state.session_configuration.environments.is_empty());
}

#[tokio::test]
async fn cwd_update_does_not_rewrite_sticky_environment_cwd() {
    let (session, _turn_context) = make_session_and_context().await;
    let (original_cwd, environment_cwd) = {
        let mut state = session.state.lock().await;
        let original_cwd = state.session_configuration.cwd.clone();
        let environment_cwd = original_cwd.join("environment");
        state.session_configuration.environments = vec![TurnEnvironmentSelection {
            environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
            cwd: environment_cwd.clone(),
        }];
        (original_cwd, environment_cwd)
    };
    let updated_cwd = original_cwd.join("project");
    std::fs::create_dir_all(updated_cwd.as_path()).expect("create project dir");

    session
        .update_settings(SessionSettingsUpdate {
            cwd: Some(PathBuf::from("project")),
            ..Default::default()
        })
        .await
        .expect("cwd update should succeed");

    let state = session.state.lock().await;
    assert_eq!(state.session_configuration.cwd, updated_cwd);
    assert_eq!(
        state.session_configuration.environments[0].cwd,
        environment_cwd
    );
}

#[tokio::test]
async fn absolute_cwd_update_with_turn_environment_is_allowed() {
    let (session, _turn_context, rx) = make_session_and_context_with_rx().await;
    let absolute_cwd = {
        let state = session.state.lock().await;
        state.session_configuration.cwd.join("absolute-turn")
    };
    std::fs::create_dir_all(absolute_cwd.as_path()).expect("create absolute turn dir");

    let turn_context = session
        .new_turn_with_sub_id(
            "sub-1".to_string(),
            SessionSettingsUpdate {
                cwd: Some(absolute_cwd.to_path_buf()),
                environments: Some(vec![TurnEnvironmentSelection {
                    environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
                    cwd: absolute_cwd.clone(),
                }]),
                ..Default::default()
            },
        )
        .await
        .expect("absolute cwd with explicit environments should succeed");

    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    assert_eq!(turn_cwd, absolute_cwd);
    assert_eq!(turn_context.config.cwd, absolute_cwd);
    assert_eq!(turn_context.environments.turn_environments.len(), 1);
}

#[tokio::test]
async fn session_new_fails_when_zsh_fork_enabled_without_zsh_path() {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let mut config = build_test_config(codex_home.path()).await;
    config
        .features
        .enable(Feature::ShellZshFork)
        .expect("test config should allow shell_zsh_fork");
    config.zsh_path = None;
    let config = Arc::new(config);

    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = models_manager_with_provider(
        config.codex_home.to_path_buf(),
        auth_manager.clone(),
        config.model_provider.clone(),
    );
    let model = get_model_offline_for_tests(config.model.as_deref());
    let model_info =
        construct_model_info_offline_for_tests(model.as_str(), &config.to_models_manager_config());
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort: config.model_reasoning_effort,
            developer_instructions: None,
        },
    };
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        permission_profile_state: config.permissions.permission_profile_state().clone(),
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        workspace_roots: config.workspace_roots.clone(),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        environments: Vec::new(),
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        app_server_client_version: None,
        session_source: SessionSource::Exec,
        thread_source: None,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };

    let (tx_event, _rx_event) = async_channel::unbounded();
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.to_path_buf()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        /*bundled_skills_enabled*/ true,
    ));
    let result = Session::new(
        session_configuration,
        Arc::clone(&config),
        "11111111-1111-4111-8111-111111111111".to_string(),
        auth_manager,
        models_manager,
        Arc::new(ExecPolicyManager::default()),
        tx_event,
        agent_status_tx,
        InitialHistory::New,
        /*spine_fork_source_boundary*/ None,
        SessionSource::Exec,
        skills_manager,
        plugins_manager,
        mcp_manager,
        Arc::new(codex_extension_api::ExtensionRegistryBuilder::new().build()),
        AgentControl::default(),
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        /*analytics_events_client*/ None,
        Arc::new(codex_thread_store::LocalThreadStore::new(
            codex_thread_store::LocalThreadStoreConfig::from_config(config.as_ref()),
            /*state_db*/ None,
        )),
        codex_rollout_trace::ThreadTraceContext::disabled(),
        /*attestation_provider*/ None,
    )
    .await;

    let err = match result {
        Ok(_) => panic!("expected startup to fail"),
        Err(err) => err,
    };
    let msg = format!("{err:#}");
    assert!(msg.contains("zsh fork feature enabled, but `zsh_path` is not configured"));
}

// todo: use online model info
pub(crate) async fn make_session_and_context() -> (Session, TurnContext) {
    let (tx_event, _rx_event) = async_channel::unbounded();
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let config = build_test_config(codex_home.path()).await;
    let config = Arc::new(config);
    let thread_id = ThreadId::default();
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = models_manager_with_provider(
        config.codex_home.to_path_buf(),
        auth_manager.clone(),
        config.model_provider.clone(),
    );
    let agent_control = AgentControl::default();
    let exec_policy = Arc::new(ExecPolicyManager::default());
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let model = get_model_offline_for_tests(config.model.as_deref());
    let model_info =
        construct_model_info_offline_for_tests(model.as_str(), &config.to_models_manager_config());
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let default_environments = vec![TurnEnvironmentSelection {
        environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
        cwd: config.cwd.clone(),
    }];
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        permission_profile_state: config.permissions.permission_profile_state().clone(),
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        workspace_roots: config.workspace_roots.clone(),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        environments: default_environments,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        app_server_client_version: None,
        session_source: SessionSource::Exec,
        thread_source: None,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };
    let per_turn_config =
        Session::build_per_turn_config(&session_configuration, session_configuration.cwd.clone());
    let model_info = construct_model_info_offline_for_tests(
        session_configuration.collaboration_mode.model(),
        &per_turn_config.to_models_manager_config(),
    );
    let session_telemetry = session_telemetry(
        thread_id,
        config.as_ref(),
        &model_info,
        session_configuration.session_source.clone(),
    );

    let state = SessionState::new(session_configuration.clone());
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.to_path_buf()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        /*bundled_skills_enabled*/ true,
    ));
    let network_approval = Arc::new(NetworkApprovalService::default());
    let environment = Arc::new(
        codex_exec_server::Environment::create_for_tests(/*exec_server_url*/ None)
            .expect("create environment"),
    );

    let services = SessionServices {
        mcp_connection_manager: Arc::new(RwLock::new(
            McpConnectionManager::new_uninitialized_with_permission_profile(
                &config.permissions.approval_policy,
                config.permissions.permission_profile(),
            ),
        )),
        mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
        unified_exec_manager: UnifiedExecProcessManager::new(
            config.background_terminal_max_timeout,
        ),
        shell_zsh_path: None,
        main_execve_wrapper_exe: config.main_execve_wrapper_exe.clone(),
        analytics_events_client: AnalyticsEventsClient::new(
            Arc::clone(&auth_manager),
            config.chatgpt_base_url.trim_end_matches('/').to_string(),
            config.analytics_enabled,
        ),
        hooks: arc_swap::ArcSwap::from_pointee(Hooks::new(HooksConfig {
            legacy_notify_argv: config.notify.clone(),
            ..HooksConfig::default()
        })),
        rollout_thread_trace: codex_rollout_trace::ThreadTraceContext::disabled(),
        user_shell: Arc::new(default_user_shell()),
        shell_snapshot_tx: watch::channel(None).0,
        show_raw_agent_reasoning: config.show_raw_agent_reasoning,
        exec_policy,
        auth_manager: auth_manager.clone(),
        session_telemetry: session_telemetry.clone(),
        models_manager: Arc::clone(&models_manager),
        tool_approvals: Mutex::new(ApprovalStore::default()),
        guardian_rejections: Mutex::new(std::collections::HashMap::new()),
        guardian_rejection_circuit_breaker: Mutex::new(Default::default()),
        runtime_handle: tokio::runtime::Handle::current(),
        skills_manager,
        plugins_manager,
        mcp_manager,
        extensions: Arc::new(codex_extension_api::ExtensionRegistryBuilder::new().build()),
        session_extension_data: codex_extension_api::ExtensionData::new(
            agent_control.session_id().to_string(),
        ),
        thread_extension_data: codex_extension_api::ExtensionData::new(thread_id.to_string()),
        agent_control,
        network_proxy: None,
        network_approval: Arc::clone(&network_approval),
        state_db: None,
        live_thread: None,
        thread_store: Arc::new(codex_thread_store::LocalThreadStore::new(
            codex_thread_store::LocalThreadStoreConfig::from_config(config.as_ref()),
            /*state_db*/ None,
        )),
        attestation_provider: None,
        debug_request_capture_dir: None,
        model_client: ModelClient::new(
            Some(auth_manager.clone()),
            thread_id.into(),
            thread_id,
            /*installation_id*/ "11111111-1111-4111-8111-111111111111".to_string(),
            session_configuration.provider.clone(),
            session_configuration.session_source.clone(),
            config.model_verbosity,
            config.features.enabled(Feature::EnableRequestCompression),
            config.features.enabled(Feature::RuntimeMetrics),
            Session::build_model_client_beta_features_header(config.as_ref()),
            /*attestation_provider*/ None,
            /*debug_request_capture_dir*/ None,
        ),
        code_mode_service: crate::tools::code_mode::CodeModeService::new(),
        environment_manager: Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
    };

    let plugin_outcome = services
        .plugins_manager
        .plugins_for_config(&per_turn_config.plugins_config_input())
        .await;
    let effective_skill_roots = plugin_outcome.effective_plugin_skill_roots();
    let skills_input =
        crate::skills_load_input_from_config(&per_turn_config, effective_skill_roots);
    let skill_fs = environment.get_filesystem();
    let skills_outcome = Arc::new(
        services
            .skills_manager
            .skills_for_config(&skills_input, Some(Arc::clone(&skill_fs)))
            .await,
    );
    let turn_environments = turn_environments_for_tests(&environment, &session_configuration.cwd);
    let turn_context = Session::make_turn_context(
        thread_id,
        SessionId::from(thread_id),
        Some(Arc::clone(&auth_manager)),
        &session_telemetry,
        session_configuration.provider.clone(),
        &session_configuration,
        services.user_shell.as_ref(),
        services.shell_zsh_path.as_ref(),
        services.main_execve_wrapper_exe.as_ref(),
        per_turn_config,
        model_info,
        &models_manager,
        /*network*/ None,
        turn_environments,
        session_configuration.cwd.clone(),
        "turn_id".to_string(),
        skills_outcome,
        /*goal_tools_supported*/ true,
    );

    let (mailbox, mailbox_rx) = crate::agent::Mailbox::new();
    let session = Session {
        conversation_id: thread_id,
        installation_id: "11111111-1111-4111-8111-111111111111".to_string(),
        tx_event,
        agent_status: agent_status_tx,
        out_of_band_elicitation_paused: watch::channel(false).0,
        state: Mutex::new(state),
        managed_network_proxy_refresh_lock: Semaphore::new(/*permits*/ 1),
        features: config.features.clone(),
        pending_mcp_server_refresh_config: Mutex::new(None),
        conversation: Arc::new(RealtimeConversationManager::new()),
        active_turn: Mutex::new(None),
        mailbox,
        mailbox_rx: Mutex::new(mailbox_rx),
        idle_pending_input: Mutex::new(Vec::new()),
        goal_runtime: crate::goals::GoalRuntimeState::new(),
        guardian_review_session: crate::guardian::GuardianReviewSessionManager::default(),
        services,
        spine: (config.features.enabled(Feature::SpineJit)
            || config.features.enabled(Feature::SpineTrim))
        .then(|| {
            TokioMutex::new(SpineSessionState::new_with_features(
                config.features.enabled(Feature::SpineJit),
                config.features.enabled(Feature::SpineTrim),
            ))
        }),
        spine_pressure_prompt_state: Mutex::new(Default::default()),
        next_internal_sub_id: AtomicU64::new(0),
    };

    (session, turn_context)
}

async fn make_session_with_config(
    mutator: impl FnOnce(&mut Config),
) -> anyhow::Result<Arc<Session>> {
    let (session, _rx_event) = make_session_with_config_and_rx(mutator).await?;
    Ok(session)
}

async fn load_latest_config_for_session(session: &Session) -> Config {
    let config = session.get_config().await;
    ConfigBuilder::default()
        .codex_home(config.codex_home.to_path_buf())
        .fallback_cwd(Some(config.cwd.to_path_buf()))
        .build()
        .await
        .expect("load latest config for session")
}

async fn make_session_with_config_and_rx(
    mutator: impl FnOnce(&mut Config),
) -> anyhow::Result<(Arc<Session>, async_channel::Receiver<Event>)> {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let mut config = build_test_config(codex_home.path()).await;
    mutator(&mut config);
    let config = Arc::new(config);
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = models_manager_with_provider(
        config.codex_home.to_path_buf(),
        auth_manager.clone(),
        config.model_provider.clone(),
    );
    let model = get_model_offline_for_tests(config.model.as_deref());
    let model_info =
        construct_model_info_offline_for_tests(model.as_str(), &config.to_models_manager_config());
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort: config.model_reasoning_effort,
            developer_instructions: None,
        },
    };
    let default_environments = vec![TurnEnvironmentSelection {
        environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
        cwd: config.cwd.clone(),
    }];
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        permission_profile_state: config.permissions.permission_profile_state().clone(),
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        workspace_roots: config.workspace_roots.clone(),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        environments: default_environments,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        app_server_client_version: None,
        session_source: SessionSource::Exec,
        thread_source: None,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };

    let (tx_event, rx_event) = async_channel::unbounded();
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.to_path_buf()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        /*bundled_skills_enabled*/ true,
    ));

    let session = Session::new(
        session_configuration,
        Arc::clone(&config),
        "11111111-1111-4111-8111-111111111111".to_string(),
        auth_manager,
        models_manager,
        Arc::new(ExecPolicyManager::default()),
        tx_event,
        agent_status_tx,
        InitialHistory::New,
        /*spine_fork_source_boundary*/ None,
        SessionSource::Exec,
        skills_manager,
        plugins_manager,
        mcp_manager,
        Arc::new(codex_extension_api::ExtensionRegistryBuilder::new().build()),
        AgentControl::default(),
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        /*analytics_events_client*/ None,
        Arc::new(codex_thread_store::LocalThreadStore::new(
            codex_thread_store::LocalThreadStoreConfig::from_config(config.as_ref()),
            /*state_db*/ None,
        )),
        codex_rollout_trace::ThreadTraceContext::disabled(),
        /*attestation_provider*/ None,
    )
    .await?;

    Ok((session, rx_event))
}

async fn make_session_with_history_source_and_agent_control_and_rx(
    initial_history: InitialHistory,
    session_source: SessionSource,
    agent_control: AgentControl,
) -> anyhow::Result<(Arc<Session>, async_channel::Receiver<Event>)> {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let mut config = build_test_config(codex_home.path()).await;
    config.ephemeral = true;
    let config = Arc::new(config);
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    let models_manager = models_manager_with_provider(
        config.codex_home.to_path_buf(),
        auth_manager.clone(),
        config.model_provider.clone(),
    );
    let model = get_model_offline_for_tests(config.model.as_deref());
    let model_info =
        construct_model_info_offline_for_tests(model.as_str(), &config.to_models_manager_config());
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort: config.model_reasoning_effort,
            developer_instructions: None,
        },
    };
    let default_environments = vec![TurnEnvironmentSelection {
        environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
        cwd: config.cwd.clone(),
    }];
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        permission_profile_state: config.permissions.permission_profile_state().clone(),
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        workspace_roots: config.workspace_roots.clone(),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        environments: default_environments,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        app_server_client_version: None,
        session_source: session_source.clone(),
        thread_source: None,
        dynamic_tools: Vec::new(),
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };

    let (tx_event, rx_event) = async_channel::unbounded();
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.to_path_buf()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        /*bundled_skills_enabled*/ true,
    ));

    let session = Session::new(
        session_configuration,
        Arc::clone(&config),
        "11111111-1111-4111-8111-111111111111".to_string(),
        auth_manager,
        models_manager,
        Arc::new(ExecPolicyManager::default()),
        tx_event,
        agent_status_tx,
        initial_history,
        /*spine_fork_source_boundary*/ None,
        session_source,
        skills_manager,
        plugins_manager,
        mcp_manager,
        Arc::new(codex_extension_api::ExtensionRegistryBuilder::new().build()),
        agent_control,
        Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        /*analytics_events_client*/ None,
        Arc::new(codex_thread_store::LocalThreadStore::new(
            codex_thread_store::LocalThreadStoreConfig::from_config(config.as_ref()),
            Some(
                codex_state::StateRuntime::init(
                    config.sqlite_home.clone(),
                    config.model_provider_id.clone(),
                )
                .await
                .expect("state db should initialize"),
            ),
        )),
        codex_rollout_trace::ThreadTraceContext::disabled(),
        /*attestation_provider*/ None,
    )
    .await?;

    Ok((session, rx_event))
}

#[tokio::test]
async fn resumed_root_session_uses_thread_id_as_session_id() {
    let thread_id = ThreadId::new();
    let (session, rx_event) = make_session_with_history_source_and_agent_control_and_rx(
        InitialHistory::Resumed(ResumedHistory {
            conversation_id: thread_id,
            history: Vec::new(),
            rollout_path: None,
        }),
        SessionSource::Exec,
        AgentControl::default(),
    )
    .await
    .expect("resume should succeed");

    assert_eq!(session.thread_id(), thread_id);
    assert_eq!(session.session_id(), SessionId::from(thread_id));

    let event = rx_event.recv().await.expect("session configured event");
    let EventMsg::SessionConfigured(event) = event.msg else {
        panic!("expected session configured event");
    };
    assert_eq!(event.session_id, SessionId::from(thread_id));
    assert_eq!(event.thread_id, thread_id);
}

#[tokio::test]
async fn resumed_subagent_session_keeps_inherited_session_id() {
    let parent_thread_id = ThreadId::new();
    let parent_session_id = SessionId::from(parent_thread_id);
    let thread_id = ThreadId::new();
    let session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id,
        depth: 1,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
    });
    let (session, rx_event) = make_session_with_history_source_and_agent_control_and_rx(
        InitialHistory::Resumed(ResumedHistory {
            conversation_id: thread_id,
            history: Vec::new(),
            rollout_path: None,
        }),
        session_source,
        AgentControl::default().with_session_id(parent_session_id),
    )
    .await
    .expect("resume should succeed");

    assert_eq!(session.thread_id(), thread_id);
    assert_eq!(session.session_id(), parent_session_id);

    let event = rx_event.recv().await.expect("session configured event");
    let EventMsg::SessionConfigured(event) = event.msg else {
        panic!("expected session configured event");
    };
    assert_eq!(event.session_id, parent_session_id);
    assert_eq!(event.thread_id, thread_id);
}

#[tokio::test]
async fn notify_request_permissions_response_ignores_unmatched_call_id() {
    let (session, _turn_context) = make_session_and_context().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());

    session
        .notify_request_permissions_response(
            "missing",
            codex_protocol::request_permissions::RequestPermissionsResponse {
                permissions: RequestPermissionProfile {
                    network: Some(codex_protocol::models::NetworkPermissions {
                        enabled: Some(true),
                    }),
                    ..RequestPermissionProfile::default()
                },
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            },
        )
        .await;

    assert_eq!(session.granted_turn_permissions().await, None);
}

#[tokio::test]
async fn record_granted_request_permissions_for_turn_uses_originating_turn() {
    let (session, _turn_context) = make_session_and_context().await;
    let originating_active_turn = ActiveTurn::default();
    let originating_turn_state = Arc::clone(&originating_active_turn.turn_state);
    *session.active_turn.lock().await = Some(originating_active_turn);

    let current_active_turn = ActiveTurn::default();
    let current_turn_state = Arc::clone(&current_active_turn.turn_state);
    *session.active_turn.lock().await = Some(current_active_turn);

    let requested_permissions = RequestPermissionProfile {
        network: Some(codex_protocol::models::NetworkPermissions {
            enabled: Some(true),
        }),
        ..RequestPermissionProfile::default()
    };
    session
        .record_granted_request_permissions_for_turn(
            &codex_protocol::request_permissions::RequestPermissionsResponse {
                permissions: requested_permissions.clone(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            },
            Some(&originating_turn_state),
        )
        .await;

    assert_eq!(
        originating_turn_state.lock().await.granted_permissions(),
        Some(requested_permissions.into())
    );
    assert_eq!(current_turn_state.lock().await.granted_permissions(), None);
    assert_eq!(session.granted_turn_permissions().await, None);
}

#[tokio::test]
async fn enable_strict_auto_review_for_turn_uses_originating_turn() {
    let (session, _turn_context) = make_session_and_context().await;
    let originating_active_turn = ActiveTurn::default();
    let originating_turn_state = Arc::clone(&originating_active_turn.turn_state);
    *session.active_turn.lock().await = Some(originating_active_turn);

    let requested_permissions = RequestPermissionProfile {
        network: Some(codex_protocol::models::NetworkPermissions {
            enabled: Some(true),
        }),
        ..RequestPermissionProfile::default()
    };
    session
        .record_granted_request_permissions_for_turn(
            &codex_protocol::request_permissions::RequestPermissionsResponse {
                permissions: requested_permissions.clone(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: true,
            },
            Some(&originating_turn_state),
        )
        .await;

    assert!(
        originating_turn_state
            .lock()
            .await
            .strict_auto_review_enabled()
    );
}

#[test]
fn strict_auto_review_session_scope_grants_no_permissions() {
    let requested_permissions = RequestPermissionProfile {
        network: Some(codex_protocol::models::NetworkPermissions {
            enabled: Some(true),
        }),
        ..RequestPermissionProfile::default()
    };

    let response = Session::normalize_request_permissions_response(
        requested_permissions.clone(),
        codex_protocol::request_permissions::RequestPermissionsResponse {
            permissions: requested_permissions,
            scope: PermissionGrantScope::Session,
            strict_auto_review: true,
        },
        std::path::Path::new("/tmp"),
    );

    assert_eq!(
        response,
        codex_protocol::request_permissions::RequestPermissionsResponse {
            permissions: RequestPermissionProfile::default(),
            scope: PermissionGrantScope::Turn,
            strict_auto_review: false,
        }
    );
}

#[tokio::test]
async fn request_permissions_emits_event_when_granular_policy_allows_requests() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "call-1".to_string();
    let expected_response = codex_protocol::request_permissions::RequestPermissionsResponse {
        permissions: RequestPermissionProfile {
            network: Some(codex_protocol::models::NetworkPermissions {
                enabled: Some(true),
            }),
            ..RequestPermissionProfile::default()
        },
        scope: PermissionGrantScope::Turn,
        strict_auto_review: false,
    };

    let handle = tokio::spawn({
        let session = Arc::clone(&session);
        let turn_context = Arc::clone(&turn_context);
        let call_id = call_id.clone();
        async move {
            session
                .request_permissions(
                    &turn_context,
                    call_id,
                    codex_protocol::request_permissions::RequestPermissionsArgs {
                        reason: Some("need network".to_string()),
                        permissions: RequestPermissionProfile {
                            network: Some(codex_protocol::models::NetworkPermissions {
                                enabled: Some(true),
                            }),
                            ..RequestPermissionProfile::default()
                        },
                    },
                    CancellationToken::new(),
                )
                .await
        }
    });

    let request_event = tokio::time::timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("request_permissions event timed out")
        .expect("request_permissions event missing");
    let EventMsg::RequestPermissions(request) = request_event.msg else {
        panic!("expected request_permissions event");
    };
    assert_eq!(request.call_id, call_id);
    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    assert_eq!(request.cwd, Some(turn_cwd));

    session
        .notify_request_permissions_response(&request.call_id, expected_response.clone())
        .await;

    let response = tokio::time::timeout(StdDuration::from_secs(1), handle)
        .await
        .expect("request_permissions future timed out")
        .expect("request_permissions join error");

    assert_eq!(response, Some(expected_response));
}

#[tokio::test]
async fn request_permissions_response_materializes_session_cwd_grants_before_recording() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: true,
            mcp_elicitations: true,
        }))
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "call-1".to_string();
    let requested_permissions = RequestPermissionProfile {
        file_system: Some(FileSystemPermissions {
            entries: vec![FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
                },
                access: FileSystemAccessMode::Write,
            }],
            glob_scan_max_depth: None,
        }),
        ..Default::default()
    };

    let handle = tokio::spawn({
        let session = Arc::clone(&session);
        let turn_context = Arc::clone(&turn_context);
        let call_id = call_id.clone();
        let requested_permissions = requested_permissions.clone();
        async move {
            session
                .request_permissions(
                    &turn_context,
                    call_id,
                    codex_protocol::request_permissions::RequestPermissionsArgs {
                        reason: Some("need cwd write".to_string()),
                        permissions: requested_permissions,
                    },
                    CancellationToken::new(),
                )
                .await
        }
    });

    let request_event = tokio::time::timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("request_permissions event timed out")
        .expect("request_permissions event missing");
    let EventMsg::RequestPermissions(request) = request_event.msg else {
        panic!("expected request_permissions event");
    };
    let request_cwd = request.cwd.clone().expect("request cwd");

    session
        .notify_request_permissions_response(
            &request.call_id,
            codex_protocol::request_permissions::RequestPermissionsResponse {
                permissions: request.permissions,
                scope: PermissionGrantScope::Session,
                strict_auto_review: false,
            },
        )
        .await;

    let expected_permissions = RequestPermissionProfile {
        file_system: Some(FileSystemPermissions::from_read_write_roots(
            /*read*/ None,
            Some(vec![request_cwd]),
        )),
        ..Default::default()
    };
    let expected_response = codex_protocol::request_permissions::RequestPermissionsResponse {
        permissions: expected_permissions.clone(),
        scope: PermissionGrantScope::Session,
        strict_auto_review: false,
    };

    let response = tokio::time::timeout(StdDuration::from_secs(1), handle)
        .await
        .expect("request_permissions future timed out")
        .expect("request_permissions join error");

    assert_eq!(response, Some(expected_response));
    assert_eq!(
        session.granted_session_permissions().await,
        Some(expected_permissions.into())
    );
}

#[tokio::test]
async fn request_permissions_is_auto_denied_when_granular_policy_blocks_tool_requests() {
    let (session, mut turn_context, rx) = make_session_and_context_with_rx().await;
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    Arc::get_mut(&mut turn_context)
        .expect("single turn context ref")
        .approval_policy
        .set(AskForApproval::Granular(GranularApprovalConfig {
            sandbox_approval: true,
            rules: true,
            skill_approval: true,
            request_permissions: false,
            mcp_elicitations: true,
        }))
        .expect("test setup should allow updating approval policy");

    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "call-1".to_string();
    let response = session
        .request_permissions(
            &turn_context,
            call_id,
            codex_protocol::request_permissions::RequestPermissionsArgs {
                reason: Some("need network".to_string()),
                permissions: RequestPermissionProfile {
                    network: Some(codex_protocol::models::NetworkPermissions {
                        enabled: Some(true),
                    }),
                    ..RequestPermissionProfile::default()
                },
            },
            CancellationToken::new(),
        )
        .await;

    assert_eq!(
        response,
        Some(
            codex_protocol::request_permissions::RequestPermissionsResponse {
                permissions: RequestPermissionProfile::default(),
                scope: PermissionGrantScope::Turn,
                strict_auto_review: false,
            }
        )
    );
    assert!(
        tokio::time::timeout(StdDuration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "request_permissions should not emit an event when granular.request_permissions is false"
    );
}

#[tokio::test]
async fn submit_with_id_captures_current_span_trace_context() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(1);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let codex = Codex {
        tx_sub,
        rx_event,
        agent_status,
        session: Arc::new(session),
        session_loop_termination: completed_session_loop_termination(),
    };

    let _trace_test_context = install_test_tracing("codex-core-tests");

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let expected_trace = async {
        let expected_trace =
            current_span_w3c_trace_context().expect("current span should have trace context");
        codex
            .submit_with_id(Submission {
                id: "sub-1".into(),
                op: Op::Interrupt,
                trace: None,
            })
            .await
            .expect("submit should succeed");
        expected_trace
    }
    .instrument(request_span)
    .await;

    let submitted = rx_sub.recv().await.expect("submission");
    assert_eq!(submitted.trace, Some(expected_trace));
}

#[tokio::test]
async fn new_default_turn_captures_current_span_trace_id() {
    let (session, _turn_context) = make_session_and_context().await;

    let _trace_test_context = install_test_tracing("codex-core-tests");

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let turn_context_item = async {
        let expected_trace_id = Span::current()
            .context()
            .span()
            .span_context()
            .trace_id()
            .to_string();
        let turn_context = session.new_default_turn().await;
        let turn_context_item = turn_context.to_turn_context_item();
        assert_eq!(turn_context_item.trace_id, Some(expected_trace_id));
        turn_context_item
    }
    .instrument(request_span)
    .await;

    assert_eq!(
        turn_context_item.trace_id.as_deref(),
        Some("00000000000000000000000000000011")
    );
}

#[test]
fn submission_dispatch_span_prefers_submission_trace_context() {
    let _trace_test_context = install_test_tracing("codex-core-tests");

    let ambient_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000033-0000000000000044-01".into()),
        tracestate: None,
    };
    let ambient_span = info_span!("ambient");
    assert!(set_parent_from_w3c_trace_context(
        &ambient_span,
        &ambient_parent
    ));

    let submission_trace = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000055-0000000000000066-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let dispatch_span = ambient_span.in_scope(|| {
        submission_dispatch_span(&Submission {
            id: "sub-1".into(),
            op: Op::Interrupt,
            trace: Some(submission_trace),
        })
    });

    let trace_id = dispatch_span.context().span().span_context().trace_id();
    assert_eq!(
        trace_id,
        TraceId::from_hex("00000000000000000000000000000055").expect("trace id")
    );
}

#[test]
fn submission_dispatch_span_uses_debug_for_realtime_audio() {
    let _trace_test_context = install_test_tracing("codex-core-tests");

    let dispatch_span = submission_dispatch_span(&Submission {
        id: "sub-1".into(),
        op: Op::RealtimeConversationAudio(ConversationAudioParams {
            frame: RealtimeAudioFrame {
                data: "ZmFrZQ==".into(),
                sample_rate: 16_000,
                num_channels: 1,
                samples_per_channel: Some(160),
                item_id: None,
            },
        }),
        trace: None,
    });

    assert_eq!(
        dispatch_span.metadata().expect("span metadata").level(),
        &tracing::Level::DEBUG
    );
}

#[test]
fn op_kind_distinguishes_turn_ops() {
    assert_eq!(
        Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,
            permission_profile: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        }
        .kind(),
        "override_turn_context"
    );
    assert_eq!(
        Op::UserInput {
            environments: None,
            items: vec![],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        }
        .kind(),
        "user_input"
    );
    assert_eq!(
        Op::UserInputWithTurnContext {
            environments: None,
            items: vec![],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            cwd: None,
            workspace_roots: None,
            profile_workspace_roots: None,
            approval_policy: None,
            approvals_reviewer: None,
            sandbox_policy: None,
            permission_profile: None,
            active_permission_profile: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        }
        .kind(),
        "user_input_with_turn_context"
    );
}

#[tokio::test]
async fn user_turn_updates_approvals_reviewer() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    let config = session.get_config().await;

    handlers::user_input_or_turn(
        &session,
        "sub-1".to_string(),
        Op::UserTurn {
            environments: None,
            items: vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            cwd: config.cwd.to_path_buf(),
            approval_policy: config.permissions.approval_policy.value(),
            approvals_reviewer: Some(codex_config::types::ApprovalsReviewer::AutoReview),
            sandbox_policy: config.legacy_sandbox_policy(),
            permission_profile: None,
            model: turn_context.model_info.slug.clone(),
            effort: config.model_reasoning_effort,
            summary: config.model_reasoning_summary,
            service_tier: None,
            final_output_json_schema: None,
            collaboration_mode: None,
            personality: config.personality,
        },
    )
    .await;

    let state = session.state.lock().await;
    assert_eq!(
        state.session_configuration.approvals_reviewer,
        codex_config::types::ApprovalsReviewer::AutoReview
    );
}

#[tokio::test]
async fn turn_environments_set_primary_environment() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let selected_cwd =
        AbsolutePathBuf::try_from(session.get_config().await.cwd.as_path().join("selected"))
            .expect("absolute path");

    let turn_context = session
        .new_turn_with_sub_id(
            "sub-1".to_string(),
            SessionSettingsUpdate {
                environments: Some(vec![TurnEnvironmentSelection {
                    environment_id: "local".to_string(),
                    cwd: selected_cwd.clone(),
                }]),
                ..Default::default()
            },
        )
        .await
        .expect("turn should start");

    let turn_environments = &turn_context.environments;
    assert_eq!(turn_environments.turn_environments.len(), 1);
    let turn_environment = turn_context
        .environments
        .primary()
        .expect("primary environment should be set");
    assert!(std::sync::Arc::ptr_eq(
        &turn_environment.environment,
        &turn_environments.turn_environments[0].environment
    ));
    assert!(!turn_context.environments.turn_environments.is_empty());
    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    assert_eq!(turn_cwd.as_path(), selected_cwd.as_path());
    assert_eq!(turn_context.config.cwd.as_path(), selected_cwd.as_path());
}

#[tokio::test]
async fn default_turn_overlays_session_cwd_onto_stored_thread_environments() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let session_cwd = session.get_config().await.cwd.clone();
    let selected_cwd =
        AbsolutePathBuf::try_from(session_cwd.as_path().join("selected")).expect("absolute path");

    {
        let mut state = session.state.lock().await;
        state.session_configuration.environments = vec![TurnEnvironmentSelection {
            environment_id: "local".to_string(),
            cwd: selected_cwd.clone(),
        }];
    }

    let turn_context = session.new_default_turn().await;

    let turn_environments = &turn_context.environments;
    assert_eq!(turn_environments.turn_environments.len(), 1);
    let turn_environment = turn_context
        .environments
        .primary()
        .expect("primary environment should be set");
    assert!(std::sync::Arc::ptr_eq(
        &turn_environment.environment,
        &turn_environments.turn_environments[0].environment
    ));
    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    assert_eq!(turn_cwd, session_cwd);
    assert_eq!(turn_context.config.cwd, session_cwd);
}

#[tokio::test]
async fn default_turn_honors_empty_stored_thread_environments() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let session_cwd = session.get_config().await.cwd.clone();

    {
        let mut state = session.state.lock().await;
        state.session_configuration.environments = Vec::new();
    }

    let turn_context = session.new_default_turn().await;

    assert!(turn_context.environments.primary().is_none());
    assert!(turn_context.environments.turn_environments.is_empty());
    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    assert_eq!(turn_cwd, session_cwd);
    assert_eq!(turn_context.config.cwd, session_cwd);
    assert_eq!(turn_context.environments.turn_environments.len(), 0);
}

#[tokio::test]
async fn primary_environment_uses_first_turn_environment() {
    let (_session, mut turn_context) = make_session_and_context().await;
    let first_environment = turn_context.environments.turn_environments[0].clone();
    #[allow(deprecated)]
    let second_cwd = turn_context.cwd.join("second");
    turn_context
        .environments
        .turn_environments
        .push(TurnEnvironment {
            environment_id: "second".to_string(),
            environment: Arc::clone(&first_environment.environment),
            cwd: second_cwd.clone(),
            shell: None,
        });

    assert_eq!(
        turn_context
            .environments
            .primary()
            .expect("primary environment")
            .environment_id,
        first_environment.environment_id
    );
    assert_eq!(
        turn_context
            .environments
            .turn_environments
            .iter()
            .find(|environment| environment.environment_id == "second")
            .expect("second environment")
            .cwd,
        second_cwd
    );
    assert_eq!(turn_context.environments.turn_environments.len(), 2);
    assert_eq!(
        turn_context.environments.turn_environments[1].cwd,
        second_cwd
    );
}

#[tokio::test]
async fn empty_turn_environments_clear_primary_environment() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;

    let turn_context = session
        .new_turn_with_sub_id(
            "sub-1".to_string(),
            SessionSettingsUpdate {
                environments: Some(vec![]),
                ..Default::default()
            },
        )
        .await
        .expect("turn should start");

    assert!(turn_context.environments.primary().is_none());
    assert!(turn_context.environments.turn_environments.is_empty());
    #[allow(deprecated)]
    let turn_cwd = turn_context.cwd.clone();
    assert_eq!(turn_cwd, session.get_config().await.cwd);
    assert_eq!(turn_context.config.cwd, session.get_config().await.cwd);
}

#[tokio::test]
async fn unknown_turn_environment_returns_error() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let original_configuration = {
        let state = session.state.lock().await;
        state.session_configuration.clone()
    };

    let err = session
        .new_turn_with_sub_id(
            "sub-1".to_string(),
            SessionSettingsUpdate {
                environments: Some(vec![TurnEnvironmentSelection {
                    environment_id: "missing".to_string(),
                    cwd: original_configuration.cwd.clone(),
                }]),
                ..Default::default()
            },
        )
        .await
        .expect_err("unknown environment should fail");

    let current_configuration = {
        let state = session.state.lock().await;
        state.session_configuration.clone()
    };
    assert!(matches!(err, CodexErr::InvalidRequest(_)));
    assert!(err.to_string().contains("missing"));
    assert_eq!(current_configuration.cwd, original_configuration.cwd);
    assert_eq!(
        current_configuration.environments,
        original_configuration.environments
    );
}

#[tokio::test]
async fn duplicate_turn_environment_returns_error_without_mutating_session() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let original_configuration = {
        let state = session.state.lock().await;
        state.session_configuration.clone()
    };

    let err = session
        .new_turn_with_sub_id(
            "sub-1".to_string(),
            SessionSettingsUpdate {
                environments: Some(vec![
                    TurnEnvironmentSelection {
                        environment_id: "local".to_string(),
                        cwd: original_configuration.cwd.clone(),
                    },
                    TurnEnvironmentSelection {
                        environment_id: "local".to_string(),
                        cwd: original_configuration.cwd.join("second"),
                    },
                ]),
                ..Default::default()
            },
        )
        .await
        .expect_err("duplicate environment should fail");

    let current_configuration = {
        let state = session.state.lock().await;
        state.session_configuration.clone()
    };
    assert!(matches!(err, CodexErr::InvalidRequest(_)));
    assert!(err.to_string().contains("duplicate"));
    assert_eq!(current_configuration.cwd, original_configuration.cwd);
    assert_eq!(
        current_configuration.environments,
        original_configuration.environments
    );
}

#[tokio::test]
async fn spawn_task_turn_span_inherits_dispatch_trace_context() {
    struct TraceCaptureTask {
        captured_trace: Arc<std::sync::Mutex<Option<W3cTraceContext>>>,
    }

    impl SessionTask for TraceCaptureTask {
        fn kind(&self) -> TaskKind {
            TaskKind::Regular
        }

        fn span_name(&self) -> &'static str {
            "session_task.trace_capture"
        }

        async fn run(
            self: Arc<Self>,
            _session: Arc<SessionTaskContext>,
            _ctx: Arc<TurnContext>,
            _input: Vec<UserInput>,
            _cancellation_token: CancellationToken,
        ) -> crate::session::turn::TurnOutput {
            let mut trace = self
                .captured_trace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *trace = current_span_w3c_trace_context();
            crate::session::turn::TurnOutput::complete(None)
        }
    }

    let _trace_test_context = install_test_tracing("codex-core-tests");

    let request_parent = W3cTraceContext {
        traceparent: Some("00-00000000000000000000000000000011-0000000000000022-01".into()),
        tracestate: Some("vendor=value".into()),
    };
    let request_span = tracing::info_span!("app_server.request");
    assert!(set_parent_from_w3c_trace_context(
        &request_span,
        &request_parent
    ));

    let submission_trace =
        async { current_span_w3c_trace_context().expect("request span should have trace context") }
            .instrument(request_span)
            .await;

    let dispatch_span = submission_dispatch_span(&Submission {
        id: "sub-1".into(),
        op: Op::Interrupt,
        trace: Some(submission_trace.clone()),
    });
    let dispatch_span_id = dispatch_span.context().span().span_context().span_id();

    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let captured_trace = Arc::new(std::sync::Mutex::new(None));

    async {
        sess.spawn_task(
            Arc::clone(&tc),
            vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            TraceCaptureTask {
                captured_trace: Arc::clone(&captured_trace),
            },
        )
        .await;
    }
    .instrument(dispatch_span)
    .await;

    let evt = tokio::time::timeout(StdDuration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for turn completion")
        .expect("event");
    assert!(matches!(evt.msg, EventMsg::TurnComplete(_)));

    let task_trace = captured_trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("turn task should capture the current span trace context");
    let submission_context =
        codex_otel::context_from_w3c_trace_context(&submission_trace).expect("submission");
    let task_context = codex_otel::context_from_w3c_trace_context(&task_trace).expect("task trace");

    assert_eq!(
        task_context.span().span_context().trace_id(),
        submission_context.span().span_context().trace_id()
    );
    assert_ne!(
        task_context.span().span_context().span_id(),
        dispatch_span_id
    );
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn shutdown_complete_does_not_append_to_thread_store_after_shutdown() {
    let (mut session, _turn_context) = make_session_and_context().await;
    let store = Arc::new(codex_thread_store::InMemoryThreadStore::default());
    let thread_store: Arc<dyn codex_thread_store::ThreadStore> = store.clone();
    let config = session.get_config().await;
    let live_thread = LiveThread::create(
        Arc::clone(&thread_store),
        CreateThreadParams {
            thread_id: session.conversation_id,
            forked_from_id: None,
            source: SessionSource::Exec,
            thread_source: None,
            base_instructions: BaseInstructions::default(),
            dynamic_tools: Vec::new(),
            metadata: ThreadPersistenceMetadata {
                cwd: Some(config.cwd.to_path_buf()),
                model_provider: config.model_provider_id.clone(),
                memory_mode: if config.memories.generate_memories {
                    ThreadMemoryMode::Enabled
                } else {
                    ThreadMemoryMode::Disabled
                },
            },
            event_persistence_mode: ThreadEventPersistenceMode::Limited,
        },
    )
    .await
    .expect("create thread persistence");
    session.services.thread_store = thread_store;
    session.services.live_thread = Some(live_thread);
    let session = Arc::new(session);

    assert!(handlers::shutdown(&session, "sub-1".to_string()).await);

    assert_eq!(
        codex_thread_store::InMemoryThreadStoreCalls {
            create_thread: 1,
            shutdown_thread: 1,
            ..Default::default()
        },
        store.calls().await
    );
}

#[tokio::test]
#[serial(spine_writer_lock)]
async fn shutdown_releases_spine_writer_lock_without_dropping_session_arc() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config
                .features
                .enable(Feature::SpineTrim)
                .expect("enable spine trim");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("record initial history");

    let err = SpineRuntime::load_for_rollout_items_for_writer(&rollout_path, &[], &[])
        .expect_err("live session should own the writer lock before shutdown");
    assert!(
        err.to_string()
            .contains("already owned by another live Codex process"),
        "unexpected writer lock error: {err}"
    );

    assert!(handlers::shutdown(&session, "sub-1".to_string()).await);

    SpineRuntime::load_for_rollout_items_for_writer(&rollout_path, &[], &[])
        .expect("shutdown must release the Spine sidecar writer lock")
        .expect("spine sidecar should exist");
    drop(session);
}

#[tokio::test]
async fn submission_loop_channel_close_emits_thread_stop_lifecycle() {
    struct SessionStopMarker;
    struct ThreadStopMarker;

    struct ThreadStopRecorder {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        expected_thread_id: ThreadId,
    }

    impl codex_extension_api::ThreadLifecycleContributor<crate::config::Config> for ThreadStopRecorder {
        fn on_thread_stop(&self, input: codex_extension_api::ThreadStopInput<'_>) {
            assert_eq!(
                self.expected_thread_id.to_string(),
                input.thread_store.level_id()
            );
            assert!(input.session_store.get::<SessionStopMarker>().is_some());
            assert!(input.thread_store.get::<ThreadStopMarker>().is_some());
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    let (mut session, turn_context) = make_session_and_context().await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    builder.thread_lifecycle_contributor(Arc::new(ThreadStopRecorder {
        calls: Arc::clone(&calls),
        expected_thread_id: session.conversation_id,
    }));
    session.services.extensions = Arc::new(builder.build());
    session
        .services
        .session_extension_data
        .insert(SessionStopMarker);
    session
        .services
        .thread_extension_data
        .insert(ThreadStopMarker);

    let (tx_sub, rx_sub) = async_channel::bounded(1);
    drop(tx_sub);
    let session = Arc::new(session);
    submission_loop(session, Arc::clone(&turn_context.config), rx_sub).await;

    assert_eq!(1, calls.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn submission_loop_channel_close_aborts_active_turn_before_thread_stop_lifecycle() {
    struct LifecycleRecorder {
        calls: Arc<std::sync::Mutex<Vec<&'static str>>>,
        expected_thread_id: ThreadId,
        expected_turn_id: String,
    }

    impl codex_extension_api::ThreadLifecycleContributor<crate::config::Config> for LifecycleRecorder {
        fn on_thread_stop(&self, input: codex_extension_api::ThreadStopInput<'_>) {
            assert_eq!(
                self.expected_thread_id.to_string(),
                input.thread_store.level_id()
            );
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push("thread_stop");
        }
    }

    impl codex_extension_api::TurnLifecycleContributor for LifecycleRecorder {
        fn on_turn_abort(&self, input: codex_extension_api::TurnAbortInput<'_>) {
            assert_eq!(
                self.expected_thread_id.to_string(),
                input.thread_store.level_id()
            );
            assert_eq!(self.expected_turn_id, input.turn_store.level_id());
            assert_eq!(TurnAbortReason::Interrupted, input.reason);
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push("turn_abort");
        }
    }

    let (mut session, turn_context) = make_session_and_context().await;
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = Arc::new(LifecycleRecorder {
        calls: Arc::clone(&calls),
        expected_thread_id: session.conversation_id,
        expected_turn_id: turn_context.sub_id.clone(),
    });
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::<crate::config::Config>::new();
    builder.thread_lifecycle_contributor(recorder.clone());
    builder.turn_lifecycle_contributor(recorder);
    session.services.extensions = Arc::new(builder.build());

    let session = Arc::new(session);
    session
        .spawn_task(
            Arc::new(turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

    let (tx_sub, rx_sub) = async_channel::bounded(1);
    drop(tx_sub);
    submission_loop(Arc::clone(&session), session.get_config().await, rx_sub).await;

    assert_eq!(
        vec!["turn_abort", "thread_stop"],
        *calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    );
}

#[tokio::test]
async fn shutdown_and_wait_allows_multiple_waiters() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(4);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let session_loop_handle = tokio::spawn(async move {
        let shutdown: Submission = rx_sub.recv().await.expect("shutdown submission");
        assert_eq!(shutdown.op, Op::Shutdown);
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    });
    let codex = Arc::new(Codex {
        tx_sub,
        rx_event,
        agent_status,
        session: Arc::new(session),
        session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
    });

    let waiter_1 = {
        let codex = Arc::clone(&codex);
        tokio::spawn(async move { codex.shutdown_and_wait().await })
    };
    let waiter_2 = {
        let codex = Arc::clone(&codex);
        tokio::spawn(async move { codex.shutdown_and_wait().await })
    };

    waiter_1
        .await
        .expect("first shutdown waiter join")
        .expect("first shutdown waiter");
    waiter_2
        .await
        .expect("second shutdown waiter join")
        .expect("second shutdown waiter");
}

#[tokio::test]
async fn shutdown_and_wait_waits_when_shutdown_is_already_in_progress() {
    let (session, _turn_context) = make_session_and_context().await;
    let (tx_sub, rx_sub) = async_channel::bounded(4);
    drop(rx_sub);
    let (_tx_event, rx_event) = async_channel::unbounded();
    let (_agent_status_tx, agent_status) = watch::channel(AgentStatus::PendingInit);
    let (shutdown_complete_tx, shutdown_complete_rx) = tokio::sync::oneshot::channel();
    let session_loop_handle = tokio::spawn(async move {
        let _ = shutdown_complete_rx.await;
    });
    let codex = Arc::new(Codex {
        tx_sub,
        rx_event,
        agent_status,
        session: Arc::new(session),
        session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
    });

    let waiter = {
        let codex = Arc::clone(&codex);
        tokio::spawn(async move { codex.shutdown_and_wait().await })
    };

    tokio::time::sleep(StdDuration::from_millis(10)).await;
    assert!(!waiter.is_finished());

    shutdown_complete_tx
        .send(())
        .expect("session loop should still be waiting to terminate");

    waiter
        .await
        .expect("shutdown waiter join")
        .expect("shutdown waiter");
}

#[tokio::test]
async fn shutdown_and_wait_shuts_down_cached_guardian_subagent() {
    let (parent_session, parent_turn_context) = make_session_and_context().await;
    let parent_session = Arc::new(parent_session);
    let parent_config = Arc::clone(&parent_turn_context.config);
    let (parent_tx_sub, parent_rx_sub) = async_channel::bounded(4);
    let (_parent_tx_event, parent_rx_event) = async_channel::unbounded();
    let (_parent_status_tx, parent_agent_status) = watch::channel(AgentStatus::PendingInit);
    let parent_session_for_loop = Arc::clone(&parent_session);
    let parent_session_loop_handle = tokio::spawn(async move {
        submission_loop(parent_session_for_loop, parent_config, parent_rx_sub).await;
    });
    let parent_codex = Codex {
        tx_sub: parent_tx_sub,
        rx_event: parent_rx_event,
        agent_status: parent_agent_status,
        session: Arc::clone(&parent_session),
        session_loop_termination: session_loop_termination_from_handle(parent_session_loop_handle),
    };

    let (child_session, _child_turn_context) = make_session_and_context().await;
    let (child_tx_sub, child_rx_sub) = async_channel::bounded(4);
    let (_child_tx_event, child_rx_event) = async_channel::unbounded();
    let (_child_status_tx, child_agent_status) = watch::channel(AgentStatus::PendingInit);
    let (child_shutdown_tx, child_shutdown_rx) = tokio::sync::oneshot::channel();
    let child_session_loop_handle = tokio::spawn(async move {
        let shutdown: Submission = child_rx_sub
            .recv()
            .await
            .expect("child shutdown submission");
        assert_eq!(shutdown.op, Op::Shutdown);
        child_shutdown_tx
            .send(())
            .expect("child shutdown signal should be delivered");
    });
    let child_codex = Codex {
        tx_sub: child_tx_sub,
        rx_event: child_rx_event,
        agent_status: child_agent_status,
        session: Arc::new(child_session),
        session_loop_termination: session_loop_termination_from_handle(child_session_loop_handle),
    };
    parent_session
        .guardian_review_session
        .cache_for_test(child_codex)
        .await;

    parent_codex
        .shutdown_and_wait()
        .await
        .expect("parent shutdown should succeed");

    child_shutdown_rx
        .await
        .expect("guardian subagent should receive a shutdown op");
}

#[tokio::test]
async fn cached_guardian_subagent_exposes_its_rollout_path() {
    let (parent_session, _parent_turn_context) = make_session_and_context().await;
    let parent_session = Arc::new(parent_session);

    let (mut child_session, _child_turn_context) = make_session_and_context().await;
    let child_rollout_path = attach_thread_persistence(&mut child_session).await;
    let (child_tx_sub, _child_rx_sub) = async_channel::bounded(4);
    let (_child_tx_event, child_rx_event) = async_channel::unbounded();
    let (_child_status_tx, child_agent_status) = watch::channel(AgentStatus::PendingInit);
    let child_session_loop_handle = tokio::spawn(async {});
    let child_codex = Codex {
        tx_sub: child_tx_sub,
        rx_event: child_rx_event,
        agent_status: child_agent_status,
        session: Arc::new(child_session),
        session_loop_termination: session_loop_termination_from_handle(child_session_loop_handle),
    };
    parent_session
        .guardian_review_session
        .cache_for_test(child_codex)
        .await;

    assert_eq!(
        parent_session
            .guardian_review_session
            .trunk_rollout_path()
            .await,
        Some(child_rollout_path)
    );
}

#[tokio::test]
async fn shutdown_and_wait_shuts_down_tracked_ephemeral_guardian_review() {
    let (parent_session, parent_turn_context) = make_session_and_context().await;
    let parent_session = Arc::new(parent_session);
    let parent_config = Arc::clone(&parent_turn_context.config);
    let (parent_tx_sub, parent_rx_sub) = async_channel::bounded(4);
    let (_parent_tx_event, parent_rx_event) = async_channel::unbounded();
    let (_parent_status_tx, parent_agent_status) = watch::channel(AgentStatus::PendingInit);
    let parent_session_for_loop = Arc::clone(&parent_session);
    let parent_session_loop_handle = tokio::spawn(async move {
        submission_loop(parent_session_for_loop, parent_config, parent_rx_sub).await;
    });
    let parent_codex = Codex {
        tx_sub: parent_tx_sub,
        rx_event: parent_rx_event,
        agent_status: parent_agent_status,
        session: Arc::clone(&parent_session),
        session_loop_termination: session_loop_termination_from_handle(parent_session_loop_handle),
    };

    let (child_session, _child_turn_context) = make_session_and_context().await;
    let (child_tx_sub, child_rx_sub) = async_channel::bounded(4);
    let (_child_tx_event, child_rx_event) = async_channel::unbounded();
    let (_child_status_tx, child_agent_status) = watch::channel(AgentStatus::PendingInit);
    let (child_shutdown_tx, child_shutdown_rx) = tokio::sync::oneshot::channel();
    let child_session_loop_handle = tokio::spawn(async move {
        let shutdown: Submission = child_rx_sub
            .recv()
            .await
            .expect("child shutdown submission");
        assert_eq!(shutdown.op, Op::Shutdown);
        child_shutdown_tx
            .send(())
            .expect("child shutdown signal should be delivered");
    });
    let child_codex = Codex {
        tx_sub: child_tx_sub,
        rx_event: child_rx_event,
        agent_status: child_agent_status,
        session: Arc::new(child_session),
        session_loop_termination: session_loop_termination_from_handle(child_session_loop_handle),
    };
    parent_session
        .guardian_review_session
        .register_ephemeral_for_test(child_codex)
        .await;

    parent_codex
        .shutdown_and_wait()
        .await
        .expect("parent shutdown should succeed");

    child_shutdown_rx
        .await
        .expect("ephemeral guardian review should receive a shutdown op");
}

async fn make_session_and_context_with_auth_and_config_and_rx<F>(
    auth: CodexAuth,
    dynamic_tools: Vec<DynamicToolSpec>,
    configure_config: F,
) -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
)
where
    F: FnOnce(&mut Config),
{
    let codex_home = tempfile::tempdir().expect("create temp dir");
    make_session_and_context_with_auth_config_home_and_rx(
        auth,
        dynamic_tools,
        codex_home.path(),
        configure_config,
    )
    .await
}

async fn make_session_and_context_with_auth_config_home_and_rx<F>(
    auth: CodexAuth,
    dynamic_tools: Vec<DynamicToolSpec>,
    codex_home: &Path,
    configure_config: F,
) -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
)
where
    F: FnOnce(&mut Config),
{
    let (tx_event, rx_event) = async_channel::unbounded();
    let mut config = build_test_config(codex_home).await;
    configure_config(&mut config);
    let state_db = if config.features.enabled(Feature::Goals) {
        Some(
            codex_state::StateRuntime::init(
                config.sqlite_home.clone(),
                config.model_provider_id.clone(),
            )
            .await
            .expect("goal tests should initialize sqlite state db"),
        )
    } else {
        None
    };
    let config = Arc::new(config);
    let thread_id = ThreadId::default();
    let auth_manager = AuthManager::from_auth_for_testing(auth);
    let models_manager = models_manager_with_provider(
        config.codex_home.to_path_buf(),
        auth_manager.clone(),
        config.model_provider.clone(),
    );
    let agent_control = AgentControl::default();
    let exec_policy = Arc::new(ExecPolicyManager::default());
    let (agent_status_tx, _agent_status_rx) = watch::channel(AgentStatus::PendingInit);
    let model = get_model_offline_for_tests(config.model.as_deref());
    let model_info =
        construct_model_info_offline_for_tests(model.as_str(), &config.to_models_manager_config());
    let reasoning_effort = config.model_reasoning_effort;
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Default,
        settings: Settings {
            model,
            reasoning_effort,
            developer_instructions: None,
        },
    };
    let default_environments = vec![TurnEnvironmentSelection {
        environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
        cwd: config.cwd.clone(),
    }];
    let session_configuration = SessionConfiguration {
        provider: config.model_provider.clone(),
        collaboration_mode,
        model_reasoning_summary: config.model_reasoning_summary,
        developer_instructions: config.developer_instructions.clone(),
        user_instructions: config.user_instructions.clone(),
        service_tier: None,
        personality: config.personality,
        base_instructions: config
            .base_instructions
            .clone()
            .unwrap_or_else(|| model_info.get_model_instructions(config.personality)),
        compact_prompt: config.compact_prompt.clone(),
        approval_policy: config.permissions.approval_policy.clone(),
        approvals_reviewer: config.approvals_reviewer,
        permission_profile_state: config.permissions.permission_profile_state().clone(),
        windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
        cwd: config.cwd.clone(),
        workspace_roots: config.workspace_roots.clone(),
        codex_home: config.codex_home.clone(),
        thread_name: None,
        environments: default_environments,
        original_config_do_not_use: Arc::clone(&config),
        metrics_service_name: None,
        app_server_client_name: None,
        app_server_client_version: None,
        session_source: SessionSource::Exec,
        thread_source: None,
        dynamic_tools,
        persist_extended_history: false,
        inherited_shell_snapshot: None,
        user_shell_override: None,
    };
    let per_turn_config =
        Session::build_per_turn_config(&session_configuration, session_configuration.cwd.clone());
    let model_info = construct_model_info_offline_for_tests(
        session_configuration.collaboration_mode.model(),
        &per_turn_config.to_models_manager_config(),
    );
    let session_telemetry = session_telemetry(
        thread_id,
        config.as_ref(),
        &model_info,
        session_configuration.session_source.clone(),
    );

    let state = SessionState::new(session_configuration.clone());
    let plugins_manager = Arc::new(PluginsManager::new(config.codex_home.to_path_buf()));
    let mcp_manager = Arc::new(McpManager::new(Arc::clone(&plugins_manager)));
    let skills_manager = Arc::new(SkillsManager::new(
        config.codex_home.clone(),
        /*bundled_skills_enabled*/ true,
    ));
    let network_approval = Arc::new(NetworkApprovalService::default());
    let environment = Arc::new(
        codex_exec_server::Environment::create_for_tests(/*exec_server_url*/ None)
            .expect("create environment"),
    );

    let services = SessionServices {
        mcp_connection_manager: Arc::new(RwLock::new(
            McpConnectionManager::new_uninitialized_with_permission_profile(
                &config.permissions.approval_policy,
                config.permissions.permission_profile(),
            ),
        )),
        mcp_startup_cancellation_token: Mutex::new(CancellationToken::new()),
        unified_exec_manager: UnifiedExecProcessManager::new(
            config.background_terminal_max_timeout,
        ),
        shell_zsh_path: None,
        main_execve_wrapper_exe: config.main_execve_wrapper_exe.clone(),
        analytics_events_client: AnalyticsEventsClient::new(
            Arc::clone(&auth_manager),
            config.chatgpt_base_url.trim_end_matches('/').to_string(),
            config.analytics_enabled,
        ),
        hooks: arc_swap::ArcSwap::from_pointee(Hooks::new(HooksConfig {
            legacy_notify_argv: config.notify.clone(),
            ..HooksConfig::default()
        })),
        rollout_thread_trace: codex_rollout_trace::ThreadTraceContext::disabled(),
        user_shell: Arc::new(default_user_shell()),
        shell_snapshot_tx: watch::channel(None).0,
        show_raw_agent_reasoning: config.show_raw_agent_reasoning,
        exec_policy,
        auth_manager: Arc::clone(&auth_manager),
        session_telemetry: session_telemetry.clone(),
        models_manager: Arc::clone(&models_manager),
        tool_approvals: Mutex::new(ApprovalStore::default()),
        guardian_rejections: Mutex::new(std::collections::HashMap::new()),
        guardian_rejection_circuit_breaker: Mutex::new(Default::default()),
        runtime_handle: tokio::runtime::Handle::current(),
        skills_manager,
        plugins_manager,
        mcp_manager,
        extensions: Arc::new(codex_extension_api::ExtensionRegistryBuilder::new().build()),
        session_extension_data: codex_extension_api::ExtensionData::new(
            agent_control.session_id().to_string(),
        ),
        thread_extension_data: codex_extension_api::ExtensionData::new(thread_id.to_string()),
        agent_control,
        network_proxy: None,
        network_approval: Arc::clone(&network_approval),
        state_db: state_db.clone(),
        live_thread: None,
        thread_store: Arc::new(codex_thread_store::LocalThreadStore::new(
            codex_thread_store::LocalThreadStoreConfig::from_config(config.as_ref()),
            state_db,
        )),
        attestation_provider: None,
        debug_request_capture_dir: None,
        model_client: ModelClient::new(
            Some(Arc::clone(&auth_manager)),
            thread_id.into(),
            thread_id,
            /*installation_id*/ "11111111-1111-4111-8111-111111111111".to_string(),
            session_configuration.provider.clone(),
            session_configuration.session_source.clone(),
            config.model_verbosity,
            config.features.enabled(Feature::EnableRequestCompression),
            config.features.enabled(Feature::RuntimeMetrics),
            Session::build_model_client_beta_features_header(config.as_ref()),
            /*attestation_provider*/ None,
            /*debug_request_capture_dir*/ None,
        ),
        code_mode_service: crate::tools::code_mode::CodeModeService::new(),
        environment_manager: Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
    };

    let plugin_outcome = services
        .plugins_manager
        .plugins_for_config(&per_turn_config.plugins_config_input())
        .await;
    let effective_skill_roots = plugin_outcome.effective_plugin_skill_roots();
    let skills_input =
        crate::skills_load_input_from_config(&per_turn_config, effective_skill_roots);
    let skill_fs = environment.get_filesystem();
    let skills_outcome = Arc::new(
        services
            .skills_manager
            .skills_for_config(&skills_input, Some(Arc::clone(&skill_fs)))
            .await,
    );
    let turn_environments = turn_environments_for_tests(&environment, &session_configuration.cwd);
    let turn_context = Arc::new(Session::make_turn_context(
        thread_id,
        SessionId::from(thread_id),
        Some(Arc::clone(&auth_manager)),
        &session_telemetry,
        session_configuration.provider.clone(),
        &session_configuration,
        services.user_shell.as_ref(),
        services.shell_zsh_path.as_ref(),
        services.main_execve_wrapper_exe.as_ref(),
        per_turn_config,
        model_info,
        &models_manager,
        /*network*/ None,
        turn_environments,
        session_configuration.cwd.clone(),
        "turn_id".to_string(),
        skills_outcome,
        /*goal_tools_supported*/ true,
    ));

    let (mailbox, mailbox_rx) = crate::agent::Mailbox::new();
    let session = Arc::new(Session {
        conversation_id: thread_id,
        installation_id: "11111111-1111-4111-8111-111111111111".to_string(),
        tx_event,
        agent_status: agent_status_tx,
        out_of_band_elicitation_paused: watch::channel(false).0,
        state: Mutex::new(state),
        managed_network_proxy_refresh_lock: Semaphore::new(/*permits*/ 1),
        features: config.features.clone(),
        pending_mcp_server_refresh_config: Mutex::new(None),
        conversation: Arc::new(RealtimeConversationManager::new()),
        active_turn: Mutex::new(None),
        mailbox,
        mailbox_rx: Mutex::new(mailbox_rx),
        idle_pending_input: Mutex::new(Vec::new()),
        goal_runtime: crate::goals::GoalRuntimeState::new(),
        guardian_review_session: crate::guardian::GuardianReviewSessionManager::default(),
        services,
        spine: (config.features.enabled(Feature::SpineJit)
            || config.features.enabled(Feature::SpineTrim))
        .then(|| {
            TokioMutex::new(SpineSessionState::new_with_features(
                config.features.enabled(Feature::SpineJit),
                config.features.enabled(Feature::SpineTrim),
            ))
        }),
        spine_pressure_prompt_state: Mutex::new(Default::default()),
        next_internal_sub_id: AtomicU64::new(0),
    });

    (session, turn_context, rx_event)
}

pub(crate) async fn make_session_and_context_with_dynamic_tools_and_rx(
    dynamic_tools: Vec<DynamicToolSpec>,
) -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
) {
    make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        dynamic_tools,
        |_config| {},
    )
    .await
}

async fn make_goal_session_and_context_with_rx() -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
    tempfile::TempDir,
) {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    let (session, turn_context, rx) = make_session_and_context_with_auth_config_home_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        codex_home.path(),
        |config| {
            config
                .features
                .enable(Feature::Goals)
                .expect("goal mode should be enableable in tests");
        },
    )
    .await;
    upsert_goal_test_thread(session.as_ref()).await;
    (session, turn_context, rx, codex_home)
}

async fn upsert_goal_test_thread(session: &Session) {
    let config = session.get_config().await;
    let state_db = session
        .state_db()
        .expect("goal test session should have a state db");
    let mut builder = codex_state::ThreadMetadataBuilder::new(
        session.conversation_id,
        config
            .codex_home
            .join("goal-test-rollout.jsonl")
            .to_path_buf(),
        chrono::Utc::now(),
        SessionSource::Cli,
    );
    builder.cwd = config.cwd.to_path_buf();
    builder.model_provider = Some(config.model_provider_id.clone());
    let metadata = builder.build(config.model_provider_id.as_str());
    state_db
        .upsert_thread(&metadata)
        .await
        .expect("goal test thread should be upserted");
}

// Like make_session_and_context, but returns Arc<Session> and the event receiver
// so tests can assert on emitted events.
pub(crate) async fn make_session_and_context_with_rx() -> (
    Arc<Session>,
    Arc<TurnContext>,
    async_channel::Receiver<Event>,
) {
    make_session_and_context_with_dynamic_tools_and_rx(Vec::new()).await
}

#[tokio::test]
async fn refresh_mcp_servers_is_deferred_until_next_turn() {
    let (session, turn_context) = make_session_and_context().await;
    let old_token = session.mcp_startup_cancellation_token().await;
    assert!(!old_token.is_cancelled());

    let mcp_oauth_credentials_store_mode =
        serde_json::to_value(OAuthCredentialsStoreMode::Auto).expect("serialize store mode");
    let refresh_config = McpServerRefreshConfig {
        mcp_servers: json!({}),
        mcp_oauth_credentials_store_mode,
    };
    {
        let mut guard = session.pending_mcp_server_refresh_config.lock().await;
        *guard = Some(refresh_config);
    }

    assert!(!old_token.is_cancelled());
    assert!(
        session
            .pending_mcp_server_refresh_config
            .lock()
            .await
            .is_some()
    );

    session
        .refresh_mcp_servers_if_requested(&turn_context, /*elicitation_reviewer*/ None)
        .await;

    assert!(old_token.is_cancelled());
    assert!(
        session
            .pending_mcp_server_refresh_config
            .lock()
            .await
            .is_none()
    );
    let new_token = session.mcp_startup_cancellation_token().await;
    assert!(!new_token.is_cancelled());
}

#[tokio::test]
async fn spawn_task_does_not_update_previous_turn_settings_for_non_run_turn_tasks() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    sess.set_previous_turn_settings(/*previous_turn_settings*/ None)
        .await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];

    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
    assert_eq!(sess.previous_turn_settings().await, None);
}

#[tokio::test]
async fn build_settings_update_items_emits_environment_item_for_network_changes() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;

    let mut config = (*current_context.config).clone();
    let mut requirements = config.config_layer_stack.requirements().clone();
    requirements.network = Some(Sourced::new(
        NetworkConstraints {
            domains: Some(NetworkDomainPermissionsToml {
                entries: std::collections::BTreeMap::from([
                    (
                        "api.example.com".to_string(),
                        NetworkDomainPermissionToml::Allow,
                    ),
                    (
                        "blocked.example.com".to_string(),
                        NetworkDomainPermissionToml::Deny,
                    ),
                ]),
            }),
            ..Default::default()
        },
        RequirementSource::CloudRequirements,
    ));
    let layers = config
        .config_layer_stack
        .get_layers(
            ConfigLayerStackOrdering::LowestPrecedenceFirst,
            /*include_disabled*/ true,
        )
        .into_iter()
        .cloned()
        .collect();
    config.config_layer_stack = ConfigLayerStack::new(
        layers,
        requirements,
        config.config_layer_stack.requirements_toml().clone(),
    )
    .expect("rebuild config layer stack with network requirements");
    current_context.config = Arc::new(config);

    let reference_context_item = previous_context.to_turn_context_item();
    let update_items = session
        .build_settings_update_items(Some(&reference_context_item), &current_context)
        .await;

    let environment_update = user_input_texts(&update_items)
        .into_iter()
        .find(|text| text.contains("<environment_context>"))
        .expect("environment update item should be emitted");
    assert!(environment_update.contains(
        "<network enabled=\"true\"><allowed>api.example.com</allowed><denied>blocked.example.com</denied></network>"
    ));
}

#[tokio::test]
async fn environment_context_uses_session_shell_when_environment_shell_is_absent() {
    let (mut session, mut turn_context) = make_session_and_context().await;
    session.services.user_shell = Arc::new(crate::shell::Shell {
        shell_type: crate::shell::ShellType::PowerShell,
        shell_path: PathBuf::from("powershell"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    });
    for environment in &mut turn_context.environments.turn_environments {
        environment.shell = None;
    }

    let session_shell = session.user_shell();
    let environment_context = crate::context::EnvironmentContext::from_turn_context(
        &turn_context,
        session_shell.as_ref(),
    )
    .render();
    assert!(
        environment_context.contains("<shell>powershell</shell>"),
        "{environment_context}"
    );

    let primary_environment = turn_context
        .environments
        .turn_environments
        .first_mut()
        .expect("primary environment");
    primary_environment.shell = Some("cmd".to_string());

    let environment_context = crate::context::EnvironmentContext::from_turn_context(
        &turn_context,
        session_shell.as_ref(),
    )
    .render();
    assert!(
        environment_context.contains("<shell>cmd</shell>"),
        "{environment_context}"
    );
}

#[tokio::test]
async fn build_settings_update_items_emits_environment_item_for_time_changes() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.current_date = Some("2026-02-27".to_string());
    current_context.timezone = Some("Europe/Berlin".to_string());

    let reference_context_item = previous_context.to_turn_context_item();
    let update_items = session
        .build_settings_update_items(Some(&reference_context_item), &current_context)
        .await;

    let environment_update = user_input_texts(&update_items)
        .into_iter()
        .find(|text| text.contains("<environment_context>"))
        .expect("environment update item should be emitted");
    assert!(environment_update.contains("<current_date>2026-02-27</current_date>"));
    assert!(environment_update.contains("<timezone>Europe/Berlin</timezone>"));
}

#[tokio::test]
async fn build_settings_update_items_omits_environment_item_when_disabled() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    let mut config = (*current_context.config).clone();
    config.include_environment_context = false;
    current_context.config = Arc::new(config);
    current_context.current_date = Some("2026-02-27".to_string());

    let reference_context_item = previous_context.to_turn_context_item();
    let update_items = session
        .build_settings_update_items(Some(&reference_context_item), &current_context)
        .await;

    let user_texts = user_input_texts(&update_items);
    assert!(
        !user_texts
            .iter()
            .any(|text| text.contains("<environment_context>")),
        "did not expect environment context updates when disabled, got {user_texts:?}"
    );
}

#[tokio::test]
async fn build_settings_update_items_emits_realtime_start_when_session_becomes_live() {
    let (session, previous_context) = make_session_and_context().await;
    let previous_context = Arc::new(previous_context);
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.realtime_active = true;

    let update_items = session
        .build_settings_update_items(
            Some(&previous_context.to_turn_context_item()),
            &current_context,
        )
        .await;

    let developer_texts = developer_input_texts(&update_items);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("<realtime_conversation>")),
        "expected a realtime start update, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_settings_update_items_emits_realtime_end_when_session_stops_being_live() {
    let (session, mut previous_context) = make_session_and_context().await;
    previous_context.realtime_active = true;
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.realtime_active = false;

    let update_items = session
        .build_settings_update_items(
            Some(&previous_context.to_turn_context_item()),
            &current_context,
        )
        .await;

    let developer_texts = developer_input_texts(&update_items);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("Reason: inactive")),
        "expected a realtime end update, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_settings_update_items_uses_previous_turn_settings_for_realtime_end() {
    let (session, previous_context) = make_session_and_context().await;
    let mut previous_context_item = previous_context.to_turn_context_item();
    previous_context_item.realtime_active = None;
    let previous_turn_settings = PreviousTurnSettings {
        model: previous_context.model_info.slug.clone(),
        realtime_active: Some(true),
    };
    let mut current_context = previous_context
        .with_model(
            previous_context.model_info.slug.clone(),
            &session.services.models_manager,
        )
        .await;
    current_context.realtime_active = false;

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let update_items = session
        .build_settings_update_items(Some(&previous_context_item), &current_context)
        .await;

    let developer_texts = developer_input_texts(&update_items);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("Reason: inactive")),
        "expected a realtime end update from previous turn settings, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_uses_previous_realtime_state() {
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.realtime_active = true;

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("<realtime_conversation>")),
        "expected initial context to describe active realtime state, got {developer_texts:?}"
    );

    let previous_context_item = turn_context.to_turn_context_item();
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(Some(previous_context_item));
    }
    let resumed_context = session.build_initial_context(&turn_context).await;
    let resumed_developer_texts = developer_input_texts(&resumed_context);
    assert!(
        !resumed_developer_texts
            .iter()
            .any(|text| text.contains("<realtime_conversation>")),
        "did not expect a duplicate realtime update, got {resumed_developer_texts:?}"
    );
}

async fn make_multi_agent_v2_usage_hint_test_session(
    enable_multi_agent_v2: bool,
) -> (Arc<Session>, Arc<TurnContext>) {
    let (session, turn_context, _rx_event) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            if enable_multi_agent_v2 {
                let _ = config.features.enable(Feature::MultiAgentV2);
            }
            config.multi_agent_v2.root_agent_usage_hint_text = Some("Root guidance.".to_string());
            config.multi_agent_v2.subagent_usage_hint_text = Some("Subagent guidance.".to_string());
        },
    )
    .await;
    (session, turn_context)
}

struct PromptExtensionTestContributor;
struct PromptExtensionTestState;

impl codex_extension_api::ContextContributor for PromptExtensionTestContributor {
    fn contribute<'a>(
        &'a self,
        _session_store: &'a codex_extension_api::ExtensionData,
        thread_store: &'a codex_extension_api::ExtensionData,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Vec<codex_extension_api::PromptFragment>> + Send + 'a>,
    > {
        Box::pin(async move {
            thread_store
                .get::<PromptExtensionTestState>()
                .is_some()
                .then(|| {
                    codex_extension_api::PromptFragment::developer_policy(
                        "prompt extension enabled",
                    )
                })
                .into_iter()
                .collect()
        })
    }
}

fn prompt_extension_test_registry()
-> Arc<codex_extension_api::ExtensionRegistry<crate::config::Config>> {
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::new();
    builder.prompt_contributor(Arc::new(PromptExtensionTestContributor));
    Arc::new(builder.build())
}

#[tokio::test]
async fn build_initial_context_includes_prompt_fragments_from_extensions() {
    let (mut session, turn_context) = make_session_and_context().await;
    session.services.extensions = prompt_extension_test_registry();
    session
        .services
        .thread_extension_data
        .insert(PromptExtensionTestState);

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_messages = developer_message_texts(&initial_context);

    assert!(
        developer_messages
            .iter()
            .flatten()
            .any(|text| *text == "prompt extension enabled"),
        "expected prompt extension developer text, got {developer_messages:?}"
    );
}

#[tokio::test]
async fn build_initial_context_omits_prompt_fragments_without_extension_state() {
    let (mut session, turn_context) = make_session_and_context().await;
    session.services.extensions = prompt_extension_test_registry();

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_messages = developer_message_texts(&initial_context);

    assert!(
        !developer_messages
            .iter()
            .flatten()
            .any(|text| *text == "prompt extension enabled"),
        "did not expect prompt extension developer text, got {developer_messages:?}"
    );
}

#[tokio::test]
async fn build_initial_context_adds_multi_agent_v2_root_usage_hint_as_developer_message() {
    let (session, turn_context) =
        make_multi_agent_v2_usage_hint_test_session(/*enable_multi_agent_v2*/ true).await;

    let initial_context = session.build_initial_context(turn_context.as_ref()).await;

    let developer_messages = developer_message_texts(&initial_context);
    assert!(
        developer_messages
            .iter()
            .any(|message| message.as_slice() == ["Root guidance."]),
        "expected standalone root usage hint developer message, got {developer_messages:?}"
    );
    assert!(
        !developer_messages
            .iter()
            .any(|message| message.as_slice() == ["Subagent guidance."]),
        "did not expect subagent usage hint for root thread, got {developer_messages:?}"
    );
}

#[tokio::test]
async fn build_initial_context_adds_multi_agent_v2_subagent_usage_hint_as_developer_message() {
    let (session, mut turn_context) =
        make_multi_agent_v2_usage_hint_test_session(/*enable_multi_agent_v2*/ true).await;
    let session_source = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: ThreadId::new(),
        depth: 1,
        agent_path: Some(AgentPath::try_from("/root/worker").expect("agent path should parse")),
        agent_nickname: Some("worker".to_string()),
        agent_role: None,
    });
    session
        .state
        .lock()
        .await
        .session_configuration
        .session_source = session_source.clone();
    Arc::get_mut(&mut turn_context)
        .expect("turn context should not be shared")
        .session_source = session_source;

    let initial_context = session.build_initial_context(turn_context.as_ref()).await;

    let developer_messages = developer_message_texts(&initial_context);
    assert!(
        developer_messages
            .iter()
            .any(|message| message.as_slice() == ["Subagent guidance."]),
        "expected standalone subagent usage hint developer message, got {developer_messages:?}"
    );
    assert!(
        !developer_messages
            .iter()
            .any(|message| message.as_slice() == ["Root guidance."]),
        "did not expect root usage hint for subagent thread, got {developer_messages:?}"
    );
}

#[tokio::test]
async fn build_initial_context_omits_multi_agent_v2_usage_hints_when_feature_disabled() {
    let (session, turn_context) =
        make_multi_agent_v2_usage_hint_test_session(/*enable_multi_agent_v2*/ false).await;

    let initial_context = session.build_initial_context(turn_context.as_ref()).await;

    let developer_messages = developer_message_texts(&initial_context);
    assert!(
        !developer_messages.iter().any(|message| {
            matches!(
                message.as_slice(),
                ["Root guidance."] | ["Subagent guidance."]
            )
        }),
        "did not expect multi-agent v2 usage hint developer messages, got {developer_messages:?}"
    );
}

#[tokio::test]
async fn configured_multi_agent_v2_usage_hint_texts_use_effective_enabled_feature_state() {
    let (mut session, _turn_context) =
        make_multi_agent_v2_usage_hint_test_session(/*enable_multi_agent_v2*/ false).await;
    let mut effective_features = Features::with_defaults();
    effective_features.enable(Feature::MultiAgentV2);
    Arc::get_mut(&mut session)
        .expect("session should not be shared")
        .features = effective_features.into();

    let hint_texts = session.configured_multi_agent_v2_usage_hint_texts().await;

    assert_eq!(
        hint_texts,
        vec![
            "Root guidance.".to_string(),
            "Subagent guidance.".to_string()
        ]
    );
}

#[tokio::test]
async fn configured_multi_agent_v2_usage_hint_texts_omit_effectively_disabled_feature() {
    let (mut session, _turn_context) =
        make_multi_agent_v2_usage_hint_test_session(/*enable_multi_agent_v2*/ true).await;
    Arc::get_mut(&mut session)
        .expect("session should not be shared")
        .features = Features::with_defaults().into();

    let hint_texts = session.configured_multi_agent_v2_usage_hint_texts().await;

    assert_eq!(hint_texts, Vec::<String>::new());
}

#[tokio::test]
async fn build_initial_context_omits_default_image_save_location_with_image_history() {
    let (session, turn_context) = make_session_and_context().await;
    session
        .replace_history(
            vec![ResponseItem::ImageGenerationCall {
                id: "ig-test".to_string(),
                status: "completed".to_string(),
                revised_prompt: Some("a tiny blue square".to_string()),
                result: "Zm9v".to_string(),
            }],
            /*reference_context_item*/ None,
        )
        .await;

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Generated images are saved to")),
        "expected initial context to omit image save instructions even with image history, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_omits_default_image_save_location_without_image_history() {
    let (session, turn_context) = make_session_and_context().await;

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);

    assert!(
        !developer_texts
            .iter()
            .any(|text| text.contains("Generated images are saved to")),
        "expected initial context to omit image save instructions without image history, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_trims_skill_metadata_from_context_window_budget() {
    let (session, mut turn_context) = make_session_and_context().await;
    let mut outcome = SkillLoadOutcome::default();
    outcome.skills = vec![
        SkillMetadata {
            name: "admin-skill".to_string(),
            description: "desc".to_string(),
            short_description: None,
            interface: None,
            dependencies: None,
            policy: None,
            path_to_skills_md: test_path_buf("/tmp/admin-skill/SKILL.md").abs(),
            scope: SkillScope::Admin,
            plugin_id: None,
        },
        SkillMetadata {
            name: "repo-skill".to_string(),
            description: "desc".to_string(),
            short_description: None,
            interface: None,
            dependencies: None,
            policy: None,
            path_to_skills_md: test_path_buf("/tmp/repo-skill/SKILL.md").abs(),
            scope: SkillScope::Repo,
            plugin_id: None,
        },
    ];
    turn_context.model_info.context_window = Some(100);
    turn_context.turn_skills = TurnSkillsContext::new(Arc::new(outcome));

    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);

    assert!(
        developer_texts
            .iter()
            .all(|text| !text.contains("Exceeded skills context budget")),
        "expected skill budget warning to stay out of the initial context, got {developer_texts:?}"
    );
    assert!(
        developer_texts
            .iter()
            .all(|text| !text.contains("- admin-skill:") && !text.contains("- repo-skill:")),
        "expected no skill metadata entries to fit the tiny budget, got {developer_texts:?}"
    );
}

#[test]
fn emit_thread_start_skill_metrics_records_enabled_kept_and_truncated_values() {
    let session_telemetry = test_session_telemetry_without_metadata();
    let mut outcome = SkillLoadOutcome::default();
    outcome.skills = vec![SkillMetadata {
        name: "repo-skill".to_string(),
        description: "desc".to_string(),
        short_description: None,
        interface: None,
        dependencies: None,
        policy: None,
        path_to_skills_md: test_path_buf("/tmp/repo-skill/SKILL.md").abs(),
        scope: SkillScope::Repo,
        plugin_id: None,
    }];
    let rendered = build_available_skills(
        &outcome,
        SkillMetadataBudget::Characters(1),
        SkillRenderSideEffects::ThreadStart {
            session_telemetry: &session_telemetry,
        },
    )
    .expect("skills should render");

    assert_eq!(
        rendered.warning_message,
        Some(
            "Exceeded skills context budget. All skill descriptions were removed and 1 additional skill was not included in the model-visible skills list."
                .to_string()
        )
    );
    let snapshot = session_telemetry
        .snapshot_metrics()
        .expect("runtime metrics snapshot");
    assert_eq!(
        histogram_sum(&snapshot, THREAD_SKILLS_ENABLED_TOTAL_METRIC),
        1
    );
    assert_eq!(histogram_sum(&snapshot, THREAD_SKILLS_KEPT_TOTAL_METRIC), 0);
    assert_eq!(histogram_sum(&snapshot, THREAD_SKILLS_TRUNCATED_METRIC), 1);
    assert_eq!(
        histogram_sum(&snapshot, THREAD_SKILLS_DESCRIPTION_TRUNCATED_CHARS_METRIC),
        4
    );
}

#[test]
fn emit_thread_start_skill_metrics_records_description_truncated_chars_without_omitted_skills() {
    let session_telemetry = test_session_telemetry_without_metadata();
    let alpha = SkillMetadata {
        name: "alpha-skill".to_string(),
        description: "abcdef".to_string(),
        short_description: None,
        interface: None,
        dependencies: None,
        policy: None,
        path_to_skills_md: test_path_buf("/tmp/alpha-skill/SKILL.md").abs(),
        scope: SkillScope::Repo,
        plugin_id: None,
    };
    let beta = SkillMetadata {
        name: "beta-skill".to_string(),
        description: "uvwxyz".to_string(),
        short_description: None,
        interface: None,
        dependencies: None,
        policy: None,
        path_to_skills_md: test_path_buf("/tmp/beta-skill/SKILL.md").abs(),
        scope: SkillScope::Repo,
        plugin_id: None,
    };
    let minimum_skill_line_cost = |skill: &SkillMetadata| {
        let path = skill.path_to_skills_md.to_string_lossy().replace('\\', "/");
        format!("- {}: (file: {})\n", skill.name, path)
            .chars()
            .count()
    };
    let minimum_budget = minimum_skill_line_cost(&alpha) + minimum_skill_line_cost(&beta);
    let mut outcome = SkillLoadOutcome::default();
    outcome.skills = vec![alpha, beta];

    let rendered = build_available_skills(
        &outcome,
        SkillMetadataBudget::Characters(minimum_budget + 6),
        SkillRenderSideEffects::ThreadStart {
            session_telemetry: &session_telemetry,
        },
    )
    .expect("skills should render");

    assert_eq!(rendered.report.omitted_count, 0);
    assert_eq!(rendered.report.truncated_description_chars, 8);
    let snapshot = session_telemetry
        .snapshot_metrics()
        .expect("runtime metrics snapshot");
    assert_eq!(histogram_sum(&snapshot, THREAD_SKILLS_TRUNCATED_METRIC), 0);
    assert_eq!(
        histogram_sum(&snapshot, THREAD_SKILLS_DESCRIPTION_TRUNCATED_CHARS_METRIC),
        8
    );
}

#[tokio::test]
async fn build_initial_context_emits_thread_start_skill_warning_on_repeated_builds() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;
    let mut turn_context = Arc::into_inner(turn_context).expect("sole turn context owner");
    let mut outcome = SkillLoadOutcome::default();
    outcome.skills = vec![
        SkillMetadata {
            name: "admin-skill".to_string(),
            description: "desc".to_string(),
            short_description: None,
            interface: None,
            dependencies: None,
            policy: None,
            path_to_skills_md: test_path_buf("/tmp/admin-skill/SKILL.md").abs(),
            scope: SkillScope::Admin,
            plugin_id: None,
        },
        SkillMetadata {
            name: "repo-skill".to_string(),
            description: "desc".to_string(),
            short_description: None,
            interface: None,
            dependencies: None,
            policy: None,
            path_to_skills_md: test_path_buf("/tmp/repo-skill/SKILL.md").abs(),
            scope: SkillScope::Repo,
            plugin_id: None,
        },
    ];
    turn_context.model_info.context_window = Some(100);
    turn_context.turn_skills = TurnSkillsContext::new(Arc::new(outcome));

    let _ = session.build_initial_context(&turn_context).await;
    let warning_event = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("warning event should arrive")
        .expect("warning event should be readable");
    assert!(matches!(
        warning_event.msg,
        EventMsg::Warning(WarningEvent { message })
            if message == "Exceeded skills context budget of 2%. All skill descriptions were removed and 2 additional skills were not included in the model-visible skills list."
    ));

    let _ = session.build_initial_context(&turn_context).await;
    let warning_event = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("warning event should arrive on repeated build")
        .expect("warning event should be readable");
    assert!(matches!(
        warning_event.msg,
        EventMsg::Warning(WarningEvent { message })
            if message == "Exceeded skills context budget of 2%. All skill descriptions were removed and 2 additional skills were not included in the model-visible skills list."
    ));
}

#[tokio::test]
async fn handle_output_item_done_records_image_save_history_message() {
    let (session, turn_context) = make_session_and_context().await;
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "ig_history_records_message";
    let expected_saved_path = crate::stream_events_utils::image_generation_artifact_path(
        &turn_context.config.codex_home,
        &session.conversation_id.to_string(),
        call_id,
    );
    let _ = std::fs::remove_file(&expected_saved_path);
    let item = ResponseItem::ImageGenerationCall {
        id: call_id.to_string(),
        status: "completed".to_string(),
        revised_prompt: Some("a tiny blue square".to_string()),
        result: "Zm9v".to_string(),
    };

    let mut ctx = HandleOutputCtx {
        sess: Arc::clone(&session),
        turn_context: Arc::clone(&turn_context),
        turn_store: Arc::new(codex_extension_api::ExtensionData::new(
            turn_context.sub_id.clone(),
        )),
        tool_runtime: test_tool_runtime(Arc::clone(&session), Arc::clone(&turn_context)),
        cancellation_token: CancellationToken::new(),
    };
    handle_output_item_done(&mut ctx, item.clone(), /*previously_active_item*/ None)
        .await
        .expect("image generation item should succeed");

    let history = session.clone_history().await;
    let image_output_path = crate::stream_events_utils::image_generation_artifact_path(
        &turn_context.config.codex_home,
        &session.conversation_id.to_string(),
        "<image_id>",
    );
    let image_output_dir = image_output_path
        .parent()
        .expect("generated image path should have a parent");
    let image_message: ResponseItem = crate::context::ContextualUserFragment::into(
        crate::context::ImageGenerationInstructions::new(
            image_output_dir.display(),
            image_output_path.display(),
        ),
    );
    assert_eq!(history.raw_items(), &[image_message, item]);
    assert_eq!(
        std::fs::read(&expected_saved_path).expect("saved file"),
        b"foo"
    );
    let _ = std::fs::remove_file(&expected_saved_path);
}

#[tokio::test]
async fn handle_output_item_done_skips_image_save_message_when_save_fails() {
    let (session, turn_context) = make_session_and_context().await;
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    let call_id = "ig_history_no_message";
    let expected_saved_path = crate::stream_events_utils::image_generation_artifact_path(
        &turn_context.config.codex_home,
        &session.conversation_id.to_string(),
        call_id,
    );
    let _ = std::fs::remove_file(&expected_saved_path);
    let item = ResponseItem::ImageGenerationCall {
        id: call_id.to_string(),
        status: "completed".to_string(),
        revised_prompt: Some("broken payload".to_string()),
        result: "_-8".to_string(),
    };

    let mut ctx = HandleOutputCtx {
        sess: Arc::clone(&session),
        turn_context: Arc::clone(&turn_context),
        turn_store: Arc::new(codex_extension_api::ExtensionData::new(
            turn_context.sub_id.clone(),
        )),
        tool_runtime: test_tool_runtime(Arc::clone(&session), Arc::clone(&turn_context)),
        cancellation_token: CancellationToken::new(),
    };
    handle_output_item_done(&mut ctx, item.clone(), /*previously_active_item*/ None)
        .await
        .expect("image generation item should still complete");

    let history = session.clone_history().await;
    assert_eq!(history.raw_items(), &[item]);
    assert!(!expected_saved_path.exists());
}

#[tokio::test]
async fn build_initial_context_uses_previous_turn_settings_for_realtime_end() {
    let (session, turn_context) = make_session_and_context().await;
    let previous_turn_settings = PreviousTurnSettings {
        model: turn_context.model_info.slug.clone(),
        realtime_active: Some(true),
    };

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("Reason: inactive")),
        "expected initial context to describe an ended realtime session, got {developer_texts:?}"
    );
}

#[tokio::test]
async fn build_initial_context_restates_realtime_start_when_reference_context_is_missing() {
    let (session, mut turn_context) = make_session_and_context().await;
    turn_context.realtime_active = true;
    let previous_turn_settings = PreviousTurnSettings {
        model: turn_context.model_info.slug.clone(),
        realtime_active: Some(true),
    };

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let initial_context = session.build_initial_context(&turn_context).await;
    let developer_texts = developer_input_texts(&initial_context);
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains("<realtime_conversation>")),
        "expected initial context to restate active realtime when the reference context is missing, got {developer_texts:?}"
    );
}

fn file_system_policy_with_unreadable_glob(turn_context: &TurnContext) -> FileSystemSandboxPolicy {
    #[allow(deprecated)]
    let mut policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &turn_context.sandbox_policy(),
        &turn_context.cwd,
    );
    #[allow(deprecated)]
    let cwd_display = turn_context.cwd.as_path().display().to_string();
    policy.entries.push(FileSystemSandboxEntry {
        path: FileSystemPath::GlobPattern {
            pattern: format!("{cwd_display}/**/*.env"),
        },
        access: FileSystemAccessMode::None,
    });
    policy
}

#[tokio::test]
async fn turn_context_item_omits_legacy_equivalent_file_system_sandbox_policy() {
    let (_session, turn_context) = make_session_and_context().await;

    let item = turn_context.to_turn_context_item();

    assert_eq!(item.file_system_sandbox_policy, None);
    assert_eq!(
        item.permission_profile,
        Some(turn_context.permission_profile())
    );
}

#[tokio::test]
async fn turn_context_item_stores_split_file_system_sandbox_policy_when_different() {
    let (_session, mut turn_context) = make_session_and_context().await;
    let file_system_sandbox_policy = file_system_policy_with_unreadable_glob(&turn_context);
    turn_context.permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
        turn_context.permission_profile.enforcement(),
        &file_system_sandbox_policy,
        turn_context.network_sandbox_policy(),
    );

    let item = turn_context.to_turn_context_item();

    assert_eq!(
        item.file_system_sandbox_policy,
        Some(file_system_sandbox_policy)
    );
    assert_eq!(
        item.permission_profile,
        Some(turn_context.permission_profile())
    );
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_injects_full_context_when_baseline_missing()
 {
    let (session, turn_context) = make_session_and_context().await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record context updates");
    let history = session.clone_history().await;
    let initial_context = session.build_initial_context(&turn_context).await;
    assert_eq!(history.raw_items().to_vec(), initial_context);

    let current_context = session.reference_context_item().await;
    assert_eq!(
        serde_json::to_value(current_context).expect("serialize current context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected context item")
    );
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_reinjects_full_context_after_clear()
{
    let (session, turn_context) = make_session_and_context().await;
    let compacted_summary = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!("{}\nsummary", crate::compact::SUMMARY_PREFIX),
        }],
        phase: None,
    };
    session
        .record_into_history(std::slice::from_ref(&compacted_summary), &turn_context)
        .await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record context updates");
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(/*item*/ None);
    }
    session
        .replace_history(
            vec![compacted_summary.clone()],
            /*reference_context_item*/ None,
        )
        .await;

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record context updates");

    let history = session.clone_history().await;
    let mut expected_history = vec![compacted_summary];
    expected_history.extend(session.build_initial_context(&turn_context).await);
    assert_eq!(history.raw_items().to_vec(), expected_history);
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_persists_baseline_without_emitting_diffs()
 {
    let (mut session, previous_context) = make_session_and_context().await;
    let next_model = if previous_context.model_info.slug == "gpt-5.4" {
        "gpt-5.2"
    } else {
        "gpt-5.4"
    };
    let turn_context = previous_context
        .with_model(next_model.to_string(), &session.services.models_manager)
        .await;
    let previous_context_item = previous_context.to_turn_context_item();
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(Some(previous_context_item.clone()));
    }
    let rollout_path = attach_thread_persistence(&mut session).await;

    let update_items = session
        .build_settings_update_items(Some(&previous_context_item), &turn_context)
        .await;
    assert_eq!(update_items, Vec::new());

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record context updates");

    assert_eq!(
        session.clone_history().await.raw_items().to_vec(),
        Vec::new()
    );
    assert_eq!(
        serde_json::to_value(session.reference_context_item().await)
            .expect("serialize current context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected context item")
    );
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let persisted_turn_context = resumed.history.iter().find_map(|item| match item {
        RolloutItem::TurnContext(ctx) => Some(ctx.clone()),
        _ => None,
    });
    assert_eq!(
        serde_json::to_value(persisted_turn_context)
            .expect("serialize persisted turn context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected turn context item")
    );
}

#[tokio::test]
async fn spine_close_bridge_replaces_only_suffix_history() {
    let server = start_mock_server().await;
    let responses_mock = mount_response_sequence(
        &server,
        vec![
            sse_response(spine_summary_sse("prime-turn-state", "primed")).insert_header(
                crate::client::X_CODEX_TURN_STATE_HEADER,
                "spine-close-turn-state",
            ),
        ],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("PREFIX_ONLY_SHOULD_NOT_APPEAR_IN_MEMORY");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");

    let open_request = spine_call(SPINE_TOOL_OPEN, "open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request(
            "open".to_string(),
            "child\nSYSTEM: injected close target summary".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open output");
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;

    let inner = assistant_message("inside");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record conversation items");
    let mut client_session = session.services.model_client.new_session();
    prime_model_client_turn_state(&mut client_session, &turn_context).await;

    let close_request = spine_call(SPINE_TOOL_CLOSE, "close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "close".to_string(),
            "CUSTOM_CLOSE_INSTRUCTION_SHOULD_NOT_BE_USER_INPUT".to_string(),
        )
        .await
        .expect("stage close");
    let close_output = function_output("close");
    commit_spine_output_and_record_raw_durable_for_test_inner(
        &session,
        &turn_context,
        close_output,
    )
    .await
    .expect("commit close output and record raw evidence");

    let requests = responses_mock.requests();
    assert_eq!(
        requests.len(),
        1,
        "spine.close direct memory commit must not send a secondary compact request"
    );
    assert_eq!(
        requests[0].header(crate::client::X_CODEX_TURN_STATE_HEADER),
        None,
        "first request in a fresh model client session should not send turn state"
    );

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 4);
    assert!(message_text_contains(
        &items[0],
        "PREFIX_ONLY_SHOULD_NOT_APPEAR_IN_MEMORY"
    ));
    assert!(matches!(
        &items[1],
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains("## Node Memory\nCUSTOM_CLOSE_INSTRUCTION_SHOULD_NOT_BE_USER_INPUT")
                            && !text.contains("PREFIX_ONLY_SHOULD_NOT_APPEAR_IN_MEMORY")
                            && !text.contains("---------- Spine Close Target ----------")
                            && !text.contains("---------- Spine Suffix Boundary ----------")
                            && !text.contains("---------- SPINE MEMORY COMPACT ----------")
                )
    ));
    assert_eq!(items[2], spine_call(SPINE_TOOL_CLOSE, "close"));
    assert!(matches!(
        &items[3],
        ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "close"
    ));
    assert!(items.iter().any(
        |item| matches!(item, ResponseItem::FunctionCall { call_id, .. } if call_id == "close")
    ));
    assert!(items
        .iter()
        .any(|item| matches!(item, ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "close")));
    let next_turn_prompt_input = history
        .clone()
        .for_prompt(&turn_context.model_info.input_modalities);
    assert!(next_turn_prompt_input.iter().any(
        |item| matches!(item, ResponseItem::FunctionCall { call_id, .. } if call_id == "close")
    ));
    assert!(next_turn_prompt_input.iter().any(
        |item| matches!(item, ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "close")
    ));
    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let (_, raw_end, context_start, context_end) = SpineStore::for_rollout(&rollout_path)
        .expect("load spine store")
        .suffix_mem_cover_for_test("1.1.1")
        .expect("read spine mem records")
        .expect("closed child suffix memory should be recorded");
    assert_eq!(context_start, 1);
    assert_eq!(
        context_end, 4,
        "suffix memory context evidence must stop before close request/output carriers"
    );
    assert_eq!(
        raw_end, 4,
        "suffix memory raw evidence must stop before close request/output carriers"
    );
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        items
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_close_direct_memory_keeps_prefix_image_provenance() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse("spine-close-image-summary", "image prefix compact summary"),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, mut turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    Arc::get_mut(&mut turn_context)
        .expect("turn context should be unique")
        .model_info
        .input_modalities = vec![InputModality::Text];
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let image_url = "data:image/png;base64,SPINE_RAW_IMAGE_URL_SENTINEL_42";
    let prefix = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "prefix multimodal user text before close".to_string(),
            },
            ContentItem::InputImage {
                image_url: image_url.to_string(),
                detail: Some(ImageDetail::High),
            },
        ],
        phase: None,
    };
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "image-prefix-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "image-prefix-open".to_string(),
            "image prefix child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("image-prefix-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open output");

    let inner = assistant_message("assistant suffix requiring generated node memory");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record suffix evidence");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "image-prefix-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "image-prefix-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("image-prefix-close"),
    )
    .await
    .expect("commit close output and record raw evidence");

    assert_eq!(
        compact_mock.requests().len(),
        0,
        "direct memory close must not build a secondary text-only compact prompt"
    );

    let history = session.clone_history().await;
    assert!(
        history.raw_items().iter().any(|item| matches!(
            item,
            ResponseItem::Message { content, .. }
                if content.iter().any(|content| matches!(
                    content,
                    ContentItem::InputImage { image_url: existing, .. } if existing == image_url
                ))
        )),
        "raw/materialized history should keep provenance image data; only the model prompt is normalized"
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_jit_user_append_publishes_anchor_to_live_history() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    session
        .record_conversation_items(&turn_context, &[user_message("anchored live user")])
        .await
        .expect("record user");

    let history = session.clone_history().await;
    assert!(matches!(
        history.raw_items(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }] if text == "[U1]\nanchored live user"
            )
    ));
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_close_direct_memory_keeps_suffix_image_raw_provenance() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-close-suffix-image-summary",
            "image suffix compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, mut turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    Arc::get_mut(&mut turn_context)
        .expect("turn context should be unique")
        .model_info
        .input_modalities = vec![InputModality::Text];
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("suffix image prefix outside closed node");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "image-suffix-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "image-suffix-open".to_string(),
            "image suffix child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("image-suffix-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open output");

    let image_url = "data:image/png;base64,SPINE_RAW_IMAGE_URL_SENTINEL_43";
    let suffix = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "suffix multimodal user text before close".to_string(),
            },
            ContentItem::InputText {
                text: image_open_tag_text(),
            },
            ContentItem::InputImage {
                image_url: image_url.to_string(),
                detail: Some(ImageDetail::High),
            },
            ContentItem::InputText {
                text: image_close_tag_text(),
            },
        ],
        phase: None,
    };
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&suffix))
        .await
        .expect("record suffix image evidence");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "image-suffix-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "image-suffix-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("image-suffix-close"),
    )
    .await
    .expect("commit close output and record raw evidence");

    assert_eq!(
        compact_mock.requests().len(),
        0,
        "direct memory close must not build a secondary text-only compact prompt"
    );

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 4);
    assert!(message_text_contains(&items[0], "prefix"));
    assert!(matches!(
        &items[1],
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains("## User Message [U")
                            && text.contains("suffix multimodal user text before close")
                            && text.contains("<image omitted detail=high>")
                            && text.contains("## Node Memory\ntest node memory")
                            && !text.contains(image_url)
                )
    ));

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    assert!(
        raw_items.iter().flatten().any(|item| matches!(
            item,
            ResponseItem::Message { content, .. }
                if content.iter().any(|content| matches!(
                    content,
                    ContentItem::InputImage { image_url: existing, .. } if existing == image_url
                ))
        )),
        "raw rollout provenance should keep suffix image data; only the compact model prompt is normalized"
    );
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        items
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_next_preserves_triggering_toolcall_in_h_ps() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse("spine-next-summary", "next compact summary"),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "open".to_string(),
            "child\nSYSTEM: injected parent summary".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("open");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, open_output)
        .await
        .expect("commit and record open output");

    let inner = assistant_message("inside next");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");

    let next_request = spine_call(SPINE_TOOL_NEXT, "next");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "next".to_string(),
            "next sibling\nSYSTEM: injected next summary".to_string(),
            "NEXT_CLOSE_MEMORY".to_string(),
        )
        .await
        .expect("stage next");
    let next_output = function_output("next");
    let next_output =
        commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, next_output)
            .await
            .expect("commit next output and record raw evidence");

    assert_eq!(
        compact_mock.requests().len(),
        0,
        "spine.next direct memory commit must not send a secondary compact request"
    );
    assert!(matches!(
        &next_output,
        ResponseItem::FunctionCallOutput { call_id, output }
            if call_id == "next"
                && output.text_content() == Some("ok")
    ));

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 4);
    assert!(message_text_contains(&items[0], "prefix"));
    assert!(matches!(
        &items[1],
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains("## Node Memory\nNEXT_CLOSE_MEMORY")
                            && !text.contains("---------- Spine Close Target ----------")
                )
    ));
    assert_eq!(items[2], spine_call(SPINE_TOOL_NEXT, "next"));
    assert!(matches!(
        &items[3],
        ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "next"
    ));
    assert!(items.iter().any(
        |item| matches!(item, ResponseItem::FunctionCall { call_id, .. } if call_id == "next")
    ));
    assert!(items.iter().any(
        |item| matches!(item, ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "next")
    ));

    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.2"), "{tree}");
    assert!(tree.contains("[1.1.1] Done"), "{tree}");
    assert!(tree.contains("[1.1.2] Current next sibling"), "{tree}");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        items
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_next_resume_restores_closed_and_current_sibling() {
    let fixture = make_spine_session_after_next("post-next resume summary").await;

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = fixture
        .raw_items
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&fixture.rollout_path, &resumed_rollout_path, &raw_live);

    resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: fixture.rollout_items.clone(),
            rollout_path: Some(fixture.rollout_path.clone()),
        }))
        .await
        .expect("resume post-next rollout");

    assert_post_next_session_state(
        &resumed_session,
        &resumed_rollout_path,
        &fixture.raw_items,
        &fixture.expected_history,
    )
    .await;
}

#[tokio::test]
async fn spine_next_fork_restores_closed_and_current_sibling() {
    let fixture = make_spine_session_after_next("post-next fork summary").await;
    let source_events_before = SpineStore::for_rollout(&fixture.rollout_path)
        .expect("load source spine store")
        .event_count_for_test()
        .expect("source event count before fork");

    let (mut forked_session, _forked_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let forked_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut forked_session).expect("session should be unique"),
    )
    .await;
    let raw_live = fixture
        .raw_items
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&fixture.rollout_path, &forked_rollout_path, &raw_live);

    forked_session
        .record_initial_history(InitialHistory::Forked(fixture.rollout_items.clone()))
        .await
        .expect("fork post-next rollout");

    assert_post_next_session_state(
        &forked_session,
        &forked_rollout_path,
        &fixture.raw_items,
        &fixture.expected_history,
    )
    .await;
    let source_events_after = SpineStore::for_rollout(&fixture.rollout_path)
        .expect("reload source spine store")
        .event_count_for_test()
        .expect("source event count after fork");
    assert_eq!(
        source_events_after, source_events_before,
        "fork replay must not mutate the source Spine sidecar"
    );
}

#[tokio::test]
async fn spine_next_rollback_preserves_closed_sibling_and_drops_new_live_turn() {
    let fixture = make_spine_session_after_next("post-next rollback summary").await;
    while fixture.rx.try_recv().is_ok() {}

    let rolled_back_user = user_message("post-next live user that rollback drops");
    let rolled_back_assistant = assistant_message("post-next live assistant that rollback drops");
    fixture
        .session
        .record_conversation_items(
            &fixture.turn_context,
            &[rolled_back_user.clone(), rolled_back_assistant.clone()],
        )
        .await
        .expect("record post-next live turn");
    assert!(
        fixture
            .session
            .clone_history()
            .await
            .raw_items()
            .iter()
            .any(|item| message_text_contains(item, "post-next live user that rollback drops")),
        "test setup should append a live turn after spine.next"
    );

    handlers::thread_rollback(
        &fixture.session,
        "post-next-rollback".to_string(),
        /*num_turns*/ 1,
    )
    .await;
    let rollback_event = wait_for_thread_rolled_back(&fixture.rx).await;
    assert_eq!(rollback_event.num_turns, 1);

    let history = fixture.session.clone_history().await;
    assert_eq!(history.raw_items(), fixture.expected_history.as_slice());
    assert!(
        !history
            .raw_items()
            .iter()
            .any(|item| message_text_contains(item, "post-next live user that rollback drops"))
    );
    assert!(!history.raw_items().contains(&rolled_back_assistant));

    fixture.session.ensure_rollout_materialized().await;
    fixture
        .session
        .flush_rollout()
        .await
        .expect("rollout should flush");
    let InitialHistory::Resumed(resumed) =
        RolloutRecorder::get_rollout_history(&fixture.rollout_path)
            .await
            .expect("read post-next rollback rollout")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&fixture.rollout_path, &raw_items, &[])
        .expect("load post-next rollback runtime")
        .expect("post-next rollback sidecar should exist");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize post-next rollback h(PS)"),
        fixture.expected_history
    );
    assert_post_next_tree(&runtime.render_tree().expect("render rollback tree"));
}

#[tokio::test]
async fn resume_rejects_committed_close_when_output_carrier_missing() {
    let fixture =
        make_spine_close_window_missing_output_carrier("close-window resume summary").await;

    let err = SpineRuntime::load_for_rollout_items(&fixture.rollout_path, &fixture.raw_items, &[])
        .expect_err("resume with missing close output carrier must fail closed");
    assert!(
        err.to_string().contains("raw-backed event at token_seq"),
        "unexpected missing carrier resume error: {err}"
    );
}

#[tokio::test]
async fn spine_close_fork_rejects_missing_output_carrier() {
    let fixture = make_spine_close_window_missing_output_carrier("close-window fork summary").await;
    let source_events_before = SpineStore::for_rollout(&fixture.rollout_path)
        .expect("load source spine store")
        .event_count_for_test()
        .expect("source event count before fork");

    let (mut forked_session, _forked_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let forked_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut forked_session).expect("session should be unique"),
    )
    .await;
    let raw_live = fixture
        .raw_items
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    let boundary = SpineStore::clone_boundary_for_rollout(
        &fixture.rollout_path,
        u64::try_from(raw_live.len()).expect("raw live len"),
    )
    .expect("capture clone boundary")
    .expect("source sidecar exists");
    let err =
        SpineStore::clone_for_rollout_with_raw_live(&boundary, &forked_rollout_path, &raw_live)
            .expect_err("fork clone with missing close output carrier must fail closed");
    assert!(
        err.to_string().contains("clone raw live state"),
        "unexpected missing carrier fork error: {err}"
    );
    let source_events_after = SpineStore::for_rollout(&fixture.rollout_path)
        .expect("reload source spine store")
        .event_count_for_test()
        .expect("source event count after fork");
    assert_eq!(
        source_events_after, source_events_before,
        "fork replay must not mutate the source Spine sidecar"
    );
}

#[tokio::test]
async fn spine_next_resume_rejects_missing_output_carrier() {
    let fixture = make_spine_next_window_missing_output_carrier("next-window resume summary").await;

    let err = SpineRuntime::load_for_rollout_items(&fixture.rollout_path, &fixture.raw_items, &[])
        .expect_err("resume with missing next output carrier must fail closed");
    assert!(
        err.to_string().contains("raw-backed event at token_seq"),
        "unexpected missing carrier resume error: {err}"
    );
}

#[tokio::test]
async fn resume_committed_sidecar_overrides_stale_host_history() {
    assert_resume_committed_sidecar_overrides_stale_host_history().await;
}

#[tokio::test]
async fn resume_replays_nested_open_close_tree() {
    assert_resume_replays_nested_open_close_tree().await;
}

#[tokio::test]
async fn resume_corrupt_checkpoint_hash_fails_closed() {
    let (_source_session, _source_turn_context, source_rollout_path, rollout_items) =
        make_spine_session_with_closed_child("corrupt resume checkpoint").await;
    let raw_items = spine_raw_items_after_rollback(&rollout_items);
    let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    clone_spine_sidecar_for_test(&source_rollout_path, &resumed_rollout_path, &raw_live);
    let corrupted_ordinal = SpineStore::for_rollout(&resumed_rollout_path)
        .expect("resumed spine store")
        .corrupt_latest_resume_checkpoint_h_ps_hash_for_test(raw_items.len())
        .expect("corrupt latest resume checkpoint");
    assert!(
        corrupted_ordinal <= u64::try_from(raw_items.len()).expect("raw item count"),
        "corrupted checkpoint must be applicable to the resumed raw boundary"
    );

    let err = resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(source_rollout_path),
        }))
        .await
        .expect_err("corrupt ordinary resume checkpoint must fail closed");
    assert!(
        err.to_string()
            .contains("spine checkpoint h(PS) hash mismatch"),
        "unexpected resume error: {err}"
    );
}

async fn assert_resume_committed_sidecar_overrides_stale_host_history() {
    let (_source_session, _source_turn_context, source_rollout_path, rollout_items) =
        make_spine_session_with_closed_child("close-window2 resume summary").await;
    let raw_items = spine_raw_items_after_rollback(&rollout_items);
    assert!(
        raw_items.iter().flatten().any(|item| matches!(
            item,
            ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "resume-close"
        )),
        "test setup must include durable close output carrier"
    );
    let runtime = SpineRuntime::load_for_rollout_items(&source_rollout_path, &raw_items, &[])
        .expect("load close-window2 runtime")
        .expect("close-window2 sidecar should exist");
    let expected_history = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize close-window2 h(PS)");
    assert!(!expected_history.iter().any(
        |item| matches!(item, ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "resume-close")
    ));

    assert_resumed_close_window_matches_sidecar(
        source_rollout_path,
        rollout_items,
        raw_items,
        expected_history,
    )
    .await;
}

async fn assert_resume_replays_nested_open_close_tree() {
    let (source_session, source_turn_context, source_rollout_path, _rollout_items) =
        make_spine_session_with_closed_child("nested resume summary").await;
    let parent_suffix = user_message("parent suffix after closed child");
    source_session
        .record_conversation_items(&source_turn_context, std::slice::from_ref(&parent_suffix))
        .await
        .expect("record parent suffix after closed child");
    source_session.ensure_rollout_materialized().await;
    source_session
        .flush_rollout()
        .await
        .expect("rollout with parent suffix should flush");
    let InitialHistory::Resumed(resumed) =
        RolloutRecorder::get_rollout_history(&source_rollout_path)
            .await
            .expect("read rollout history with parent suffix")
    else {
        panic!("expected resumed rollout history with parent suffix");
    };
    let rollout_items = resumed.history;
    let raw_items = spine_raw_items_after_rollback(&rollout_items);
    assert!(
        raw_items
            .iter()
            .flatten()
            .any(|item| item == &parent_suffix),
        "test setup must include a durable parent suffix after child close"
    );
    let runtime = SpineRuntime::load_for_rollout_items(&source_rollout_path, &raw_items, &[])
        .expect("load nested resume runtime")
        .expect("nested resume sidecar should exist");
    let expected_history = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize nested resume h(PS)");
    assert!(
        expected_history.iter().any(|item| matches!(
            item,
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                )
        )),
        "h(PS) should include the closed child memory"
    );
    assert!(
        expected_history.contains(&parent_suffix),
        "h(PS) should preserve the current parent suffix after the closed child"
    );

    assert_resumed_close_window_matches_sidecar(
        source_rollout_path,
        rollout_items,
        raw_items,
        expected_history,
    )
    .await;
}

async fn assert_resumed_close_window_matches_sidecar(
    source_rollout_path: PathBuf,
    rollout_items: Vec<RolloutItem>,
    raw_items: Vec<Option<ResponseItem>>,
    expected_history: Vec<ResponseItem>,
) {
    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&source_rollout_path, &resumed_rollout_path, &raw_live);

    resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(source_rollout_path),
        }))
        .await
        .expect("resume close-window2 rollout");

    assert_close_window_session_state(
        &resumed_session,
        &resumed_rollout_path,
        &raw_items,
        &expected_history,
    )
    .await;
}

#[tokio::test]
async fn resume_base_reconstruction_metadata_survives_h_ps_override() {
    assert_resume_base_reconstruction_metadata_survives_h_ps_override().await;
}

#[tokio::test]
async fn resume_restores_context_manager_items_from_h_ps() {
    assert_resume_base_reconstruction_metadata_survives_h_ps_override().await;
}

async fn assert_resume_base_reconstruction_metadata_survives_h_ps_override() {
    let (_source_session, source_turn_context, source_rollout_path, mut rollout_items) =
        make_spine_session_with_closed_child("metadata survives sidecar override").await;
    let raw_items = spine_raw_items_after_rollback(&rollout_items);
    let runtime = SpineRuntime::load_for_rollout_items(&source_rollout_path, &raw_items, &[])
        .expect("load source runtime")
        .expect("source sidecar should exist");
    let expected_history = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("source h(PS)");
    assert_ne!(
        rollout_items
            .iter()
            .filter_map(|item| match item {
                RolloutItem::ResponseItem(item) => Some(item),
                _ => None,
            })
            .collect::<Vec<_>>(),
        expected_history.iter().collect::<Vec<_>>(),
        "base rollout reconstruction must differ so sidecar h(PS) is the final authority"
    );

    let mut previous_context_item = source_turn_context.to_turn_context_item();
    previous_context_item.model = "metadata-survives-model".to_string();
    let metadata_turn_id = previous_context_item
        .turn_id
        .clone()
        .expect("turn context item should have a turn id");
    rollout_items.push(RolloutItem::EventMsg(EventMsg::TurnStarted(
        TurnStartedEvent {
            turn_id: metadata_turn_id.clone(),
            started_at: None,
            model_context_window: Some(128_000),
            collaboration_mode_kind: ModeKind::Default,
        },
    )));
    rollout_items.push(RolloutItem::EventMsg(EventMsg::UserMessage(
        UserMessageEvent {
            message: "metadata-only resume turn".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
        },
    )));
    rollout_items.push(RolloutItem::TurnContext(previous_context_item.clone()));
    rollout_items.push(RolloutItem::EventMsg(EventMsg::TurnComplete(
        TurnCompleteEvent {
            turn_id: metadata_turn_id,
            last_agent_message: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        },
    )));

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&source_rollout_path, &resumed_rollout_path, &raw_live);

    resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(source_rollout_path),
        }))
        .await
        .expect("resume with sidecar h(PS) override");

    assert_eq!(
        resumed_session.clone_history().await.raw_items(),
        expected_history.as_slice()
    );
    assert_eq!(
        resumed_session.previous_turn_settings().await,
        Some(PreviousTurnSettings {
            model: previous_context_item.model.clone(),
            realtime_active: previous_context_item.realtime_active,
        })
    );
    assert_eq!(
        serde_json::to_value(resumed_session.reference_context_item().await)
            .expect("serialize resumed reference context"),
        serde_json::to_value(Some(previous_context_item))
            .expect("serialize expected reference context")
    );
}

#[tokio::test]
async fn close_commit_is_atomic_across_sidecar_and_history() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-close-raw-output-failure",
            "close failure compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path = attach_thread_persistence_with_raw_output_append_failure(
        Arc::get_mut(&mut session).expect("session should be unique"),
        "close-raw-fail",
    )
    .await;

    let prefix = user_message("prefix before close raw output failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "close-raw-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "close-raw-fail-open".to_string(),
            "close raw failure child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("close-raw-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let inner = assistant_message("child body before close raw output failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "close-raw-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "close-raw-fail".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    let original_history = session.clone_history().await.raw_items().to_vec();
    while rx.try_recv().is_ok() {}

    let err = commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("close-raw-fail"),
    )
    .await
    .expect_err("close raw output append failure should fail closed");
    assert!(
        matches!(err, CodexErr::SpineAppendFailure { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice(),
        "failed close raw output append must not replace host history"
    );
    let err = session
        .spine_tree()
        .await
        .expect_err("raw output append failure should invalidate Spine runtime");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected runtime error: {err}"
    );
    assert_no_pending_spine_tree_update_matching(
        &rx,
        "failed close raw output append must not publish committed close tree state",
        |snapshot| {
            snapshot
                .nodes
                .iter()
                .any(|node| node.node_id == "1.1.1" && node.status == SpineTreeNodeStatus::Closed)
        },
    );
    assert_eq!(
        compact_mock.requests().len(),
        0,
        "failed durable close output append must not start a secondary memory request"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(
        !resumed.history.iter().any(|item| matches!(
            item,
            RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, .. })
                if call_id == "close-raw-fail"
        )),
        "failed close raw output append must not persist the close output carrier"
    );
}

#[tokio::test]
async fn close_sidecar_commit_marker_failure_invalidates_runtime() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-close-sidecar-marker-failure",
            "close sidecar marker failure compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before close sidecar marker failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "close-sidecar-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "close-sidecar-fail-open".to_string(),
            "close sidecar failure child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("close-sidecar-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let inner = assistant_message("child body before close sidecar marker failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "close-sidecar-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "close-sidecar-fail".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    let original_history = session.clone_history().await.raw_items().to_vec();
    while rx.try_recv().is_ok() {}

    let store = SpineStore::for_rollout(&rollout_path).expect("spine store");
    std::fs::create_dir_all(store.commit_path_for_test())
        .expect("block close commit marker append");

    let err = test_on_toolcall_single(
        &session,
        &turn_context,
        &function_output("close-sidecar-fail"),
    )
    .await
    .expect_err("close sidecar marker append failure should fail");
    assert!(
        err.should_invalidate_runtime(),
        "sidecar marker failure should be invalidating: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice(),
        "failed close sidecar commit must not replace host history"
    );
    let err = session
        .spine_tree()
        .await
        .expect_err("sidecar commit failure should invalidate Spine runtime");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected runtime error: {err}"
    );
    assert_no_pending_spine_tree_update_matching(
        &rx,
        "failed close sidecar commit must not publish committed close tree state",
        |snapshot| {
            snapshot
                .nodes
                .iter()
                .any(|node| node.node_id == "1.1.1" && node.status == SpineTreeNodeStatus::Closed)
        },
    );
    assert_eq!(compact_mock.requests().len(), 0);
}

#[tokio::test]
async fn spine_close_deferred_history_failure_does_not_publish_success_events() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-close-history-failure",
            "close history failure compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let prefix = user_message("prefix before close history failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "close-history-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "close-history-fail-open".to_string(),
            "close history failure child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("close-history-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let inner = assistant_message("child body before close history failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "close-history-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "close-history-fail".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    while rx.try_recv().is_ok() {}

    let close_output = function_output("close-history-fail");
    let mut commit = test_on_toolcall_single(&session, &turn_context, &close_output)
        .await
        .expect("commit close output");
    assert_eq!(
        commit.recording(),
        SpineToolOutputRecording::Skip,
        "close reduce boundary should record raw output before returning success"
    );
    let deferred_tree_update = commit.take_deferred_tree_update();
    assert_no_event_matching(
        &rx,
        "close reduce must not publish committed tree state before the post-commit caller step",
        |event| match &event.msg {
            EventMsg::SpineTreeUpdate(snapshot) => snapshot
                .nodes
                .iter()
                .any(|node| node.node_id == "1.1.1" && node.status == SpineTreeNodeStatus::Closed),
            _ => false,
        },
    );
    let history = session.clone_history().await;
    assert!(
        history
            .raw_items()
            .iter()
            .any(|item| message_text_contains(item, "test node memory")),
        "close reduce success must publish host history before returning"
    );
    assert!(
        history.raw_items().contains(&close_output),
        "close reduce boundary must retain the raw output carrier in host history"
    );
    if let Some(snapshot) = deferred_tree_update {
        assert!(
            snapshot
                .nodes
                .iter()
                .any(|node| node.node_id == "1.1.1" && node.status == SpineTreeNodeStatus::Closed),
            "test must hold a committed close tree snapshot before deciding not to publish it"
        );
    } else {
        panic!("close commit should defer a tree update");
    }
    session
        .spine_tree()
        .await
        .expect("Spine runtime remains valid");
    assert_eq!(compact_mock.requests().len(), 0);
}

#[tokio::test]
async fn spine_close_host_publish_failure_does_not_install_live_parse_stack() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-close-host-publish-failure",
            "close host publish failure compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let prefix = user_message("prefix before close host publish failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "close-host-publish-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "close-host-publish-fail-open".to_string(),
            "close host publish failure child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("close-host-publish-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let inner = assistant_message("child body before close host publish failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "close-host-publish-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "close-host-publish-fail".to_string(),
            "host publish failure memory".to_string(),
        )
        .await
        .expect("stage close");
    let close_output = function_output("close-host-publish-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_output))
        .await
        .expect("record close output before commit");
    let original_history = session.clone_history().await.raw_items().to_vec();
    while rx.try_recv().is_ok() {}

    session
        .fail_next_history_suffix_replace_for_test("forced host publish failure")
        .await;
    let err = test_on_toolcall_single(&session, &turn_context, &close_output)
        .await
        .expect_err("host publish failure should fail close reduce");
    assert!(
        err.should_invalidate_runtime(),
        "host publish failure after durable reduce must invalidate runtime: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice(),
        "host publish failure must leave host history unchanged"
    );
    let err = session
        .spine_tree()
        .await
        .expect_err("host publish failure should leave no continuable advanced runtime");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected runtime error: {err}"
    );
    assert_no_pending_spine_tree_update_matching(
        &rx,
        "host publish failure must not emit a closed-node tree update",
        |snapshot| {
            snapshot
                .nodes
                .iter()
                .any(|node| node.node_id == "1.1.1" && node.status == SpineTreeNodeStatus::Closed)
        },
    );
    assert_eq!(compact_mock.requests().len(), 0);
}

#[tokio::test]
async fn spine_next_host_publish_failure_does_not_install_live_parse_stack() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-next-host-publish-failure",
            "next host publish failure compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let prefix = user_message("prefix before next host publish failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "next-host-publish-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "next-host-publish-fail-open".to_string(),
            "next host publish failure child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("next-host-publish-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let inner = assistant_message("child body before next host publish failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");
    let next_request = spine_call(SPINE_TOOL_NEXT, "next-host-publish-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "next-host-publish-fail".to_string(),
            "next host publish failure sibling".to_string(),
            "next host publish failure memory".to_string(),
        )
        .await
        .expect("stage next");
    let next_output = function_output("next-host-publish-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_output))
        .await
        .expect("record next output before commit");
    let original_history = session.clone_history().await.raw_items().to_vec();
    while rx.try_recv().is_ok() {}

    session
        .fail_next_history_suffix_replace_for_test("forced next host publish failure")
        .await;
    let err = test_on_toolcall_single(&session, &turn_context, &next_output)
        .await
        .expect_err("host publish failure should fail next reduce");
    assert!(
        err.should_invalidate_runtime(),
        "host publish failure after durable next reduce must invalidate runtime: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice(),
        "host publish failure must leave host history unchanged"
    );
    let err = session
        .spine_tree()
        .await
        .expect_err("host publish failure should leave no continuable advanced runtime");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected runtime error: {err}"
    );
    assert_no_pending_spine_tree_update_matching(
        &rx,
        "host publish failure must not emit a next-sibling tree update",
        |snapshot| snapshot.active_node_id == "1.1.2",
    );
    assert_eq!(compact_mock.requests().len(), 0);
}

#[tokio::test]
async fn spine_next_raw_output_append_failure_does_not_replace_host_history() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-next-raw-output-failure",
            "next failure compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path = attach_thread_persistence_with_raw_output_append_failure(
        Arc::get_mut(&mut session).expect("session should be unique"),
        "next-raw-fail",
    )
    .await;

    let prefix = user_message("prefix before next raw output failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "next-raw-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "next-raw-fail-open".to_string(),
            "next raw failure child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("next-raw-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let inner = assistant_message("child body before next raw output failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");
    let next_request = spine_call(SPINE_TOOL_NEXT, "next-raw-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "next-raw-fail".to_string(),
            "next raw failure sibling".to_string(),
            "next raw failure memory".to_string(),
        )
        .await
        .expect("stage next");
    let original_history = session.clone_history().await.raw_items().to_vec();
    while rx.try_recv().is_ok() {}

    let err = commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("next-raw-fail"),
    )
    .await
    .expect_err("next raw output append failure should fail closed");
    assert!(
        matches!(err, CodexErr::SpineAppendFailure { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice(),
        "failed next raw output append must not replace host history"
    );
    let err = session
        .spine_tree()
        .await
        .expect_err("raw output append failure should invalidate Spine runtime");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected runtime error: {err}"
    );
    assert_no_pending_spine_tree_update_matching(
        &rx,
        "failed next raw output append must not publish committed next tree state",
        |snapshot| snapshot.active_node_id == "1.1.2",
    );
    assert_eq!(
        compact_mock.requests().len(),
        0,
        "failed durable tool output append must not start a secondary memory request"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(
        !resumed.history.iter().any(|item| matches!(
            item,
            RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, .. })
                if call_id == "next-raw-fail"
        )),
        "failed next raw output append must not persist the next output carrier"
    );
}

#[tokio::test]
async fn next_sidecar_commit_marker_failure_invalidates_runtime() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-next-sidecar-marker-failure",
            "next sidecar marker failure compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before next sidecar marker failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "next-sidecar-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "next-sidecar-fail-open".to_string(),
            "next sidecar failure child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("next-sidecar-fail-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let inner = assistant_message("child body before next sidecar marker failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");
    let next_request = spine_call(SPINE_TOOL_NEXT, "next-sidecar-fail");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "next-sidecar-fail".to_string(),
            "next sidecar failure sibling".to_string(),
            "next sidecar failure memory".to_string(),
        )
        .await
        .expect("stage next");
    let original_history = session.clone_history().await.raw_items().to_vec();
    while rx.try_recv().is_ok() {}

    let store = SpineStore::for_rollout(&rollout_path).expect("spine store");
    std::fs::create_dir_all(store.commit_path_for_test()).expect("block next commit marker append");

    let err = test_on_toolcall_single(
        &session,
        &turn_context,
        &function_output("next-sidecar-fail"),
    )
    .await
    .expect_err("next sidecar marker append failure should fail");
    assert!(
        err.should_invalidate_runtime(),
        "sidecar marker failure should be invalidating: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice(),
        "failed next sidecar commit must not replace host history"
    );
    let err = session
        .spine_tree()
        .await
        .expect_err("sidecar commit failure should invalidate Spine runtime");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected runtime error: {err}"
    );
    assert_no_pending_spine_tree_update_matching(
        &rx,
        "failed next sidecar commit must not publish committed next tree state",
        |snapshot| snapshot.active_node_id == "1.1.2",
    );
    assert_eq!(compact_mock.requests().len(), 0);
}

#[tokio::test]
async fn spine_next_direct_memory_ignores_mock_compact_response_and_opens_sibling() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        sse(vec![
            serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "image_generation_call",
                    "id": "ig-spine-next-compact",
                    "status": "completed",
                    "revised_prompt": "spine.next\nsummary: bad sibling\ninstruction: preserve the failed compact payload",
                    "result": "Zm9v"
                }
            }),
            ev_completed("spine-next-image-generation-response"),
        ]),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before image-generation next failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "image-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "image-next-open".to_string(),
            "image next child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("image-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let child_body = assistant_message("child body before image-generation next failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record child body");
    let next_request = spine_call(SPINE_TOOL_NEXT, "image-next");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "image-next".to_string(),
            "bad sibling".to_string(),
            "image-generation compact must fail closed".to_string(),
        )
        .await
        .expect("stage next");
    let next_output = function_output("image-next");
    session
        .record_conversation_items_without_spine_observe(
            &turn_context,
            std::slice::from_ref(&next_output),
        )
        .await
        .expect("record next output before Spine commit");

    test_on_toolcall_single(&session, &turn_context, &next_output)
        .await
        .expect("direct memory next should not request or parse compact response");
    assert_eq!(compact_mock.requests().len(), 0);

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(
        session.clone_history().await.raw_items(),
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)")
            .as_slice(),
        "next reduce success must leave ContextManager.items equal to h(PS)"
    );
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1.2"), "{tree}");
    assert!(tree.contains("[1.1.1] Done"), "{tree}");
    assert!(tree.contains("[1.1.2] Current bad sibling"), "{tree}");
}

#[tokio::test]
async fn spine_next_direct_memory_commit_does_not_wait_for_compact_request() {
    let server = start_mock_server().await;
    let next_compact_mock = core_test_support::responses::mount_response_once(
        &server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(spine_node_memory_summary_sse(
                "late-spine-next-summary",
                "late next compact summary",
            ))
            .set_delay(Duration::from_secs(5)),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before partial next");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "partial-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "partial-next-open".to_string(),
            "partial next child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("partial-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let child_body = assistant_message("partial next child work");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record child body");
    let next_request = spine_call(SPINE_TOOL_NEXT, "partial-next");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "partial-next".to_string(),
            "partial next sibling".to_string(),
            "partial next memory".to_string(),
        )
        .await
        .expect("stage next");
    let next_output = function_output("partial-next");

    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, next_output)
        .await
        .expect("direct-memory next commit should not wait for compact request");
    assert_no_pending_spine_commit(&session, "partial-next").await;
    assert_eq!(next_compact_mock.requests().len(), 0);

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render spine tree");
    assert!(tree.contains("[1.1.1] Done"), "{tree}");
    assert!(
        tree.contains("[1.1.2] Current partial next sibling"),
        "{tree}"
    );
}

#[tokio::test]
async fn spine_next_direct_memory_commit_does_not_run_overflow_compact() {
    let server = start_mock_server().await;
    let responses_mock = core_test_support::responses::mount_response_once(
        &server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse_failed(
                "spine-next-overflow",
                "context_length_exceeded",
                "Your input exceeds the context window of this model. Please adjust your input and try again.",
            ))
            .set_delay(Duration::from_secs(5)),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.name = "non-openai test provider".to_string();
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before next overflow");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "overflow-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "overflow-next-open".to_string(),
            "overflow next child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("overflow-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let child_body = assistant_message("child work before next overflow");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record child body");
    let next_request = spine_call(SPINE_TOOL_NEXT, "overflow-next");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "overflow-next".to_string(),
            "overflow next sibling".to_string(),
            "overflow next memory".to_string(),
        )
        .await
        .expect("stage next");
    let next_output = function_output("overflow-next");

    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, next_output)
        .await
        .expect("direct-memory next commit should not run a compact request");
    assert_no_pending_spine_commit(&session, "overflow-next").await;
    assert_eq!(responses_mock.requests().len(), 0);

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    let tree = runtime.render_tree().expect("render spine tree");
    assert!(tree.contains("[1.1.1] Done"), "{tree}");
    assert!(
        tree.contains("[1.1.2] Current overflow next sibling"),
        "{tree}"
    );
    assert!(
        materialized
            .iter()
            .any(|item| message_text_contains(item, "overflow next memory")),
        "direct-memory next should publish the provided node memory"
    );
}

#[tokio::test]
async fn spine_next_direct_memory_opens_sibling_and_keeps_completed_toolcall() {
    let server = start_mock_server().await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.name = "non-openai test provider".to_string();
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before durable next overflow");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "durable-overflow-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "durable-overflow-next-open".to_string(),
            "durable overflow next child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("durable-overflow-next-open");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, open_output)
        .await
        .expect("commit durable open output");

    let child_body = assistant_message("child work before durable next overflow");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record child body");
    let next_request = spine_call(SPINE_TOOL_NEXT, "durable-overflow-next");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "durable-overflow-next".to_string(),
            "direct durable sibling".to_string(),
            "durable overflow next memory".to_string(),
        )
        .await
        .expect("stage next");
    let next_output = function_output("durable-overflow-next");

    let recorded_output =
        commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, next_output)
            .await
            .expect("direct-memory next opens sibling and keeps durable toolcall");
    assert!(matches!(
        recorded_output,
        ResponseItem::FunctionCallOutput { ref call_id, .. } if call_id == "durable-overflow-next"
    ));
    assert_no_pending_spine_commit(&session, "durable-overflow-next").await;
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    assert_eq!(session.clone_history().await.raw_items(), materialized);
    assert!(
        materialized.iter().any(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, .. }
                    if call_id == "durable-overflow-next"
            )
        }),
        "durable completed next toolcall must remain in sibling h(PS)"
    );
    let tree = runtime.render_tree().expect("render spine tree");
    assert!(tree.contains("[1.1.1] Done"), "{tree}");
    assert!(
        tree.contains("[1.1.2] Current direct durable sibling"),
        "{tree}"
    );
}

#[tokio::test]
async fn grouped_spine_next_direct_memory_opens_sibling_and_keeps_completed_toolcall() {
    let server = start_mock_server().await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.name = "non-openai test provider".to_string();
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before grouped durable next overflow");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "grouped-durable-overflow-next-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "grouped-durable-overflow-next-open".to_string(),
            "grouped durable overflow next child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("grouped-durable-overflow-next-open");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, open_output)
        .await
        .expect("commit durable open output");

    let child_body = assistant_message("child work before grouped durable next overflow");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record child body");
    let ordinary_request = function_call("ordinary_tool", "grouped-durable-ordinary");
    let next_request = spine_call(SPINE_TOOL_NEXT, "grouped-durable-overflow-next");
    session
        .record_conversation_items(
            &turn_context,
            &[ordinary_request.clone(), next_request.clone()],
        )
        .await
        .expect("record grouped requests");
    session
        .test_seed_spine_next_control_request(
            "grouped-durable-overflow-next".to_string(),
            "direct grouped durable sibling".to_string(),
            "grouped durable overflow next memory".to_string(),
        )
        .await
        .expect("stage next");
    let ordinary_output = function_output("grouped-durable-ordinary");
    let next_output = function_output("grouped-durable-overflow-next");
    let commit = session
        .test_on_toolcall(
            &turn_context,
            ToolCallEvidence::grouped(
                "grouped-durable-overflow-next",
                &[
                    "grouped-durable-ordinary".to_string(),
                    "grouped-durable-overflow-next".to_string(),
                ],
                &[ordinary_output.clone(), next_output.clone()],
            ),
        )
        .await
        .expect("direct-memory grouped next opens sibling and keeps durable toolcall");
    assert_eq!(
        commit.recording(),
        SpineToolOutputRecording::Skip,
        "grouped close-like reduce records raw outputs inside the commit boundary"
    );
    assert_no_pending_spine_commit(&session, "grouped-durable-overflow-next").await;
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    assert_eq!(
        session.clone_history().await.raw_items(),
        materialized.as_slice(),
        "grouped next reduce success must leave ContextManager.items equal to h(PS)"
    );
    for expected_call_id in ["grouped-durable-ordinary", "grouped-durable-overflow-next"] {
        assert!(
            materialized.iter().any(|item| {
                matches!(
                    item,
                    ResponseItem::FunctionCall { call_id, .. }
                        | ResponseItem::FunctionCallOutput { call_id, .. }
                        if call_id == expected_call_id
                )
            }),
            "durable grouped completed toolcall must keep request/output for {expected_call_id} in sibling h(PS): {materialized:#?}"
        );
    }
    let tree = runtime.render_tree().expect("render spine tree");
    assert!(tree.contains("[1.1.1] Done"), "{tree}");
    assert!(
        tree.contains("[1.1.2] Current direct grouped durable sibling"),
        "{tree}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_next_direct_memory_does_not_emit_blank_turn_complete() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::SpineJit)
            .expect("enable spine feature");
        config.model_provider.supports_websockets = false;
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call_with_namespace(
                    "call-spine-next-direct-memory",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_NEXT,
                    r#"{"summary":"direct sibling","memory":"direct next memory"}"#,
                ),
                ev_completed("spine-next-tool-response"),
            ]),
            sse(vec![
                ev_assistant_message("spine-next-follow-up", "advanced"),
                ev_completed("spine-next-follow-up-response"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "start a spine task and advance".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;

    let last_agent_message = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(turn_complete) => turn_complete.last_agent_message.clone(),
        _ => None,
    })
    .await;
    assert_eq!(last_agent_message.as_str(), "advanced");

    let blank_completion = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let event = test.codex.next_event().await?;
            if let EventMsg::TurnComplete(TurnCompleteEvent {
                last_agent_message: None,
                ..
            }) = event.msg
            {
                return anyhow::Ok(());
            }
        }
    })
    .await;
    assert!(
        blank_completion.is_err(),
        "direct-memory Spine next must not emit an extra blank TurnComplete"
    );

    let requests = responses.requests();
    assert_eq!(
        requests.len(),
        2,
        "expected sampling request and follow-up request without secondary compact"
    );
    assert_eq!(
        requests[0].body_json()["tool_choice"].as_str(),
        Some("auto")
    );
    assert!(!requests[0].body_contains_text("---------- SPINE MEMORY COMPACT ----------"));
    assert!(!requests[1].body_contains_text("---------- SPINE MEMORY COMPACT ----------"));
    test.codex.ensure_rollout_materialized().await;
    test.codex.flush_rollout().await?;
    let rollout_path = test
        .codex
        .rollout_path()
        .expect("test thread should have rollout persistence");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime after direct-memory next")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Current direct sibling"), "{tree}");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS) after direct-memory next");
    assert!(
        materialized.iter().any(|item| matches!(
            item,
            ResponseItem::Message { content, .. }
                if content.iter().any(|content| matches!(
                    content,
                    ContentItem::InputText { text }
                        if text.contains("direct next memory")
                ))
        )),
        "materialized h(PS) should contain direct next memory: {materialized:#?}"
    );
    assert!(
        materialized.iter().any(|item| matches!(
            item,
            ResponseItem::Message { role, content, .. }
                if role == "assistant"
                    && content.iter().any(|content| matches!(
                        content,
                        ContentItem::OutputText { text }
                            if text == "advanced"
                    ))
        )),
        "materialized h(PS) should contain the follow-up assistant message: {materialized:#?}"
    );

    Ok(())
}

#[tokio::test]
async fn spine_close_bridge_can_close_initial_root_child() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-close-root-child-summary",
            "root child compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let root_child_work = user_message("initial root child work");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&root_child_work))
        .await
        .expect("record conversation items");

    let close_request = spine_call(SPINE_TOOL_CLOSE, "close-root-child");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "close-root-child".to_string(),
            "root child direct memory".to_string(),
        )
        .await
        .expect("stage close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("close-root-child"),
    )
    .await
    .expect("commit close output and record raw evidence");

    assert_eq!(
        compact_mock.requests().len(),
        0,
        "spine.close should use direct memory without a secondary compact request"
    );

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 3);
    assert!(matches!(
        &items[0],
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1")
                            && contains_user_memory_block(text, "initial root child work")
                            && text.contains("## Node Memory\nroot child direct memory")
                            && !text.contains("---------- SPINE MEMORY COMPACT ----------")
                )
    ));
    assert_eq!(items[1], spine_call(SPINE_TOOL_CLOSE, "close-root-child"));
    assert!(matches!(
        &items[2],
        ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "close-root-child"
    ));

    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1"), "{tree}");
    assert!(tree.contains("[1] Current"), "{tree}");
    assert!(tree.contains("[1.1] Done"), "{tree}");
    assert!(!tree.contains("root"), "{tree}");
    let (_, raw_end, context_start, context_end) = SpineStore::for_rollout(&rollout_path)
        .expect("load spine store")
        .suffix_mem_cover_for_test("1.1")
        .expect("read spine mem records")
        .expect("closed root child suffix memory should be recorded");
    assert_eq!(context_start, 0);
    assert_eq!(context_end, 1);
    assert_eq!(raw_end, 1);
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        items
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_close_memory_uses_required_node_memory_for_exact_only_suffix() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-close-node-memory-only-summary",
            "preserved node memory facts: KEEP_CLOSE_GUIDANCE_42",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let root_child_work = user_message("initial exact-only root child work");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&root_child_work))
        .await
        .expect("record conversation items");

    let close_request = spine_call(SPINE_TOOL_CLOSE, "close-root-child-with-instruction");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "close-root-child-with-instruction".to_string(),
            "KEEP_CLOSE_MEMORY_42".to_string(),
        )
        .await
        .expect("stage close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("close-root-child-with-instruction"),
    )
    .await
    .expect("commit close output and record raw evidence");

    assert_eq!(compact_mock.requests().len(), 0);

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 3);
    assert!(matches!(
        &items[0],
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1")
                            && contains_user_memory_block(text, "initial exact-only root child work")
                            && text.contains("## Node Memory\nKEEP_CLOSE_MEMORY_42")
                            && !text.contains("## Memory Slot")
                            && !contains_user_memory_block(text, "KEEP_CLOSE_MEMORY_42")
                            && !text.contains("---------- SPINE MEMORY COMPACT ----------")
                )
    ));
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_close_direct_memory_commit_publishes_host_history_before_return() {
    let server = start_mock_server().await;
    let compact_mock = core_test_support::responses::mount_response_once(
        &server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(spine_node_memory_summary_sse(
                "unused-spine-summary",
                "unused compact summary",
            )),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");
    let open_request = spine_call(SPINE_TOOL_OPEN, "open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request("open".to_string(), "child".to_string())
        .await
        .expect("stage open");
    let open_output = function_output("open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open output");
    let inner = assistant_message("inside");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record conversation items");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request("close".to_string(), "test node memory".to_string())
        .await
        .expect("stage close");
    let close_output = function_output("close");
    session
        .record_conversation_items_without_spine_observe(
            &turn_context,
            std::slice::from_ref(&close_output),
        )
        .await
        .expect("record close output before Spine commit");
    test_on_toolcall_single(&session, &turn_context, &close_output)
        .await
        .expect("direct close commit should publish staged history replacement");
    assert_eq!(
        compact_mock.requests().len(),
        0,
        "direct close commit must not send a secondary compact request"
    );

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 4);
    assert!(message_text_contains(&items[0], "prefix"));
    assert!(
        matches!(
            &items[1],
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains("test node memory")
                            && !text.contains("unused compact summary")
                )
        ),
        "close reduce success must install direct Node Memory into host history"
    );
    assert_eq!(items[2], close_request);
    assert_eq!(items[3], close_output);
    assert!(
        !items.iter().any(|item| matches!(
            item,
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("unused compact summary")
                )
        )),
        "close reduce success must not use compact response text"
    );
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(
        session.clone_history().await.raw_items(),
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)")
            .as_slice(),
        "close reduce success must leave ContextManager.items equal to h(PS)"
    );
}

#[tokio::test]
async fn spine_close_reduce_records_raw_output_and_publishes_host_history_before_return() {
    let server = start_mock_server().await;
    let compact_mock = core_test_support::responses::mount_response_once(
        &server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(spine_node_memory_summary_sse(
                "unused-spine-summary",
                "unused compact summary",
            )),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before raw-internal close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "raw-internal-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request("raw-internal-open".to_string(), "child".to_string())
        .await
        .expect("stage open");
    let open_output = function_output("raw-internal-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open output");

    let inner = assistant_message("inside raw-internal close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record child body");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "raw-internal-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "raw-internal-close".to_string(),
            "raw-internal node memory".to_string(),
        )
        .await
        .expect("stage close");
    while rx.try_recv().is_ok() {}

    let close_output = function_output("raw-internal-close");
    let commit = test_on_toolcall_single(&session, &turn_context, &close_output)
        .await
        .expect("close reduce should record raw output and publish host history");
    assert_eq!(
        commit.recording(),
        SpineToolOutputRecording::Skip,
        "close reduce boundary should already record the raw output carrier"
    );
    assert_eq!(
        compact_mock.requests().len(),
        0,
        "direct memory close must not send a secondary compact request"
    );

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 4);
    assert!(message_text_contains(
        &items[0],
        "prefix before raw-internal close"
    ));
    assert!(
        matches!(
            &items[1],
            ResponseItem::Message { content, .. }
                if matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains("raw-internal node memory")
                )
        ),
        "close reduce success must publish direct Node Memory into host history"
    );
    assert_eq!(items[2], close_request);
    assert_eq!(items[3], close_output);
    assert_no_pending_spine_tree_update_matching(
        &rx,
        "tree update must remain a post-commit effect and not publish before reduce success",
        |snapshot| {
            snapshot
                .nodes
                .iter()
                .any(|node| node.node_id == "1.1.1" && node.status == SpineTreeNodeStatus::Closed)
        },
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(
        session.clone_history().await.raw_items(),
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)")
            .as_slice(),
        "close reduce success must leave ContextManager.items equal to h(PS)"
    );
}

#[tokio::test]
async fn spine_close_open_toolcall_leaf_makes_live_suffix_non_empty() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "open-toolcall-close-summary",
            "open toolcall compact summary",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before empty close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");
    let open_request = spine_call(SPINE_TOOL_OPEN, "empty-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request("empty-open".to_string(), "empty child".to_string())
        .await
        .expect("stage open");
    let open_output = function_output("empty-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let close_request = spine_call(SPINE_TOOL_CLOSE, "empty-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "empty-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    let close_output = commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("empty-close"),
    )
    .await
    .expect("open toolcall leaf makes the live suffix compactable");
    assert!(matches!(
        &close_output,
        ResponseItem::FunctionCallOutput { call_id, output }
            if call_id == "empty-close"
                && output.text_content() == Some("ok")
    ));
    assert_no_pending_spine_commit(&session, "empty-close").await;
    assert_eq!(
        compact_mock.requests().len(),
        0,
        "close after open must use direct memory without a secondary compact request"
    );
    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 4);
    assert_eq!(
        items[0],
        anchored_user_message(1, "prefix before empty close")
    );
    assert!(matches!(
        &items[1],
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains("## Node Memory\ntest node memory")
                            && !text.contains("open toolcall compact summary")
            )
    ));
    assert_eq!(items[2], spine_call(SPINE_TOOL_CLOSE, "empty-close"));
    assert!(matches!(
        &items[3],
        ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "empty-close"
    ));

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render spine tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert!(tree.contains("[1.1.1] Done empty child"), "{tree}");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        items
    );
}

#[tokio::test]
async fn spine_close_accepts_marker_like_memory_as_opaque_text_without_mutating_history() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        sse(vec![
            serde_json::json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction",
                    "encrypted_content": "gAAAAencrypted-only-summary"
                }
            }),
            ev_completed("encrypted-only-spine-close"),
        ]),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before encrypted only close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");
    let open_request = spine_call(SPINE_TOOL_OPEN, "encrypted-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request(
            "encrypted-open".to_string(),
            "encrypted child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("encrypted-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");
    let child_body = assistant_message("child body before encrypted only close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record conversation items");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "encrypted-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "encrypted-close".to_string(),
            "bad memory with ## Node Memory marker".to_string(),
        )
        .await
        .expect("node memory text is opaque except for non-empty validation");
    assert_pending_spine_commit(&session, "encrypted-close").await;
    assert_eq!(compact_mock.requests().len(), 0);
    assert_eq!(
        session.clone_history().await.raw_items(),
        &[
            anchored_user_message(1, "prefix before encrypted only close"),
            open_request.clone(),
            open_output.clone(),
            child_body.clone(),
            close_request.clone(),
        ]
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render spine tree");
    assert!(tree.contains("[1.1.1] Current encrypted child"), "{tree}");
}

#[tokio::test]
async fn spine_close_direct_memory_commit_does_not_wait_for_compact_request() {
    let server = start_mock_server().await;
    let memory_assembly_mock = core_test_support::responses::mount_response_once(
        &server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(spine_node_memory_summary_sse(
                "late-spine-summary",
                "late close memory response",
            ))
            .set_delay(Duration::from_secs(5)),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before partial close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");
    let open_request = spine_call(SPINE_TOOL_OPEN, "partial-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request(
            "partial-open".to_string(),
            "partial child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("partial-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");
    let child_body = assistant_message("partial child work");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record conversation items");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "partial-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "partial-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    let close_output = function_output("partial-close");

    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, close_output)
        .await
        .expect("direct-memory close commit should not wait for compact request");
    assert_no_pending_spine_commit(&session, "partial-close").await;
    assert_eq!(memory_assembly_mock.requests().len(), 0);

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render spine tree");
    assert!(tree.contains("[1.1] Current"), "{tree}");
    assert!(tree.contains("[1.1.1] Done partial child"), "{tree}");
}

#[tokio::test]
async fn spine_close_direct_memory_commit_does_not_run_overflow_compact() {
    let server = start_mock_server().await;
    let responses_mock = core_test_support::responses::mount_response_once(
        &server,
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse_failed(
                "spine-close-overflow",
                "context_length_exceeded",
                "Your input exceeds the context window of this model. Please adjust your input and try again.",
            ))
            .set_delay(Duration::from_secs(5)),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.name = "non-openai test provider".to_string();
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before close overflow");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "overflow-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "overflow-open".to_string(),
            "overflow child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("overflow-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let child_body = assistant_message("child work before close overflow");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record child body");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "overflow-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "overflow-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    let close_output = function_output("overflow-close");

    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, close_output)
        .await
        .expect("direct-memory close commit should not run a compact request");
    assert_no_pending_spine_commit(&session, "overflow-close").await;
    assert_eq!(responses_mock.requests().len(), 0);

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    let tree = runtime.render_tree().expect("render spine tree");
    assert!(tree.contains("[1.1.1] Done overflow child"), "{tree}");
    assert!(
        materialized
            .iter()
            .any(|item| message_text_contains(item, "test node memory")),
        "direct-memory close should publish the provided node memory"
    );
}

#[tokio::test]
async fn spine_parent_memory_assemblys_child_memory_not_child_raw_trajs() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before outer");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");

    let outer_open_request = spine_call(SPINE_TOOL_OPEN, "outer-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&outer_open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request("outer-open".to_string(), "outer".to_string())
        .await
        .expect("stage outer open");
    let outer_open_output = function_output("outer-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&outer_open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &outer_open_output)
        .await
        .expect("commit outer open");

    let outer_setup = user_message("outer setup");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&outer_setup))
        .await
        .expect("record conversation items");

    let inner_open_request = spine_call(SPINE_TOOL_OPEN, "inner-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner_open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request("inner-open".to_string(), "inner".to_string())
        .await
        .expect("stage inner open");
    let inner_open_output = function_output("inner-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner_open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &inner_open_output)
        .await
        .expect("commit inner open");

    let inner_raw = assistant_message("inner assistant traj should be folded away");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner_raw))
        .await
        .expect("record conversation items");

    let inner_close_request = spine_call(SPINE_TOOL_CLOSE, "inner-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner_close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "inner-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage inner close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("inner-close"),
    )
    .await
    .expect("commit inner close and record raw evidence");

    let after_inner = assistant_message("after inner in outer suffix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&after_inner))
        .await
        .expect("record conversation items");

    let outer_close_request = spine_call(SPINE_TOOL_CLOSE, "outer-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&outer_close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "outer-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage outer close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("outer-close"),
    )
    .await
    .expect("commit outer close and record raw evidence");

    let store = SpineStore::for_rollout(&rollout_path).expect("load spine store");
    let inner_memory = store
        .memory_body_for_test("1.1.1.1")
        .expect("read inner memory")
        .expect("inner memory should exist");
    assert!(inner_memory.contains("## Node Memory\ntest node memory"));
    let outer_memory = store
        .memory_body_for_test("1.1.1")
        .expect("read outer memory")
        .expect("outer memory should exist");
    assert!(
        outer_memory.contains("## Child Memory\n# Spine Memory 1.1.1.1"),
        "outer close suffix evidence should include child memory evidence: {outer_memory}"
    );
    assert!(outer_memory.contains("## Node Memory\ntest node memory"));
    assert!(
        !outer_memory.contains("inner assistant traj should be folded away"),
        "outer memory must preserve child memory, not child raw trajs: {outer_memory}"
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_native_compact_replacement_history_matches_parse_stack_materialization() {
    let server = start_mock_server().await;
    let responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("native-compact-summary", "native root compact summary"),
            ev_completed("native-compact-response"),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before native compact");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");

    let open_request = spine_call(SPINE_TOOL_OPEN, "native-compact-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request(
            "native-compact-open".to_string(),
            "child before native compact".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("native-compact-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let child_body = assistant_message("child body before native compact");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record conversation items");

    let close_request = spine_call(SPINE_TOOL_CLOSE, "native-compact-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "native-compact-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    let close_output = function_output("native-compact-close");
    test_on_toolcall_single(&session, &turn_context, &close_output)
        .await
        .expect("commit close");
    session
        .record_conversation_items_raw_only(&turn_context, std::slice::from_ref(&close_output))
        .await
        .expect("record conversation items");

    crate::compact::run_compact_task(
        Arc::clone(&session),
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: "compact now".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await
    .expect("native compact succeeds");

    assert_eq!(responses_mock.requests().len(), 1);
    let native_compact_request = &responses_mock.requests()[0];
    assert_eq!(
        native_compact_request.body_json()["tool_choice"].as_str(),
        Some("auto"),
        "Spine must not change native compact tool choice before the request"
    );
    assert!(
        native_compact_request.body_json()["tools"]
            .as_array()
            .expect("tools should be an array")
            .is_empty(),
        "local native compact should keep its base empty tool envelope"
    );
    assert!(
        !native_compact_request.body_contains_text(
            "You are compacting a Spine root epoch after native context compaction"
        ),
        "Spine root-epoch semantics are installed after native compact succeeds"
    );
    assert!(
        !native_compact_request
            .body_contains_text("successful `spine.open`, `spine.close`, and `spine.tree` calls"),
        "Spine must not inject prompt wording into native compact requests"
    );
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    let replacement_history = resumed
        .history
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_ref(),
            _ => None,
        })
        .expect("native compact should persist replacement_history");

    assert_eq!(replacement_history, &materialized);
    assert_eq!(
        session.clone_history().await.raw_items(),
        materialized.as_slice()
    );
    assert!(
        materialized
            .iter()
            .any(|item| message_text_contains(item, "native root compact summary")),
        "root compact h(PS) should preserve native compact summary slot: {materialized:#?}"
    );
    assert_eq!(
        message_text_count(&materialized, "# Spine Native Compact Memory"),
        0
    );
    assert_eq!(
        message_text_count(
            &materialized,
            "This memory is derived from the host context after native compact succeeded."
        ),
        0
    );
    assert!(
        materialized
            .iter()
            .filter(|item| message_text_contains(item, "native root compact summary"))
            .all(|item| !message_text_contains(item, "<spine_memory>")),
        "native compact summary slot must not be wrapped as Spine memory: {materialized:#?}"
    );
}

#[tokio::test]
async fn spine_native_compact_post_hook_ignores_stale_compacted_item_carrier() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("record initial history");

    let replaced_context = vec![assistant_message("actual post-hook replacement summary")];
    session
        .replace_compacted_history(
            &turn_context,
            replaced_context.clone(),
            None,
            CompactedItem {
                message: "stale compacted item carrier must not become Spine memory".to_string(),
                replacement_history: Some(vec![assistant_message(
                    "stale compacted item replacement history must not become Spine memory",
                )]),
            },
            Some(replaced_context),
        )
        .await
        .expect("install root compact after native compact");

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    let replacement_history = resumed
        .history
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_ref(),
            _ => None,
        })
        .expect("native compact should persist replacement_history");

    assert_eq!(replacement_history, &materialized);
    assert!(
        materialized
            .iter()
            .any(|item| message_text_contains(item, "actual post-hook replacement summary")),
        "root compact h(PS) should preserve the actual replacement summary: {materialized:#?}"
    );
    assert_eq!(
        message_text_count(&materialized, "# Spine Native Compact Memory"),
        0
    );
    assert_eq!(message_text_count(&materialized, "<spine_memory>"), 0);
    assert_eq!(
        message_text_count(&materialized, "stale compacted item carrier"),
        0
    );
    assert_eq!(
        message_text_count(&materialized, "stale compacted item replacement history"),
        0
    );
}

#[tokio::test]
async fn spine_non_toolcall_full_h_ps_publication_preserves_fixed_context() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.developer_instructions =
                Some("fixed developer context before h(PS) publication".to_string());
            config.user_instructions =
                Some("fixed AGENTS/user context before h(PS) publication".to_string());
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("initialize spine");
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record fixed context baseline");
    assert!(
        session.reference_context_item().await.is_some(),
        "test setup must establish a fixed context baseline"
    );

    let user_prompt = user_message("current-turn user message that publishes full h(PS)");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&user_prompt))
        .await
        .expect("record user prompt");

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    let host_history = session.clone_history().await.raw_items().to_vec();

    assert_eq!(
        variable_spine_items(&host_history),
        materialized,
        "full non-toolcall publication must leave variable context equal to h(PS)"
    );
    assert_eq!(
        message_text_count(
            &host_history,
            "fixed developer context before h(PS) publication"
        ),
        1,
        "developer fixed context must be preserved exactly once outside h(PS)"
    );
    assert_eq!(
        message_text_count(
            &host_history,
            "fixed AGENTS/user context before h(PS) publication"
        ),
        1,
        "AGENTS/user fixed context must be preserved exactly once outside h(PS)"
    );
    assert_eq!(
        message_text_count(&host_history, "<environment_context>"),
        1,
        "environment fixed context must be preserved exactly once outside h(PS)"
    );
    assert!(
        session.reference_context_item().await.is_some(),
        "baseline remains valid only because fixed context is still visible"
    );
}

#[tokio::test]
async fn spine_mid_turn_native_compact_preserves_fixed_context_without_cwd_only_suffix() {
    let server = start_mock_server().await;
    let responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message(
                "native-compact-summary",
                "mid turn native root compact summary",
            ),
            ev_completed("native-compact-response"),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.developer_instructions =
                Some("fixed compact developer context outside h(PS)".to_string());
            config.user_instructions =
                Some("fixed compact AGENTS/user context outside h(PS)".to_string());
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before mid-turn native compact");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");

    crate::compact::run_inline_auto_compact_task(
        Arc::clone(&session),
        Arc::clone(&turn_context),
        crate::compact::InitialContextInjection::BeforeLastUserMessage,
        codex_analytics::CompactionReason::ContextLimit,
        codex_analytics::CompactionPhase::MidTurn,
    )
    .await
    .expect("mid-turn native compact succeeds");

    assert_eq!(responses_mock.requests().len(), 1);
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    let replacement_history = resumed
        .history
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_ref(),
            _ => None,
        })
        .expect("native compact should persist replacement_history");

    let variable_replacement_history = variable_spine_items(replacement_history);
    assert_eq!(
        materialized
            .get(..variable_replacement_history.len())
            .expect("latest h(PS) should include compact checkpoint prefix"),
        variable_replacement_history.as_slice(),
        "mid-turn compact replacement_history must remain the compact checkpoint prefix"
    );
    assert!(
        variable_replacement_history
            .iter()
            .any(|item| message_text_contains(item, "mid turn native root compact summary")),
        "root compact h(PS) should preserve native compact summary slot: {variable_replacement_history:#?}"
    );
    assert_eq!(
        message_text_count(
            &variable_replacement_history,
            "# Spine Native Compact Memory"
        ),
        0
    );
    assert_eq!(
        message_text_count(&variable_replacement_history, "## Replaced Context Item"),
        0
    );
    assert_eq!(
        message_text_count(&variable_replacement_history, "<spine_memory>"),
        0
    );

    let history_items = session.clone_history().await.raw_items().to_vec();
    assert_eq!(
        variable_spine_items(&history_items),
        materialized,
        "mid-turn compact live variable context must equal h(PS)"
    );
    assert_eq!(
        message_text_count(
            &history_items,
            "fixed compact developer context outside h(PS)"
        ),
        1,
        "mid-turn compact must preserve developer fixed context exactly once"
    );
    assert_eq!(
        message_text_count(
            &history_items,
            "fixed compact AGENTS/user context outside h(PS)"
        ),
        1,
        "mid-turn compact must preserve AGENTS/user fixed context exactly once"
    );
    assert_eq!(
        message_text_count(&history_items, "<environment_context>"),
        1,
        "mid-turn compact must preserve environment context exactly once"
    );
    let last_text = history_items
        .last()
        .and_then(|item| match item {
            ResponseItem::Message { content, .. } => {
                content.iter().find_map(|content| match content {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        Some(text.as_str())
                    }
                    ContentItem::InputImage { .. } => None,
                })
            }
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        !last_text.starts_with("<environment_context>"),
        "mid-turn compact must not append a cwd-only environment-context suffix"
    );
}

#[tokio::test]
async fn native_compact_commit_marker_distinguishes_prefix_recovery() {
    let server = start_mock_server().await;
    let _responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message(
                "native-compact-summary",
                "native checkpoint failure summary",
            ),
            ev_completed("native-compact-response"),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("history before failed native compact checkpoint");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");
    let original_history = session.clone_history().await.raw_items().to_vec();

    let store = SpineStore::for_rollout(&rollout_path).expect("spine store");
    std::fs::create_dir_all(store.compact_checkpoint_path_for_test())
        .expect("block compact checkpoint append with directory");

    let err = crate::compact::run_compact_task(
        Arc::clone(&session),
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: "compact should fail while writing checkpoint".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await
    .expect_err("native compact checkpoint append failure should fail closed");
    assert!(
        err.to_string()
            .contains("failed to install Spine root compact"),
        "unexpected compact error: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice(),
        "failed checkpoint append must not replace host history"
    );

    let err = session
        .spine_tree()
        .await
        .expect_err("checkpoint failure should invalidate Spine runtime");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected runtime error: {err}"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("checkpoint failure should leave a recoverable pre-compact prefix")
        .expect("spine sidecar should exist");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize recovered h(PS)"),
        original_history
    );
}

#[tokio::test]
async fn native_compact_rollout_append_failure_does_not_replace_host_history() {
    let server = start_mock_server().await;
    let _responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("native-append-failure", "native append failure summary"),
            ev_completed("native-append-failure-response"),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path = attach_thread_persistence_with_compact_append_failure(
        Arc::get_mut(&mut session).expect("session should be unique"),
    )
    .await;

    let prefix = user_message("history before rollout append failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");
    let original_history = session.clone_history().await.raw_items().to_vec();

    let err = crate::compact::run_compact_task(
        Arc::clone(&session),
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: "compact should fail while appending rollout boundary".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await
    .expect_err("native compact rollout append failure should fail closed");
    assert!(
        matches!(
            &err,
            CodexErr::SpineCompactCommitFailure { operation, .. }
                if operation == "persist native compact rollout boundary"
        ),
        "unexpected compact error: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice(),
        "failed rollout append must not replace host history"
    );
    let err = session
        .spine_tree()
        .await
        .expect_err("rollout append failure should invalidate Spine runtime");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected runtime error: {err}"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(
        !resumed
            .history
            .iter()
            .any(|item| matches!(item, RolloutItem::Compacted(_))),
        "failed compact boundary append must not persist Compacted"
    );
}

#[tokio::test]
async fn native_compact_metadata_failure_after_raw_append_still_replaces_host_history() {
    let server = start_mock_server().await;
    let _responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("native-metadata-failure", "native metadata failure summary"),
            ev_completed("native-metadata-failure-response"),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path = attach_thread_persistence_with_compact_metadata_failure(
        Arc::get_mut(&mut session).expect("session should be unique"),
    )
    .await;

    let prefix = user_message("history before metadata failure");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");

    crate::compact::run_compact_task(
        Arc::clone(&session),
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: "compact should succeed after raw append despite metadata failure".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await
    .expect("native compact should succeed after raw rollout append is durable");

    let compacted_history = session.clone_history().await.raw_items().to_vec();
    assert_ne!(
        compacted_history,
        vec![prefix],
        "successful native compact must replace host history"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let compacted = resumed
        .history
        .iter()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => Some(compacted),
            _ => None,
        })
        .expect("raw-durable native compact must persist Compacted");
    assert_eq!(
        compacted.replacement_history.as_ref(),
        Some(&compacted_history),
        "persisted compact boundary must prove the replaced host history"
    );

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = spine_raw_items_after_rollback(&resumed.history)
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&rollout_path, &resumed_rollout_path, &raw_live);

    resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: resumed.history,
            rollout_path: Some(rollout_path),
        }))
        .await
        .expect("resuming raw-durable compact should validate Spine proof");
    assert_eq!(
        resumed_session.clone_history().await.raw_items(),
        compacted_history.as_slice()
    );
}

#[tokio::test]
async fn spine_resume_rejects_root_compact_sidecar_without_rollout_boundary() {
    let (mut source_session, turn_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let source_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut source_session).expect("session should be unique"),
    )
    .await;
    let prefix = user_message("prefix before orphan root compact");
    source_session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    source_session
        .install_spine_root_compact("orphan root compact body".to_string())
        .await
        .expect("install sidecar root compact")
        .expect("spine root compact should run");

    source_session.ensure_rollout_materialized().await;
    source_session
        .flush_rollout()
        .await
        .expect("rollout should flush");
    let InitialHistory::Resumed(resumed) =
        RolloutRecorder::get_rollout_history(&source_rollout_path)
            .await
            .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(
        !resumed
            .history
            .iter()
            .any(|item| matches!(item, RolloutItem::Compacted(_))),
        "test setup must leave rollout without native compact boundary"
    );

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    clone_spine_sidecar_for_test(&source_rollout_path, &resumed_rollout_path, &[true]);

    let err = resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: resumed.history,
            rollout_path: Some(source_rollout_path),
        }))
        .await
        .expect_err("orphan sidecar root compact must fail closed");
    assert!(
        err.to_string()
            .contains("root compact sidecar is missing rollout compact boundary"),
        "unexpected resume error: {err}"
    );
}

#[tokio::test]
async fn resume_rejects_second_orphan_root_compact_after_prior_successful_compact() {
    let server = start_mock_server().await;
    let _responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("first-native-compact", "first native compact summary"),
            ev_completed("first-native-compact-response"),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut source_session, turn_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
                config.model_provider.base_url = Some(base_url.clone());
                config.model_provider.supports_websockets = false;
            },
        )
        .await;
    let source_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut source_session).expect("session should be unique"),
    )
    .await;

    let prefix = user_message("prefix before first native compact");
    source_session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    crate::compact::run_compact_task(
        Arc::clone(&source_session),
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: "first native compact".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await
    .expect("first native compact succeeds");
    let post_first = user_message("post first compact raw item");
    source_session
        .record_conversation_items(&turn_context, std::slice::from_ref(&post_first))
        .await
        .expect("record post first item");
    let (orphan, _) = source_session
        .install_spine_root_compact("second orphan root compact body".to_string())
        .await
        .expect("install second sidecar root compact")
        .expect("spine root compact should run");

    source_session.ensure_rollout_materialized().await;
    source_session
        .flush_rollout()
        .await
        .expect("rollout should flush");
    let InitialHistory::Resumed(resumed) =
        RolloutRecorder::get_rollout_history(&source_rollout_path)
            .await
            .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert_eq!(
        resumed
            .history
            .iter()
            .filter(|item| matches!(item, RolloutItem::Compacted(_)))
            .count(),
        1,
        "test setup should have exactly one persisted compact boundary"
    );

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = spine_raw_items_after_rollback(&resumed.history)
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&source_rollout_path, &resumed_rollout_path, &raw_live);

    let err = resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: resumed.history,
            rollout_path: Some(source_rollout_path),
        }))
        .await
        .expect_err("second orphan sidecar root compact must fail closed");
    assert!(
        err.to_string().contains(&format!(
            "root compact sidecar is missing rollout compact boundary at raw boundary {} token_seq {}",
            orphan.raw_boundary,
            orphan.token_seq_after - 1
        )),
        "unexpected resume error: {err}"
    );
}

#[tokio::test]
async fn spine_root_compact_records_close_tokens_without_next_open_baseline() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    let prefix = user_message("prefix before root compact");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 229_136,
                total_tokens: 230_871,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }

    session
        .install_spine_root_compact("root compact body".to_string())
        .await
        .expect("install root compact")
        .expect("spine root compact should run");

    let store = SpineStore::for_rollout(&rollout_path).expect("spine store");
    let close_tokens = store.mem_close_tokens_for_test().expect("mem close tokens");
    assert_eq!(close_tokens, vec![(Some(229_136), Some(229_136))]);

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(runtime.current_open_input_tokens(), None);
    assert_eq!(runtime.current_open_provider_input_tokens(), None);
}

#[tokio::test]
async fn spine_root_compact_reduce_publishes_host_history_before_return() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let precompact_items = vec![
        user_message("root compact publish prefix 1"),
        assistant_message("root compact publish assistant 2"),
        user_message("root compact publish suffix 3"),
    ];
    for item in &precompact_items {
        session
            .record_conversation_items(&turn_context, std::slice::from_ref(item))
            .await
            .expect("record precompact item");
    }
    while rx.try_recv().is_ok() {}

    let fixed_developer = ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fixed developer instruction outside PS".to_string(),
        }],
        phase: None,
    };
    let fixed_agents = user_message(
        "# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>\nfixed agents outside PS\n</INSTRUCTIONS>",
    );
    let spine_source = vec![assistant_message("root compact publish body")];
    let mut replacement = vec![fixed_developer.clone(), fixed_agents.clone()];
    replacement.extend(spine_source.clone());
    let outcome = session
        .replace_compacted_history(
            &turn_context,
            replacement,
            None,
            CompactedItem {
                message: "native compact carrier for publication contract".to_string(),
                replacement_history: None,
            },
            Some(spine_source),
        )
        .await
        .expect("root compact reduce should publish host history");
    let snapshot = outcome
        .spine_tree_snapshot
        .expect("spine root compact should run");
    assert!(
        snapshot.active_node_id == "2.1"
            && snapshot
                .nodes
                .iter()
                .any(|node| node.node_id == "1" && node.status == SpineTreeNodeStatus::Compacted),
        "root compact snapshot should be a post-commit effect"
    );
    assert_no_pending_spine_tree_update_matching(
        &rx,
        "root compact reduce must not emit tree updates before the caller publishes post-commit effects",
        |snapshot| {
            snapshot.active_node_id == "2.1"
                && snapshot.nodes.iter().any(|node| {
                    node.node_id == "1" && node.status == SpineTreeNodeStatus::Compacted
                })
        },
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let raw_items = match RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    {
        InitialHistory::Resumed(resumed) => spine_raw_items_after_rollback(&resumed.history),
        _ => panic!("expected resumed rollout history"),
    };
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize root compact h(PS)");
    let host_history = session.clone_history().await.raw_items().to_vec();
    assert_eq!(
        host_history.first(),
        Some(&fixed_developer),
        "root compact reduce must retain developer fixed prefix outside h(PS)"
    );
    assert_eq!(
        host_history.get(1),
        Some(&fixed_agents),
        "root compact reduce must retain AGENTS fixed prefix outside h(PS)"
    );
    assert_eq!(
        variable_spine_items(&host_history),
        materialized,
        "root compact reduce success must leave variable ContextManager.items equal to h(PS)"
    );
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let rollout_replacement_history = resumed
        .history
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_ref(),
            _ => None,
        })
        .expect("native compact boundary should persist replacement_history");
    assert_eq!(
        variable_spine_items(rollout_replacement_history),
        materialized,
        "root compact reduce success must persist variable replacement_history equal to h(PS)"
    );
    assert_eq!(
        materialized.len(),
        1,
        "root compact h(PS) should publish the single root memory item"
    );
    assert!(
        materialized
            .iter()
            .any(|item| message_text_contains(item, "root compact publish body")),
        "root compact memory must contain the model-authored compact body"
    );
    assert!(
        !materialized
            .iter()
            .any(|item| message_text_contains(item, "fixed developer instruction outside PS")),
        "root compact memory must not archive fixed developer prefix"
    );
    assert!(
        !materialized
            .iter()
            .any(|item| message_text_contains(item, "fixed agents outside PS")),
        "root compact memory must not archive AGENTS fixed prefix"
    );
}

#[tokio::test]
async fn spine_root_compact_resume_preserves_fixed_prefix_outside_h_ps() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let source_rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("record initial history");

    let fixed_developer = ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "fixed resume developer instruction outside PS".to_string(),
        }],
        phase: None,
    };
    let fixed_agents = user_message(
        "# AGENTS.md instructions for /repo\n\n<INSTRUCTIONS>\nfixed resume agents outside PS\n</INSTRUCTIONS>",
    );
    let spine_source = vec![assistant_message("root compact resume body")];
    let mut replacement = vec![fixed_developer.clone(), fixed_agents.clone()];
    replacement.extend(spine_source.clone());

    session
        .replace_compacted_history(
            &turn_context,
            replacement,
            None,
            CompactedItem {
                message: "native compact carrier for resume contract".to_string(),
                replacement_history: None,
            },
            Some(spine_source),
        )
        .await
        .expect("install source root compact");
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("flush source rollout");

    let InitialHistory::Resumed(resumed) =
        RolloutRecorder::get_rollout_history(&source_rollout_path)
            .await
            .expect("read source rollout")
    else {
        panic!("expected resumed source rollout history");
    };
    let rollout_items = resumed.history;
    let raw_items = spine_raw_items_after_rollback(&rollout_items);
    let runtime = SpineRuntime::load_for_rollout_items(&source_rollout_path, &raw_items, &[])
        .expect("load source runtime")
        .expect("source sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize source h(PS)");

    let (mut resumed_session, _resumed_context, _rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("resumed session should be unique"),
    )
    .await;
    let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&source_rollout_path, &resumed_rollout_path, &raw_live);

    resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(source_rollout_path),
        }))
        .await
        .expect("resume root compact rollout");

    let host_history = resumed_session.clone_history().await.raw_items().to_vec();
    assert_eq!(
        host_history.first(),
        Some(&fixed_developer),
        "resume must retain developer fixed prefix outside h(PS)"
    );
    assert_eq!(
        host_history.get(1),
        Some(&fixed_agents),
        "resume must retain AGENTS fixed prefix outside h(PS)"
    );
    assert_eq!(
        variable_spine_items(&host_history),
        materialized,
        "resume must restore only variable history from h(PS)"
    );
}

#[tokio::test]
async fn native_compact_missing_usage_does_not_copy_stale_token_info_to_root_handoff() {
    let server = start_mock_server().await;
    let _responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("native-missing-usage-summary", "native compact summary"),
            serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": "native-missing-usage-response"
                }
            }),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session
        .record_conversation_items(
            &turn_context,
            &[user_message("prefix before native compact without usage")],
        )
        .await
        .expect("record prefix");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 98_765,
                total_tokens: 87_654,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }

    crate::compact::run_compact_task(
        Arc::clone(&session),
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: "compact without token usage".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await
    .expect("native compact succeeds");

    let store = SpineStore::for_rollout(&rollout_path).expect("spine store");
    assert_eq!(
        store
            .root_compact_next_open_tokens_for_test()
            .expect("root compact handoff tokens"),
        vec![(None, None)]
    );
}

#[tokio::test]
async fn replacement_history_validates_at_compact_boundary() {
    let server = start_mock_server().await;
    let responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("sequence-native-summary", "sequence native compact summary"),
            ev_completed("sequence-native-response"),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("sequence prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;

    let open_request = spine_call(SPINE_TOOL_OPEN, "sequence-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request(
            "sequence-open".to_string(),
            "sequence child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("sequence-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;

    let child_body = assistant_message("sequence child body");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record conversation items");
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;

    let close_request = spine_call(SPINE_TOOL_CLOSE, "sequence-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "sequence-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("sequence-close"),
    )
    .await
    .expect("commit close and record raw evidence");
    assert_eq!(
        responses_mock.requests().len(),
        0,
        "close(memory) reduce must not request a compact summary from the model"
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;

    crate::compact::run_compact_task(
        Arc::clone(&session),
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: "sequence native compact".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await
    .expect("native compact succeeds");
    assert_eq!(
        responses_mock.requests().len(),
        1,
        "native compact should issue the only model request in this scenario"
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let source_raw_items = spine_raw_items_after_rollback(&resumed.history);
    let source_runtime =
        SpineRuntime::load_for_rollout_items(&rollout_path, &source_raw_items, &[])
            .expect("load source spine runtime")
            .expect("source sidecar should exist");
    let source_materialized = source_runtime
        .materialize_variable_context_for_test(&source_raw_items)
        .expect("materialize source h(PS)");

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = source_raw_items
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&rollout_path, &resumed_rollout_path, &raw_live);

    resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: resumed.history,
            rollout_path: Some(rollout_path),
        }))
        .await
        .expect("record initial history");
    assert_eq!(
        resumed_session.clone_history().await.raw_items(),
        source_materialized.as_slice()
    );
}

#[tokio::test]
async fn replacement_history_not_compared_to_post_close_h_ps() {
    assert_resume_after_replacement_history_suffix_uses_sidecar_h_ps().await;
}

#[tokio::test]
async fn resume_final_history_is_sidecar_h_ps_after_replacement_history_suffix() {
    assert_resume_after_replacement_history_suffix_uses_sidecar_h_ps().await;
}

#[tokio::test]
async fn resume_does_not_compare_checkpoint_history_to_latest_h_ps() {
    assert_resume_after_replacement_history_suffix_uses_sidecar_h_ps().await;
}

async fn assert_resume_after_replacement_history_suffix_uses_sidecar_h_ps() {
    let server = start_mock_server().await;
    let responses_mock = mount_sse_sequence(
        &server,
        vec![sse(vec![
            ev_assistant_message("root-compact-summary", "native root compact summary"),
            ev_completed("root-compact-response"),
        ])],
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let precompact_items = vec![
        user_message("root compact pre item 1"),
        user_message("root compact pre item 2"),
        user_message("root compact pre item 3"),
    ];
    for item in &precompact_items {
        session
            .record_conversation_items(&turn_context, std::slice::from_ref(item))
            .await
            .expect("record conversation items");
    }

    crate::compact::run_compact_task(
        Arc::clone(&session),
        Arc::clone(&turn_context),
        vec![UserInput::Text {
            text: "compact before post-native close".to_string(),
            text_elements: Vec::new(),
        }],
    )
    .await
    .expect("native compact succeeds");

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    let replacement_history = resumed
        .history
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_ref(),
            _ => None,
        })
        .expect("native compact should persist replacement_history");
    assert_eq!(replacement_history, &materialized);
    assert_eq!(
        session.clone_history().await.raw_items(),
        materialized.as_slice()
    );
    assert_eq!(
        runtime.current_open_index().expect("open index"),
        materialized.len()
    );
    assert_eq!(materialized.len(), 1);

    let open_request = spine_call(SPINE_TOOL_OPEN, "post-native-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request(
            "post-native-open".to_string(),
            "post native child".to_string(),
        )
        .await
        .expect("stage post-native open");
    let open_output = function_output("post-native-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("open after native compact should use corrected root open index");

    let post_native_body = user_message("post native root work");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&post_native_body))
        .await
        .expect("record conversation items");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "post-native-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_close_control_request(
            "post-native-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage post-native close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("post-native-close"),
    )
    .await
    .expect("close after native compact should use corrected root open index");

    assert_eq!(
        responses_mock.requests().len(),
        1,
        "post-native close must use direct memory without a secondary compact request"
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let source_raw_items = spine_raw_items_after_rollback(&resumed.history);
    let source_runtime =
        SpineRuntime::load_for_rollout_items(&rollout_path, &source_raw_items, &[])
            .expect("load source spine runtime")
            .expect("source sidecar should exist");
    let source_materialized = source_runtime
        .materialize_variable_context_for_test(&source_raw_items)
        .expect("materialize source h(PS)");
    let replacement_history = resumed
        .history
        .iter()
        .find_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_ref(),
            _ => None,
        })
        .expect("native compact should persist replacement_history");
    assert_ne!(
        replacement_history, &source_materialized,
        "post-compact close should make latest h(PS) differ from the old compact checkpoint"
    );

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = source_raw_items
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&rollout_path, &resumed_rollout_path, &raw_live);

    resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: resumed.history,
            rollout_path: Some(rollout_path),
        }))
        .await
        .expect("record initial history");
    assert_eq!(
        resumed_session.clone_history().await.raw_items(),
        source_materialized.as_slice()
    );
}

#[tokio::test]
async fn spine_feature_off_records_spine_shaped_items_as_plain_history_without_sidecar() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .disable(Feature::SpineJit)
                .expect("disable spine feature");
        },
    )
    .await;
    assert!(session.spine.is_none());
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("feature off prefix");
    let open_request = spine_call(SPINE_TOOL_OPEN, "feature-off-open");
    let open_output = function_output("feature-off-open");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "feature-off-close");
    let close_output = function_output("feature-off-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record conversation items");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record conversation items");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_output))
        .await
        .expect("record conversation items");

    assert_eq!(
        session.clone_history().await.raw_items(),
        &[
            prefix.clone(),
            open_request.clone(),
            open_output.clone(),
            close_request.clone(),
            close_output.clone(),
        ]
    );
    assert!(!SpineStore::has_for_rollout(&rollout_path).expect("check sidecar"));
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let reconstructed = session
        .reconstruct_history_from_rollout(&turn_context, &resumed.history)
        .await;
    assert_eq!(
        reconstructed.history,
        vec![
            prefix,
            open_request,
            open_output,
            close_request,
            close_output
        ]
    );
    assert!(!reconstructed.used_replacement_history);
    assert!(reconstructed.spine_rollback_cuts.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_feature_off_does_not_overlay_spine_shaped_streaming_followup() -> anyhow::Result<()>
{
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .disable(Feature::SpineJit)
            .expect("disable spine feature");
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call_with_namespace(
                    "feature-off-spine-call",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_OPEN,
                    r#"{"summary":"should stay plain"}"#,
                ),
                ev_function_call_with_namespace(
                    "feature-off-spine-next",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_NEXT,
                    r#"{"summary":"should also stay plain"}"#,
                ),
                ev_completed("feature-off-spine-call-response"),
            ]),
            sse(vec![
                ev_assistant_message("feature-off-follow-up", "done"),
                ev_completed("feature-off-follow-up-response"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "emit a spine-shaped call while disabled".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => Some(()),
        _ => None,
    })
    .await;

    let requests = responses.requests();
    assert_eq!(
        requests.len(),
        2,
        "unknown function call should still trigger one base follow-up"
    );
    let follow_up_body = requests[1].body_json();
    let follow_up_input = follow_up_body["input"]
        .as_array()
        .expect("follow-up input array");
    let spine_shaped_call_items = follow_up_input
        .iter()
        .filter(|item| {
            item.get("call_id").and_then(serde_json::Value::as_str)
                == Some("feature-off-spine-call")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        spine_shaped_call_items.len(),
        2,
        "feature off should keep only the base unknown-call request/output, not add a Spine overlay: {follow_up_body}"
    );
    assert!(
        spine_shaped_call_items.iter().any(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
        }),
        "base follow-up should include the unknown function call request: {follow_up_body}"
    );
    assert!(
        spine_shaped_call_items.iter().any(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
        }),
        "base follow-up should include the unknown function call output: {follow_up_body}"
    );
    let second_spine_shaped_call_items = follow_up_input
        .iter()
        .filter(|item| {
            item.get("call_id").and_then(serde_json::Value::as_str)
                == Some("feature-off-spine-next")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        second_spine_shaped_call_items.len(),
        2,
        "feature off should keep the second base unknown-call request/output too: {follow_up_body}"
    );
    for call_id in ["feature-off-spine-call", "feature-off-spine-next"] {
        let output = follow_up_input
            .iter()
            .find(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
            })
            .unwrap_or_else(|| panic!("missing base output for {call_id}: {follow_up_body}"));
        assert!(
            !output.to_string().contains("mutually exclusive"),
            "feature off must not use Spine mutual-exclusion rejection for {call_id}: {output}"
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_spine_parser_control_calls_in_one_response_fail_before_tool_bodies()
-> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::SpineJit)
            .expect("enable spine feature");
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call_with_namespace(
                    "multi-open",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_OPEN,
                    r#"{"summary":"must not open"}"#,
                ),
                ev_function_call_with_namespace(
                    "multi-next",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_NEXT,
                    r#"{"summary":"must not advance"}"#,
                ),
                ev_shell_command_call("multi-shell", "printf multi-ordinary-tool"),
                ev_completed("multi-spine-control-response"),
            ]),
            sse(vec![
                ev_assistant_message("multi-spine-control-follow-up", "ack"),
                ev_completed("multi-spine-control-follow-up-response"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "try two spine control tools at once".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => Some(()),
        _ => None,
    })
    .await;

    let requests = responses.requests();
    assert_eq!(
        requests.len(),
        2,
        "conflicting Spine control calls should produce one corrective follow-up"
    );
    let follow_up = requests[1].body_json();
    let follow_up_items = follow_up["input"]
        .as_array()
        .expect("follow-up input should be an array");
    for call_id in ["multi-open", "multi-next"] {
        let output = follow_up_items
            .iter()
            .find(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
            })
            .unwrap_or_else(|| panic!("missing failure output for {call_id}: {follow_up}"));
        assert!(
            output.to_string().contains("mutually exclusive"),
            "unexpected failure output for {call_id}: {output}"
        );
        assert!(
            output
                .to_string()
                .contains("No Spine control action was applied"),
            "rejection output must say no Spine control action was applied for {call_id}: {output}"
        );
        assert!(
            output
                .to_string()
                .contains("Ordinary non-Spine tools may have run"),
            "rejection output must preserve ordinary-tool uncertainty for {call_id}: {output}"
        );
        assert!(
            output
                .to_string()
                .contains("retry with at most one of spine.open, spine.close, or spine.next"),
            "rejection output must ask the model to resend one control for {call_id}: {output}"
        );
        assert!(
            !output
                .to_string()
                .contains("No tool in this response was executed"),
            "rejection output must not claim ordinary tools were skipped for {call_id}: {output}"
        );
    }
    let shell_output = follow_up_items
        .iter()
        .find(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(serde_json::Value::as_str) == Some("multi-shell")
        })
        .unwrap_or_else(|| panic!("missing ordinary tool output: {follow_up}"));
    assert!(
        shell_output.to_string().contains("multi-ordinary-tool"),
        "ordinary tool body from a conflicting toolreq must execute normally: {shell_output}"
    );
    assert!(
        !shell_output.to_string().contains("mutually exclusive"),
        "ordinary tool output must not be replaced by Spine mutual-exclusion rejection: {shell_output}"
    );
    assert!(
        !follow_up.to_string().contains("Spine open accepted."),
        "spine.open body must not run before conflict detection: {follow_up}"
    );
    assert!(
        !follow_up.to_string().contains("Spine next accepted."),
        "spine.next body must not run before conflict detection: {follow_up}"
    );
    for call_id in ["multi-open", "multi-next", "multi-shell"] {
        let request_count = follow_up_items
            .iter()
            .filter(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
                    && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
            })
            .count();
        let output_count = follow_up_items
            .iter()
            .filter(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
            })
            .count();
        assert_eq!(
            request_count, 1,
            "follow-up should carry exactly one request for {call_id}: {follow_up}"
        );
        assert_eq!(
            output_count, 1,
            "follow-up should carry exactly one output for {call_id}: {follow_up}"
        );
    }

    let rollout_path = test
        .codex
        .rollout_path()
        .expect("test thread should have rollout persistence");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert_eq!(runtime.parse_stack_toolcall_leaf_count_for_test(), 1);
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize conflict history");
    let persisted_items = raw_items.iter().flatten().cloned().collect::<Vec<_>>();
    assert_eq!(
        materialized.len(),
        variable_spine_items(&persisted_items).len()
    );
    for call_id in ["multi-open", "multi-next"] {
        let persisted_output = function_output_text_by_call_id(&persisted_items, call_id);
        assert!(
            persisted_output.contains("mutually exclusive"),
            "raw audit should keep the original conflict response for {call_id}: {persisted_output}"
        );
        assert!(
            !persisted_output.contains("[TRIM_ID:"),
            "raw audit must not be rewritten with trim tags for {call_id}: {persisted_output}"
        );
        let materialized_output = function_output_text_by_call_id(&materialized, call_id);
        assert!(
            materialized_output.contains("mutually exclusive"),
            "visible h(PS) should keep the conflict rejection output: {materialized_output}"
        );
        assert!(
            !materialized_output.starts_with("[TRIM_ID: trim_"),
            "short conflict rejection output should not be forced into trim tagging: {materialized_output}"
        );
    }
    assert_eq!(
        function_output_text_by_call_id(&materialized, "multi-shell"),
        function_output_text_by_call_id(&persisted_items, "multi-shell")
    );
    assert!(
        !tree.contains("must not open") && !tree.contains("must not advance"),
        "conflicting control calls must not mutate Spine tree: {tree}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflicting_spine_controls_preserve_custom_and_tool_search_output_carriers()
-> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::SpineJit)
            .expect("enable spine feature");
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call_with_namespace(
                    "carrier-open",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_OPEN,
                    r#"{"summary":"must not open"}"#,
                ),
                ev_function_call_with_namespace(
                    "carrier-next",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_NEXT,
                    r#"{"summary":"must not advance"}"#,
                ),
                ev_custom_tool_call(
                    "carrier-custom",
                    "exec",
                    r#"{"cmd":"printf ordinary-custom-tool"}"#,
                ),
                ev_tool_search_call(
                    "carrier-search",
                    &json!({
                        "query": "ordinary tool search",
                        "limit": 1,
                    }),
                ),
                ev_completed("carrier-conflict-response"),
            ]),
            sse(vec![
                ev_assistant_message("carrier-conflict-follow-up", "ack"),
                ev_completed("carrier-conflict-follow-up-response"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "try conflicting spine controls with mixed carriers".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => Some(()),
        _ => None,
    })
    .await;

    let requests = responses.requests();
    assert_eq!(
        requests.len(),
        2,
        "conflict should produce exactly one corrective follow-up"
    );
    let follow_up = requests[1].body_json();
    let input = follow_up["input"]
        .as_array()
        .expect("follow-up input should be an array");
    let item_with_type_and_call_id = |item_type: &str, call_id: &str| {
        input
            .iter()
            .find(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some(item_type)
                    && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
            })
            .unwrap_or_else(|| {
                panic!("missing {item_type} for {call_id} in follow-up: {follow_up}")
            })
    };

    for call_id in ["carrier-open", "carrier-next"] {
        let output = item_with_type_and_call_id("function_call_output", call_id);
        assert!(
            output.to_string().contains("mutually exclusive"),
            "Spine control rejection must explain conflict for {call_id}: {output}"
        );
    }
    let custom_output = item_with_type_and_call_id("custom_tool_call_output", "carrier-custom");
    assert!(
        !custom_output.to_string().contains("mutually exclusive"),
        "ordinary custom output must not be replaced by Spine conflict rejection: {custom_output}"
    );
    let search_output = item_with_type_and_call_id("tool_search_output", "carrier-search");
    assert_eq!(search_output["execution"], "client");
    assert_eq!(search_output["status"], "completed");
    assert!(
        !search_output.to_string().contains("mutually exclusive"),
        "ordinary tool-search output must not be replaced by Spine conflict rejection: {search_output}"
    );
    assert!(
        input.iter().all(|item| {
            !(item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
                && matches!(
                    item.get("call_id").and_then(serde_json::Value::as_str),
                    Some("carrier-custom" | "carrier-search")
                ))
        }),
        "custom/tool-search outputs must not be downgraded to function_call_output: {follow_up}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_tree_runs_normally_with_conflicting_spine_controls() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::SpineJit)
            .expect("enable spine feature");
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call_with_namespace(
                    "tree-conflict-open",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_OPEN,
                    r#"{"summary":"must not open from tree conflict"}"#,
                ),
                ev_function_call_with_namespace(
                    "tree-conflict-next",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_NEXT,
                    r#"{"summary":"must not advance from tree conflict"}"#,
                ),
                ev_function_call_with_namespace(
                    "tree-conflict-tree",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_TREE,
                    "{}",
                ),
                ev_completed("tree-conflict-response"),
            ]),
            sse(vec![
                ev_assistant_message("tree-conflict-follow-up", "ack"),
                ev_completed("tree-conflict-follow-up-response"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "try conflicting spine controls with tree".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => Some(()),
        _ => None,
    })
    .await;

    let requests = responses.requests();
    assert_eq!(
        requests.len(),
        2,
        "conflicting controls plus spine.tree should produce one corrective follow-up"
    );
    let follow_up = requests[1].body_json();
    let follow_up_items = follow_up["input"]
        .as_array()
        .expect("follow-up input should be an array");
    let output_with_call_id = |call_id: &str| {
        follow_up_items
            .iter()
            .find(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
                    && item.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
            })
            .unwrap_or_else(|| panic!("missing function output for {call_id}: {follow_up}"))
    };

    for call_id in ["tree-conflict-open", "tree-conflict-next"] {
        let output = output_with_call_id(call_id);
        assert!(
            output.to_string().contains("mutually exclusive"),
            "conflicting Spine control must receive rejection for {call_id}: {output}"
        );
    }
    let tree_output = output_with_call_id("tree-conflict-tree");
    assert!(
        tree_output.to_string().contains("Cursor: 1.1"),
        "spine.tree must execute normally and return the current tree: {tree_output}"
    );
    assert!(
        !tree_output.to_string().contains("mutually exclusive"),
        "spine.tree output must not be replaced by conflict rejection: {tree_output}"
    );
    assert!(
        !follow_up.to_string().contains("Spine open accepted."),
        "spine.open body must not run before conflict detection: {follow_up}"
    );
    assert!(
        !follow_up.to_string().contains("Spine next accepted."),
        "spine.next body must not run before conflict detection: {follow_up}"
    );

    let rollout_path = test
        .codex
        .rollout_path()
        .expect("test thread should have rollout persistence");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Cursor: 1.1"), "{tree}");
    assert_eq!(runtime.parse_stack_toolcall_leaf_count_for_test(), 1);
    assert!(
        !tree.contains("must not open from tree conflict")
            && !tree.contains("must not advance from tree conflict"),
        "conflicting control calls must not mutate Spine tree: {tree}"
    );
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize conflict history");
    let persisted_items = raw_items.iter().flatten().cloned().collect::<Vec<_>>();
    assert_eq!(
        materialized.len(),
        variable_spine_items(&persisted_items).len()
    );
    for call_id in ["tree-conflict-open", "tree-conflict-next"] {
        let persisted_output = function_output_text_by_call_id(&persisted_items, call_id);
        assert!(
            persisted_output.contains("mutually exclusive"),
            "raw audit should keep the original conflict response for {call_id}: {persisted_output}"
        );
        assert!(
            !persisted_output.contains("[TRIM_ID:"),
            "raw audit must not be rewritten with trim tags for {call_id}: {persisted_output}"
        );
        let materialized_output = function_output_text_by_call_id(&materialized, call_id);
        assert!(
            materialized_output.contains("mutually exclusive"),
            "visible h(PS) should keep the conflict rejection output: {materialized_output}"
        );
        assert!(
            !materialized_output.starts_with("[TRIM_ID: trim_"),
            "short conflict rejection output should not be forced into trim tagging: {materialized_output}"
        );
    }
    assert_eq!(
        function_output_text_by_call_id(&materialized, "tree-conflict-tree"),
        function_output_text_by_call_id(&persisted_items, "tree-conflict-tree")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_control_with_ordinary_tool_call_commits_grouped_toolcall_leaf() -> anyhow::Result<()>
{
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::SpineJit)
            .expect("enable spine feature");
        config.model_provider.supports_websockets = false;
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call_with_namespace(
                    "mixed-close",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_CLOSE,
                    r#"{"memory":"grouped close memory"}"#,
                ),
                ev_shell_command_call("mixed-shell", "printf grouped-ordinary-tool"),
                ev_completed("mixed-control-and-tool-response"),
            ]),
            sse(vec![
                ev_assistant_message("mixed-follow-up", "done"),
                ev_completed("mixed-follow-up-response"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "grouped close ordinary work".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;

    let last_agent_message = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(turn_complete) => turn_complete.last_agent_message.clone(),
        _ => None,
    })
    .await;
    assert_eq!(last_agent_message.as_str(), "done");

    let requests = responses.requests();
    assert_eq!(
        requests.len(),
        2,
        "expected sampling and follow-up requests without secondary close memory request"
    );
    assert!(!requests[0].body_contains_text("---------- SPINE MEMORY COMPACT ----------"));
    assert!(!requests[1].body_contains_text("---------- SPINE MEMORY COMPACT ----------"));

    let follow_up = &requests[1];
    assert!(follow_up.has_function_call("mixed-close"));
    assert!(follow_up.has_function_call("mixed-shell"));
    let close_text = follow_up
        .function_call_output_text("mixed-close")
        .expect("follow-up should include the committed close output");
    assert!(
        close_text == "Spine close accepted.",
        "grouped close follow-up must carry the simple control output: {close_text}"
    );
    assert!(
        follow_up
            .function_call_output_text("mixed-shell")
            .is_some_and(|text| text.contains("grouped-ordinary-tool")),
        "follow-up must preserve the ordinary tool output: {}",
        follow_up.body_json()
    );

    let rollout_path = test
        .codex
        .rollout_path()
        .expect("test thread should have rollout persistence");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    assert!(
        matches!(
            materialized.as_slice(),
            [
                ResponseItem::Message { content, .. },
                ResponseItem::FunctionCall { call_id: close_call, .. },
                ResponseItem::FunctionCall { call_id: shell_call, .. },
                ResponseItem::FunctionCallOutput { call_id: close_output, .. },
                ResponseItem::FunctionCallOutput { call_id: shell_output, .. },
                ResponseItem::Message { role, .. },
            ] if content.iter().any(|item| matches!(
                    item,
                    ContentItem::InputText { text }
                        if text.contains("grouped close memory")
                ))
                && close_call == "mixed-close"
                && shell_call == "mixed-shell"
                && close_output == "mixed-close"
                && shell_output == "mixed-shell"
                && role == "assistant"
        ),
        "materialized h(PS) must be memory || complete grouped toolcall || follow-up message: {materialized:#?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_spine_control_overlay_does_not_duplicate_followup_prompt() -> anyhow::Result<()>
{
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::SpineJit)
            .expect("enable spine feature");
        config.model_provider.supports_websockets = false;
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call_with_namespace(
                    "overlay-open-once",
                    SPINE_NAMESPACE,
                    SPINE_TOOL_OPEN,
                    r#"{"summary":"overlay child"}"#,
                ),
                ev_completed("overlay-open-response"),
            ]),
            sse(vec![
                ev_assistant_message("overlay-follow-up", "done"),
                ev_completed("overlay-follow-up-response"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "open a spine child then continue".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => Some(()),
        _ => None,
    })
    .await;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2, "expected sampling and follow-up");
    let follow_up = requests[1].body_json();
    let follow_up_items = follow_up["input"]
        .as_array()
        .expect("follow-up input should be an array");
    let request_count = follow_up_items
        .iter()
        .filter(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
                && item.get("call_id").and_then(serde_json::Value::as_str)
                    == Some("overlay-open-once")
        })
        .count();
    let output_count = follow_up_items
        .iter()
        .filter(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(serde_json::Value::as_str)
                    == Some("overlay-open-once")
        })
        .count();
    assert_eq!(
        request_count, 1,
        "completed Spine control request must appear exactly once from durable h(PS), not stale overlay: {follow_up}"
    );
    assert_eq!(
        output_count, 1,
        "completed Spine control output must appear exactly once from durable h(PS), not stale overlay: {follow_up}"
    );
    let output_text = follow_up_items
        .iter()
        .find_map(|item| {
            (item.get("type").and_then(serde_json::Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(serde_json::Value::as_str)
                    == Some("overlay-open-once"))
            .then(|| item.to_string())
        })
        .expect("follow-up should include overlay-open-once output");
    assert!(
        output_text.contains("Spine open accepted."),
        "follow-up must preserve the simple control output: {follow_up}"
    );
    assert!(
        !output_text.contains("Cursor: 1.1.1"),
        "control tool output should not carry rendered tree state: {follow_up}"
    );

    let rollout_path = test
        .codex
        .rollout_path()
        .expect("test thread should have rollout persistence");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(
        runtime.parse_stack_toolcall_leaf_count_for_test(),
        1,
        "open control transaction must be one durable toolcall leaf"
    );

    Ok(())
}

#[tokio::test]
async fn init_new_session_creates_root_and_1_1() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    session.on_init().await.expect("initialize spine");
    let tree_before = {
        session
            .spine
            .as_ref()
            .expect("spine enabled")
            .lock()
            .await
            .runtime()
            .expect("runtime exists")
            .render_tree()
            .expect("render tree before")
    };
    let store = SpineStore::for_rollout(&rollout_path).expect("spine store");
    let events_before = store.event_count_for_test().expect("events before");
    assert_eq!(
        store
            .initial_checkpoint_identity_for_test()
            .expect("read initial checkpoint"),
        ("initial".to_string(), "1.1".to_string())
    );
    assert!(tree_before.contains("Cursor: 1.1"), "{tree_before}");

    let tree_from_tool = session.spine_tree().await.expect("tree");
    let events_after = store.event_count_for_test().expect("events after");
    assert!(
        tree_from_tool.starts_with(&tree_before),
        "tool tree should preserve pure runtime tree prefix\nbefore:\n{tree_before}\nafter:\n{tree_from_tool}"
    );
    assert_eq!(events_after, events_before);
}

#[tokio::test]
async fn spine_open_control_toolcall_is_durable_context_history() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before control carrier");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "overlay-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "overlay-open".to_string(),
            "overlay child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("overlay-open");
    let recorded_open_output =
        commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, open_output)
            .await
            .expect("commit and record open output");

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0],
        anchored_user_message(1, "prefix before control carrier")
    );
    assert_eq!(items[1], open_request);
    assert_eq!(items[2], recorded_open_output);

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(resumed.history.iter().any(|item| {
        matches!(item, RolloutItem::ResponseItem(ResponseItem::FunctionCall { call_id, .. }) if call_id == "overlay-open")
    }));
    assert!(resumed.history.iter().any(|item| {
        matches!(item, RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, .. }) if call_id == "overlay-open")
    }));

    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let materialized = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize h(PS)");
    assert_eq!(materialized, items);
}

#[tokio::test]
async fn spine_trim_rewrites_visible_history_and_preserves_raw_tool_output() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config
                .features
                .enable(Feature::SpineTrim)
                .expect("enable spine trim");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let request = function_call("shell_command", "long-tool");
    let long_text = trim_candidate_text("important raw output ");
    let output = function_output_with_text("long-tool", &long_text);
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&request))
        .await
        .expect("record ordinary request");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, output.clone())
        .await
        .expect("commit ordinary output");

    let tagged_history = session.clone_history().await.raw_items().to_vec();
    assert_eq!(tagged_history[0], request);
    assert!(
        function_output_text(&tagged_history[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "long output should be tagged before trim: {:?}",
        tagged_history[1]
    );
    assert!(
        function_output_text(&tagged_history[1]).contains(&long_text),
        "tagging must keep the original output visible until trim"
    );

    let trim_request = ResponseItem::FunctionCall {
        id: None,
        name: SPINE_TOOL_TRIM.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: json!({"TRIM_ID": "trim_0", "op": "snip"}).to_string(),
        call_id: "trim-long".to_string(),
    };
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&trim_request))
        .await
        .expect("record trim request");
    let outcome = session
        .trim_spine_tool_response("trim_0".to_string())
        .await
        .expect("trim previous tool response");
    assert_eq!(
        outcome,
        crate::spine::SpineTrimOutcome::Cleared {
            trim_id: "trim_0".to_string()
        }
    );
    let trim_output = function_output_with_text("trim-long", "Trimmed tool response trim_0.");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        trim_output.clone(),
    )
    .await
    .expect("commit trim output");

    let visible_history = session.clone_history().await.raw_items().to_vec();
    assert_eq!(visible_history[0], request);
    assert_eq!(
        function_output_text(&visible_history[1]),
        "[Old tool result content cleared]"
    );
    assert!(
        !function_output_text(&visible_history[1]).contains("[TRIM_ID:"),
        "cleared target output must replace the whole visible body, including the trim tag"
    );
    assert!(
        !function_output_text(&visible_history[1]).contains(&long_text),
        "cleared target output must not retain the original long body"
    );
    assert_eq!(visible_history[2], trim_request);
    assert_eq!(visible_history[3], trim_output);

    let next_prompt_history = session
        .clone_history()
        .await
        .for_prompt(&turn_context.model_info.input_modalities);
    assert_eq!(
        function_output_text_by_call_id(&next_prompt_history, "long-tool"),
        "[Old tool result content cleared]",
        "next LLM-visible prompt history must see only the cleared placeholder for the target"
    );
    assert!(
        !function_output_text_by_call_id(&next_prompt_history, "long-tool").contains("[TRIM_ID:"),
        "next LLM-visible prompt history must not retain the cleared target trim tag"
    );
    assert!(
        !function_output_text_by_call_id(&next_prompt_history, "long-tool").contains(&long_text),
        "next LLM-visible prompt history must not retain the cleared target body"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(resumed.history.iter().any(|item| {
        matches!(
            item,
            RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, output })
                if call_id == "long-tool" && output.text_content() == Some(long_text.as_str())
        )
    }));

    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load replayed runtime")
        .expect("spine sidecar exists");
    assert_eq!(runtime.parse_stack_toolcall_leaf_count_for_test(), 2);
    let replayed_visible = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize replayed trim projection");
    assert_eq!(replayed_visible, visible_history);
    assert_eq!(
        function_output_text_by_call_id(&replayed_visible, "long-tool"),
        "[Old tool result content cleared]",
        "replayed h(PS) must see only the cleared placeholder for the target"
    );
}

#[tokio::test]
async fn spine_trim_tail_guidance_overlay_lists_current_targets_without_persisting() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config
                .features
                .enable(Feature::SpineTrim)
                .expect("enable spine trim");
            config
                .features
                .enable(Feature::SpineTrimTailGuidance)
                .expect("enable spine trim tail guidance");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let request = function_call("shell_command", "tail-guidance-long-tool");
    let long_text = format!(
        "Exit code: 0\n{}",
        trim_candidate_text("tail guidance raw output with useful head ")
    );
    let output = function_output_with_text("tail-guidance-long-tool", &long_text);
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&request))
        .await
        .expect("record ordinary request");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, output.clone())
        .await
        .expect("commit ordinary output");

    let history_before = session.clone_history().await.raw_items().to_vec();
    assert!(
        function_output_text(&history_before[1]).starts_with("[TRIM_ID: trim_0]\n"),
        "legacy visible tag remains unchanged: {:?}",
        history_before[1]
    );

    let overlay = session
        .spine_trim_targets_prompt_overlay()
        .await
        .expect("trim target overlay should be present");
    let ResponseItem::Message { content, role, .. } = overlay.item else {
        panic!("expected developer message overlay");
    };
    assert_eq!(role, "developer");
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("expected single input text overlay item: {content:?}");
    };
    assert!(
        text.starts_with(
            "At natural Spine boundaries, close/next with compact continuation memory, or open a child for the next smaller concrete goal. For the latest tool outputs listed below, trim irrelevant noisy content now, or slice to keep only needed evidence; preserve any facts needed for continuation before trimming.\n<current_trim_targets>\n"
        ),
        "{text}"
    );
    assert!(text.contains(r#"0 id="trim_0" bytes=""#), "{text}");
    assert!(
        text.contains(r#"head="Exit code: 0 tail guidance raw output"#),
        "{text}"
    );
    assert!(text.ends_with("\n</current_trim_targets>"), "{text}");
    assert!(!text.contains("valid_for"), "{text}");
    assert!(!text.contains("rule"), "{text}");

    assert_eq!(
        session.clone_history().await.raw_items(),
        history_before,
        "prompt-only trim target overlay must not mutate conversation history"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let rollout_text = format!("{:?}", resumed.history);
    assert!(
        !rollout_text.contains("<current_trim_targets>"),
        "prompt-only overlay must not be durable rollout history: {rollout_text}"
    );
}

#[tokio::test]
async fn spine_trim_slice_rewrites_visible_history_and_preserves_raw_tool_output() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config
                .features
                .enable(Feature::SpineTrim)
                .expect("enable spine trim");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let request = function_call("shell_command", "long-tool");
    let long_text = format!(
        "{}abc<needle>xyz{}",
        trim_candidate_text("left "),
        trim_candidate_text(" right")
    );
    let output = function_output_with_text("long-tool", &long_text);
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&request))
        .await
        .expect("record ordinary request");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, output.clone())
        .await
        .expect("commit ordinary output");

    let trim_request = ResponseItem::FunctionCall {
        id: None,
        name: SPINE_TOOL_TRIM.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: json!({
            "TRIM_ID": "trim_0",
            "op": "slice",
            "anchor": "<needle>",
            "preceding": 3,
            "following": 3
        })
        .to_string(),
        call_id: "slice-long".to_string(),
    };
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&trim_request))
        .await
        .expect("record trim request");
    let outcome = session
        .slice_spine_tool_response_anchor("trim_0".to_string(), "<needle>".to_string(), 3, 3)
        .await
        .expect("slice previous tool response");
    assert_eq!(
        outcome,
        crate::spine::SpineTrimOutcome::Sliced {
            trim_id: "trim_0".to_string()
        }
    );
    let trim_output = function_output_with_text("slice-long", "Sliced tool response trim_0.");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        trim_output.clone(),
    )
    .await
    .expect("commit trim output");

    let visible_history = session.clone_history().await.raw_items().to_vec();
    assert_eq!(function_output_text(&visible_history[1]), "abc<needle>xyz");
    assert!(!function_output_text(&visible_history[1]).contains("[TRIM_ID:"));
    assert!(!function_output_text(&visible_history[1]).contains("left left"));
    assert_eq!(visible_history[2], trim_request);
    assert_eq!(visible_history[3], trim_output);

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(resumed.history.iter().any(|item| {
        matches!(
            item,
            RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, output })
                if call_id == "long-tool" && output.text_content() == Some(long_text.as_str())
        )
    }));

    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load replayed runtime")
        .expect("spine sidecar exists");
    let replayed_visible = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize replayed trim projection");
    assert_eq!(replayed_visible, visible_history);
    assert_eq!(
        function_output_text_by_call_id(&replayed_visible, "long-tool"),
        "abc<needle>xyz"
    );
}

#[tokio::test]
async fn spine_trim_slice_after_prior_close_uses_rollout_raw_trace() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config
                .features
                .enable(Feature::SpineTrim)
                .expect("enable spine trim");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "trim-close-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "trim-close-open".to_string(),
            "trim close child".to_string(),
        )
        .await
        .expect("stage open");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("trim-close-open"),
    )
    .await
    .expect("commit open output");

    let folded_inner = assistant_message("inner raw body folded by close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&folded_inner))
        .await
        .expect("record inner item");

    let close_request = spine_call(SPINE_TOOL_CLOSE, "trim-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "trim-close".to_string(),
            "trim close memory".to_string(),
        )
        .await
        .expect("stage close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("trim-close"),
    )
    .await
    .expect("commit close output");

    let after_close_history = session.clone_history().await.raw_items().to_vec();
    assert!(
        after_close_history.len() < 6,
        "close should compact visible history: {after_close_history:#?}"
    );
    assert!(
        !after_close_history.contains(&folded_inner),
        "inner raw item should be folded into memory"
    );

    let request = function_call("shell_command", "post-close-long-tool");
    let long_text = format!(
        "{}abc<needle>xyz{}",
        trim_candidate_text("left "),
        trim_candidate_text(" right")
    );
    let output = function_output_with_text("post-close-long-tool", &long_text);
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&request))
        .await
        .expect("record post-close request");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, output.clone())
        .await
        .expect("commit post-close output");

    let tagged_history = session.clone_history().await.raw_items().to_vec();
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(tagged_resumed) =
        RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .expect("read tagged rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let tagged_raw_items = spine_raw_items_after_rollback(&tagged_resumed.history);
    assert!(
        tagged_raw_items.len() > tagged_history.len(),
        "prior close should make raw trace longer than visible history: raw={} visible={}",
        tagged_raw_items.len(),
        tagged_history.len()
    );
    let tagged_body = function_output_text_by_call_id(&tagged_history, "post-close-long-tool");
    let trim_id = tagged_body
        .strip_prefix("[TRIM_ID: ")
        .and_then(|rest| rest.split_once("]\n"))
        .map(|(trim_id, _)| trim_id.to_string())
        .unwrap_or_else(|| panic!("missing trim tag in post-close output: {tagged_body:?}"));
    assert!(
        tagged_body.contains(&long_text),
        "tagging must keep the original output visible until trim"
    );

    let trim_request = ResponseItem::FunctionCall {
        id: None,
        name: SPINE_TOOL_TRIM.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: json!({
            "TRIM_ID": trim_id,
            "op": "slice",
            "anchor": "<needle>",
            "preceding": 3,
            "following": 3
        })
        .to_string(),
        call_id: "slice-post-close-long".to_string(),
    };
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&trim_request))
        .await
        .expect("record trim request");
    let outcome = session
        .slice_spine_tool_response_anchor(trim_id.clone(), "<needle>".to_string(), 3, 3)
        .await
        .expect("slice post-close tool response");
    assert_eq!(
        outcome,
        crate::spine::SpineTrimOutcome::Sliced {
            trim_id: trim_id.clone()
        }
    );
    let trim_output = function_output_with_text(
        "slice-post-close-long",
        &format!("Sliced tool response {trim_id}."),
    );
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        trim_output.clone(),
    )
    .await
    .expect("commit trim output");

    let visible_history = session.clone_history().await.raw_items().to_vec();
    assert_eq!(
        function_output_text_by_call_id(&visible_history, "post-close-long-tool"),
        "abc<needle>xyz"
    );
    assert!(
        !function_output_text_by_call_id(&visible_history, "post-close-long-tool")
            .contains("[TRIM_ID:")
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load replayed runtime")
        .expect("spine sidecar exists");
    let replayed_visible = runtime
        .materialize_variable_context_for_test(&raw_items)
        .expect("materialize replayed trim projection");
    assert_eq!(replayed_visible, visible_history);
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn spine_trim_replay_patches_post_close_target_by_call_id() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config
                .features
                .enable(Feature::SpineTrim)
                .expect("enable spine trim");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let open_request = spine_call(SPINE_TOOL_OPEN, "stale-trim-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "stale-trim-open".to_string(),
            "stale trim child".to_string(),
        )
        .await
        .expect("stage open");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("stale-trim-open"),
    )
    .await
    .expect("commit open output");

    let stale_request = function_call("shell_command", "stale-long-tool");
    let stale_body = trim_candidate_text("stale output folded by close ");
    let stale_output = function_output_with_text("stale-long-tool", &stale_body);
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&stale_request))
        .await
        .expect("record stale request");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        stale_output.clone(),
    )
    .await
    .expect("commit stale output");
    let tagged_before_close = session.clone_history().await.raw_items().to_vec();
    assert!(
        function_output_text_by_call_id(&tagged_before_close, "stale-long-tool")
            .starts_with("[TRIM_ID: trim_"),
        "pre-close long output should be tagged"
    );

    let close_request = spine_call(SPINE_TOOL_CLOSE, "stale-trim-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "stale-trim-close".to_string(),
            "memory after stale trim target".to_string(),
        )
        .await
        .expect("stage close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("stale-trim-close"),
    )
    .await
    .expect("commit close output");
    let after_close = session.clone_history().await.raw_items().to_vec();
    assert!(
        !after_close.iter().any(|item| matches!(
            item,
            ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "stale-long-tool"
        )),
        "close should fold the old trim target out of current visible history"
    );

    let current_request = function_call("shell_command", "current-long-tool");
    let current_body = trim_candidate_text("current visible output after close ");
    let current_output = function_output_with_text("current-long-tool", &current_body);
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&current_request))
        .await
        .expect("record current request");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        current_output.clone(),
    )
    .await
    .expect("commit current output");

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let visible_before_reapply = session.clone_history().await.raw_items().to_vec();
    assert!(
        raw_items.len() > visible_before_reapply.len(),
        "close should make raw trace longer than current visible history"
    );

    let replayed = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load replayed runtime")
        .expect("spine sidecar exists");
    let updates = replayed
        .current_trim_body_updates(&raw_items)
        .expect("build trim body updates");
    assert!(
        updates
            .iter()
            .any(|update| update.call_id == "current-long-tool"),
        "replay should produce a body update for the current post-close target"
    );
    {
        let mut state = session.state.lock().await;
        Session::apply_spine_trim_body_updates_to_locked_state_for_test(&mut state, updates)
            .expect("post-close trim target must patch by call_id in short history");
    }

    let visible_after_reapply = session.clone_history().await.raw_items().to_vec();
    assert_eq!(
        visible_after_reapply, visible_before_reapply,
        "reapplying replayed trim projection should be idempotent on short history"
    );
    assert!(
        function_output_text_by_call_id(&visible_after_reapply, "current-long-tool")
            .starts_with("[TRIM_ID: trim_"),
        "current visible post-close output should remain tagged by call_id"
    );
}

#[tokio::test]
async fn spine_trim_candidate_grouped_jit_on_patches_only_target_bodies() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config
                .features
                .enable(Feature::SpineTrim)
                .expect("enable spine trim");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    for index in 0..15 {
        session
            .record_conversation_items(
                &turn_context,
                std::slice::from_ref(&assistant_message(&format!("prelude {index}"))),
            )
            .await
            .expect("record prelude message");
    }
    let before_toolcall = session.clone_history().await.raw_items().to_vec();
    assert_eq!(before_toolcall.len(), 15);
    let context_13_before = before_toolcall[13].clone();

    let requests = [
        function_call("shell_command", "call-a"),
        function_call("shell_command", "call-b"),
        function_call("shell_command", "call-c"),
        function_call("shell_command", "call-d"),
    ];
    session
        .record_conversation_items(&turn_context, &requests)
        .await
        .expect("record grouped requests");
    let outputs = [
        function_output_with_text("call-a", &trim_candidate_text("target output a ")),
        function_output_with_text("call-b", &trim_candidate_text("target output b ")),
        function_output_with_text("call-c", &trim_candidate_text("target output c ")),
        function_output_with_text("call-d", "short output d"),
    ];
    session
        .test_on_toolcall(
            &turn_context,
            ToolCallEvidence::grouped_as_ordinary(
                "call-a",
                &[
                    "call-a".to_string(),
                    "call-b".to_string(),
                    "call-c".to_string(),
                    "call-d".to_string(),
                ],
                &outputs,
            ),
        )
        .await
        .expect("grouped ordinary trim candidate commit");

    let visible = session.clone_history().await.raw_items().to_vec();
    assert_eq!(visible.len(), 23);
    assert_eq!(
        visible[13], context_13_before,
        "trim candidate patch must not touch unrelated assistant context 13"
    );
    assert!(
        function_output_text(&visible[19]).starts_with("[TRIM_ID: trim_0]\n"),
        "first target output should be tagged"
    );
    assert!(
        function_output_text(&visible[20]).starts_with("[TRIM_ID: trim_1]\n"),
        "second target output should be tagged"
    );
    assert!(
        function_output_text(&visible[21]).starts_with("[TRIM_ID: trim_2]\n"),
        "third target output should be tagged"
    );
    assert_eq!(
        function_output_text(&visible[22]),
        "short output d",
        "short non-candidate output should remain untagged"
    );
}

#[tokio::test]
async fn spine_trim_only_session_tags_outputs_and_fork_suffix_without_jit_tree() {
    let (mut source_session, source_context, _source_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineTrim)
                    .expect("enable spine trim");
            },
        )
        .await;
    let source_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut source_session).expect("source session should be unique"),
    )
    .await;
    source_session
        .on_init()
        .await
        .expect("initialize trim-only spine");
    let source_store = SpineStore::for_rollout(&source_rollout_path).expect("source store");
    assert!(
        !source_store.tree_path_for_test().exists(),
        "spine_trim alone must not create the JIT parser tree ledger"
    );
    assert!(
        !source_store.initial_checkpoint_path_for_test().exists(),
        "spine_trim alone must not create JIT checkpoints"
    );

    let source_output = function_output_with_text(
        "source-long-tool",
        &trim_candidate_text("source trim-only output "),
    );
    source_session
        .record_conversation_items(&source_context, std::slice::from_ref(&source_output))
        .await
        .expect("record source trim-only output");
    source_session
        .apply_spine_trim_projection_if_available()
        .await
        .expect("apply source trim projection");
    let source_visible = source_session.clone_history().await.raw_items().to_vec();
    assert!(
        function_output_text(&source_visible[0]).starts_with("[TRIM_ID: trim_1]\n"),
        "trim-only source output should receive a TRIM_ID"
    );
    assert!(
        !source_store.tree_path_for_test().exists(),
        "trim-only tagging must not create the JIT parser tree ledger"
    );

    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout_path, 1)
        .expect("capture trim-only boundary")
        .expect("source sidecar exists");
    let (mut child_session, _child_context, _child_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineTrim)
                    .expect("enable spine trim");
            },
        )
        .await;
    let child_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut child_session).expect("child session should be unique"),
    )
    .await;
    let child_output = function_output_with_text(
        "child-long-tool",
        &trim_candidate_text("child trim-only output "),
    );
    let child_raw_items = vec![Some(source_output.clone()), Some(child_output.clone())];
    child_session
        .clone_spine_sidecar_for_fork(&boundary, &child_raw_items)
        .await
        .expect("clone trim-only sidecar and replay child suffix");

    let child_store = SpineStore::for_rollout(&child_rollout_path).expect("child store");
    assert!(
        !child_store.tree_path_for_test().exists(),
        "trim-only fork clone must not create the JIT parser tree ledger"
    );
    let child_projected = {
        let spine = child_session.spine.as_ref().expect("child spine runtime");
        let guard = spine.lock().await;
        let runtime = guard.runtime().expect("child runtime should be installed");
        runtime
            .project_raw_history_with_trim(&[source_output, child_output])
            .expect("project child trim-only history")
    };
    assert!(
        function_output_text(&child_projected[0]).starts_with("[TRIM_ID: trim_1]\n"),
        "forked prefix trim id should remain visible"
    );
    assert!(
        function_output_text(&child_projected[1]).starts_with("[TRIM_ID: trim_3]\n"),
        "fork replayed suffix should allocate a non-colliding trim id"
    );
}

#[tokio::test]
async fn spine_trim_only_local_patch_uses_call_id_with_fixed_prefix() {
    let (mut session, turn_context, _rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineTrim)
                .expect("enable spine trim");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session.on_init().await.expect("initialize trim-only spine");

    let fixed_prefix = developer_message("fixed developer prefix before trim-only output");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&fixed_prefix))
        .await
        .expect("record fixed prefix");
    let long_output = function_output_with_text(
        "trim-only-fixed-prefix",
        &trim_candidate_text("fixed prefix trim output "),
    );
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&long_output))
        .await
        .expect("record trim-only output");

    session
        .apply_spine_trim_projection_if_available()
        .await
        .expect("apply trim-only local patch");

    let visible = session.clone_history().await.raw_items().to_vec();
    assert_eq!(
        visible[0], fixed_prefix,
        "trim-only local patch must not mutate fixed prefix items"
    );
    assert!(
        function_output_text(&visible[1]).starts_with("[TRIM_ID: trim_1]\n"),
        "trim-only local patch should tag the target tool output after a fixed prefix"
    );

    let store = SpineStore::for_rollout(&rollout_path).expect("trim-only store");
    assert!(
        !store.tree_path_for_test().exists(),
        "trim-only local patch must not create or depend on the JIT parser tree ledger"
    );
}

#[tokio::test]
async fn spine_trim_only_head_fork_installs_runtime_without_jit_tree() {
    let (mut source_session, source_context, _source_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineTrim)
                    .expect("enable source spine trim");
            },
        )
        .await;
    let source_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut source_session).expect("source session should be unique"),
    )
    .await;
    source_session
        .on_init()
        .await
        .expect("initialize source trim-only spine");
    let source_output = function_output_with_text(
        "source-long-tool",
        &trim_candidate_text("source head fork output "),
    );
    source_session
        .record_conversation_items(&source_context, std::slice::from_ref(&source_output))
        .await
        .expect("record source output");
    source_session
        .apply_spine_trim_projection_if_available()
        .await
        .expect("tag source output");

    let boundary = SpineStore::clone_boundary_for_rollout(&source_rollout_path, 1)
        .expect("capture source head boundary")
        .expect("source sidecar exists");
    let (mut child_session, child_context, _child_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineTrim)
                    .expect("enable child spine trim");
            },
        )
        .await;
    let child_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut child_session).expect("child session should be unique"),
    )
    .await;
    child_session
        .clone_spine_sidecar_for_fork(&boundary, &[Some(source_output.clone())])
        .await
        .expect("clone trim-only head sidecar");
    child_session
        .replace_history(
            vec![source_output.clone()],
            child_session.reference_context_item().await,
        )
        .await;
    child_session
        .persist_rollout_items(&[RolloutItem::ResponseItem(source_output.clone())])
        .await;
    child_session.ensure_rollout_materialized().await;

    let child_store = SpineStore::for_rollout(&child_rollout_path).expect("child store");
    assert!(
        !child_store.tree_path_for_test().exists(),
        "trim-only head fork must not create the JIT parser tree ledger"
    );
    assert!(
        child_session
            .spine
            .as_ref()
            .expect("child spine state")
            .lock()
            .await
            .runtime()
            .is_some(),
        "head-boundary fork must install the cloned trim runtime"
    );

    let child_output = function_output_with_text(
        "child-long-tool",
        &trim_candidate_text("child after head fork output "),
    );
    child_session
        .record_conversation_items(&child_context, std::slice::from_ref(&child_output))
        .await
        .expect("record child output after head fork");
    child_session
        .apply_spine_trim_projection_if_available()
        .await
        .expect("tag child output after head fork");
    let visible = child_session.clone_history().await.raw_items().to_vec();
    assert!(
        function_output_text_by_call_id(&visible, "source-long-tool")
            .starts_with("[TRIM_ID: trim_1]\n"),
        "forked source trim id should remain visible"
    );
    assert!(
        function_output_text_by_call_id(&visible, "child-long-tool")
            .starts_with("[TRIM_ID: trim_3]\n"),
        "child output after head fork should continue the trim ledger without JIT"
    );
    assert!(
        !child_store.tree_path_for_test().exists(),
        "continuing trim-only head fork must still not create the JIT parser tree ledger"
    );
}

#[tokio::test]
async fn custom_and_tool_search_outputs_commit_as_completed_toolcalls() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let custom_request = custom_tool_call("custom-tool");
    let custom_output = custom_tool_output("custom-tool");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&custom_request))
        .await
        .expect("record custom request");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        custom_output.clone(),
    )
    .await
    .expect("commit custom output");

    let search_request = tool_search_call("search-tool");
    let search_output = tool_search_output("search-tool");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&search_request))
        .await
        .expect("record tool-search request");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        search_output.clone(),
    )
    .await
    .expect("commit tool-search output");

    let expected = vec![custom_request, custom_output, search_request, search_output];
    assert_eq!(
        session.clone_history().await.raw_items(),
        expected.as_slice()
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load replayed runtime")
        .expect("spine sidecar exists");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize replayed custom/tool-search toolcalls"),
        expected
    );
}

#[tokio::test]
async fn provider_tool_search_output_recorded_by_base_path_commits_completed_toolcall() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let search_request = tool_search_call_with_execution("server-search-tool", "server");
    let search_output = tool_search_output_with_execution("server-search-tool", "server");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&search_request))
        .await
        .expect("record provider tool-search request through base path");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&search_output))
        .await
        .expect("record provider tool-search output through base path");

    let expected = vec![search_request, search_output];
    assert_eq!(
        session.clone_history().await.raw_items(),
        expected.as_slice()
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load replayed runtime")
        .expect("spine sidecar exists");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize replayed provider tool-search toolcall"),
        expected
    );
}

#[tokio::test]
async fn grouped_toolcall_prevalidates_request_anchors_before_recording_outputs() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let anchored_request = function_call("anchored_tool", "anchored-call");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&anchored_request))
        .await
        .expect("record anchored request");
    let before_history = session.clone_history().await.raw_items().to_vec();
    let err = session
        .test_on_toolcall(
            &turn_context,
            ToolCallEvidence::grouped(
                "anchored-call",
                &["anchored-call".to_string(), "missing-request".to_string()],
                &[
                    function_output("anchored-call"),
                    function_output("missing-request"),
                ],
            ),
        )
        .await
        .expect_err("missing request anchor must fail before recording outputs");
    assert!(
        err.to_string()
            .contains("missing tool request anchor for call_id=missing-request"),
        "unexpected grouped prevalidation error: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        before_history.as_slice(),
        "failed grouped toolcall prevalidation must not append outputs to host history"
    );
}

#[tokio::test]
async fn grouped_toolcall_rejects_unexpected_output_before_recording_outputs() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let anchored_request = function_call("anchored_tool", "anchored-call");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&anchored_request))
        .await
        .expect("record anchored request");
    let before_history = session.clone_history().await.raw_items().to_vec();
    let err = session
        .test_on_toolcall(
            &turn_context,
            ToolCallEvidence::grouped(
                "anchored-call",
                &["anchored-call".to_string()],
                &[
                    function_output("anchored-call"),
                    function_output("extra-call"),
                ],
            ),
        )
        .await
        .expect_err("unexpected grouped output must fail before recording outputs");
    assert!(
        err.to_string()
            .contains("grouped Spine toolcall unexpected output for call_id=extra-call"),
        "unexpected grouped prevalidation error: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        before_history.as_slice(),
        "failed grouped toolcall prevalidation must not append outputs to host history"
    );
}

#[tokio::test]
async fn grouped_spine_open_after_close_uses_current_mutable_output_context_slots() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("outer user prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "open-inner");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record inner open request");
    session
        .test_seed_spine_open_control_request("open-inner".to_string(), "inner scope".to_string())
        .await
        .expect("stage inner open");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("open-inner"),
    )
    .await
    .expect("commit inner open");

    let inner_body = assistant_message("inner body");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner_body))
        .await
        .expect("record inner body");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "close-inner");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record inner close request");
    session
        .test_seed_spine_close_control_request(
            "close-inner".to_string(),
            "inner memory".to_string(),
        )
        .await
        .expect("stage inner close");
    commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("close-inner"),
    )
    .await
    .expect("commit inner close");

    let post_close_user = user_message("post close user");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&post_close_user))
        .await
        .expect("record post-close user");
    let sibling_open = spine_call(SPINE_TOOL_OPEN, "open-sibling");
    let ordinary_request = function_call("exec_command", "post-close-ordinary");
    session
        .record_conversation_items(
            &turn_context,
            &[sibling_open.clone(), ordinary_request.clone()],
        )
        .await
        .expect("record grouped sibling open and ordinary request");
    session
        .test_seed_spine_open_control_request(
            "open-sibling".to_string(),
            "sibling scope".to_string(),
        )
        .await
        .expect("stage sibling open");

    session
        .test_on_toolcall(
            &turn_context,
            ToolCallEvidence::grouped(
                "open-sibling",
                &[
                    "open-sibling".to_string(),
                    "post-close-ordinary".to_string(),
                ],
                &[
                    function_output("open-sibling"),
                    function_output("post-close-ordinary"),
                ],
            ),
        )
        .await
        .expect("grouped sibling open commit must use current mutable context slots");

    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
    let visible_history = session.clone_history().await.raw_items().to_vec();
    assert!(
        visible_history.iter().any(|item| matches!(
            item,
            ResponseItem::Message { role, content, .. }
                if role == "user"
                    && matches!(
                        content.as_slice(),
                        [ContentItem::InputText { text }]
                            if text.starts_with("[U")
                                && text.contains("post close user")
                    )
        )),
        "post-close user message must remain a user anchor, not be projected onto a tool output"
    );
    let visible_refs =
        assert_spine_visible_response_context_refs_strictly_increase(&session, &rollout_path).await;
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let open_output_raw = raw_items
        .iter()
        .enumerate()
        .find_map(|(raw, item)| {
            matches!(
                item,
                Some(ResponseItem::FunctionCallOutput { call_id, .. })
                    if call_id == "open-sibling"
            )
            .then_some(u64::try_from(raw).expect("raw ordinal fits"))
        })
        .expect("open-sibling output raw ordinal");
    let ordinary_output_raw = raw_items
        .iter()
        .enumerate()
        .find_map(|(raw, item)| {
            matches!(
                item,
                Some(ResponseItem::FunctionCallOutput { call_id, .. })
                    if call_id == "post-close-ordinary"
            )
            .then_some(u64::try_from(raw).expect("raw ordinal fits"))
        })
        .expect("ordinary output raw ordinal");
    let open_output_ctx = visible_refs
        .iter()
        .find_map(|(raw, ctx)| (*raw == open_output_raw).then_some(*ctx))
        .expect("open output visible context index");
    let ordinary_output_ctx = visible_refs
        .iter()
        .find_map(|(raw, ctx)| (*raw == ordinary_output_raw).then_some(*ctx))
        .expect("ordinary output visible context index");
    assert_eq!(
        ordinary_output_ctx,
        open_output_ctx
            .checked_add(1)
            .expect("ordinary output context fits"),
        "grouped response outputs must occupy adjacent mutable context slots"
    );
    assert!(
        ordinary_output_ctx < usize::try_from(ordinary_output_raw).expect("raw fits"),
        "post-close grouped output context must be current mutable context, not raw-prefix space"
    );
    drop(rx);
}

#[tokio::test]
async fn grouped_spine_open_output_and_followup_message_have_distinct_context_slots() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("prefix before grouped open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "grouped-open-distinct");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record grouped open request");
    session
        .test_seed_spine_open_control_request(
            "grouped-open-distinct".to_string(),
            "grouped open distinct".to_string(),
        )
        .await
        .expect("stage grouped open");

    let open_output = function_output("grouped-open-distinct");
    session
        .test_on_toolcall(
            &turn_context,
            ToolCallEvidence::grouped(
                "grouped-open-distinct",
                &["grouped-open-distinct".to_string()],
                std::slice::from_ref(&open_output),
            ),
        )
        .await
        .expect("grouped open should commit");

    let followup = assistant_message("message after grouped open output");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&followup))
        .await
        .expect("record followup message");

    let expected = vec![
        anchored_user_message(1, "prefix before grouped open"),
        open_request,
        open_output,
        followup,
    ];
    assert_eq!(
        session.clone_history().await.raw_items(),
        expected.as_slice(),
        "grouped spine.open output must remain in host history before the followup message"
    );
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(
        variable_spine_items(session.clone_history().await.raw_items()),
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        "host variable context must equal h(PS)"
    );
    let visible_refs =
        assert_spine_visible_response_context_refs_strictly_increase(&session, &rollout_path).await;
    let open_output_ctx = visible_refs
        .iter()
        .find_map(|(raw, ctx)| (*raw == 2).then_some(*ctx))
        .expect("grouped open output raw ref should be visible");
    let followup_ctx = visible_refs
        .iter()
        .find_map(|(raw, ctx)| (*raw == 3).then_some(*ctx))
        .expect("followup message raw ref should be visible");
    assert_ne!(
        open_output_ctx, followup_ctx,
        "grouped open output and next assistant message must not share a visible PS context_index"
    );
    drop(rx);
}

#[tokio::test]
async fn grouped_spine_open_with_fixed_prefix_uses_mutable_context_indices() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.user_instructions =
                Some("fixed AGENTS/user prefix before mutable grouped open".to_string());
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session
        .record_initial_history(InitialHistory::New)
        .await
        .expect("initialize spine");
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record fixed context baseline");

    let fixed_prefix = developer_message("fixed developer prefix outside mutable grouped open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&fixed_prefix))
        .await
        .expect("record fixed developer prefix");

    let prefix = user_message("mutable prefix before grouped open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record mutable prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "grouped-open-fixed-prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record grouped open request");
    session
        .test_seed_spine_open_control_request(
            "grouped-open-fixed-prefix".to_string(),
            "grouped open with fixed prefix".to_string(),
        )
        .await
        .expect("stage grouped open");

    let open_output = function_output("grouped-open-fixed-prefix");
    session
        .test_on_toolcall(
            &turn_context,
            ToolCallEvidence::grouped(
                "grouped-open-fixed-prefix",
                &["grouped-open-fixed-prefix".to_string()],
                std::slice::from_ref(&open_output),
            ),
        )
        .await
        .expect("grouped open should commit");

    let followup = assistant_message("message after grouped open with fixed prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&followup))
        .await
        .expect("record followup message");

    let host_history = session.clone_history().await.raw_items().to_vec();
    assert_eq!(
        variable_spine_items(&host_history),
        vec![
            anchored_user_message(1, "mutable prefix before grouped open"),
            open_request,
            open_output,
            followup,
        ],
        "fixed prefix must stay outside h(PS), while mutable context equals h(PS)"
    );
    assert!(
        fixed_spine_context_item_count(&host_history) > 0,
        "test setup must include a fixed prefix outside h(PS)"
    );
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(
        variable_spine_items(session.clone_history().await.raw_items()),
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        "host variable context must equal h(PS)"
    );
    let visible_refs =
        assert_spine_visible_response_context_refs_strictly_increase(&session, &rollout_path).await;
    assert_eq!(
        visible_refs
            .iter()
            .map(|(_, context_index)| *context_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "PS-visible context_index must be mutable Ctx coordinates, not full host history indices"
    );
    drop(rx);
}

#[tokio::test]
async fn base_path_completed_toolcall_groups_all_outputs_in_one_append() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let request = function_call("batched_tool", "batched-tool");
    let first_output = function_output("batched-tool");
    let mut second_output = function_output("batched-tool");
    if let ResponseItem::FunctionCallOutput { output, .. } = &mut second_output {
        output.body = FunctionCallOutputBody::Text("second ok".to_string());
    }
    let expected = vec![request, first_output, second_output];
    session
        .record_conversation_items(&turn_context, &expected)
        .await
        .expect("record request and both outputs through base path");

    assert_eq!(
        session.clone_history().await.raw_items(),
        expected.as_slice()
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load replayed runtime")
        .expect("spine sidecar exists");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize replayed batched toolcall"),
        expected
    );
}

#[tokio::test]
async fn base_path_completed_toolcall_groups_multiple_requests_in_one_leaf() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let request_z = function_call("z_tool", "z-call");
    let request_a = function_call("a_tool", "a-call");
    let mut output_z = function_output("z-call");
    let mut output_a = function_output("a-call");
    replace_function_output_text_for_test(&mut output_z, "z ok".to_string());
    replace_function_output_text_for_test(&mut output_a, "a ok".to_string());
    let expected = vec![
        request_z.clone(),
        request_a.clone(),
        output_z.clone(),
        output_a.clone(),
    ];

    session
        .record_conversation_items(&turn_context, &expected)
        .await
        .expect("record grouped request and outputs through base path");

    assert_eq!(
        session.clone_history().await.raw_items(),
        expected.as_slice()
    );
    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load replayed runtime")
        .expect("spine sidecar exists");
    assert_eq!(runtime.parse_stack_toolcall_leaf_count_for_test(), 1);
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize replayed grouped toolcall"),
        expected
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spine_status_overlay_is_injected_into_sampling_prompt_only() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::SpineJit)
            .expect("enable spine feature");
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("spine-status-response", "ok"),
            ev_completed("spine-status-response"),
        ]),
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "check status overlay".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;
    wait_for_event_match(&test.codex, |event| match event {
        EventMsg::TurnComplete(_) => Some(()),
        _ => None,
    })
    .await;

    let request = responses.single_request();
    let developer_texts = request.message_input_texts("developer");
    let status = developer_texts
        .iter()
        .find(|text| text.contains("<spine_status"))
        .expect("status overlay should be present");
    assert!(status.contains(r#"cursor="1.1""#), "{status}");
    assert!(status.contains(r#"parent="1""#), "{status}");
    assert!(
        status.contains(r#"cursor_context="unavailable""#),
        "{status}"
    );
    assert!(!status.contains("cursor_context_problem"), "{status}");
    assert!(!status.contains(r#"live_node=""#), "{status}");
    assert!(
        status.contains(r#"context_left=""#) || status.contains(r#"context_left="unavailable""#),
        "{status}"
    );
    assert!(!status.contains(r#"window=""#), "{status}");

    Ok(())
}

#[tokio::test]
async fn spine_close_control_toolcalls_are_durable_context_history() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse(
            "spine-close-overlay-summary",
            "overlay secondary close memory",
        ),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let prefix = user_message("overlay close prefix");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&prefix))
        .await
        .expect("record prefix");

    let open_request = spine_call(SPINE_TOOL_OPEN, "overlay-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "overlay-open".to_string(),
            "overlay child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("overlay-open");
    commit_spine_output_and_record_raw_durable_for_test(&session, &turn_context, open_output)
        .await
        .expect("commit and record open output");

    let inner = assistant_message("overlay close inner work");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner))
        .await
        .expect("record inner");

    let close_request = spine_call(SPINE_TOOL_CLOSE, "overlay-close");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "overlay-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    let close_output = commit_spine_output_and_record_raw_durable_for_test(
        &session,
        &turn_context,
        function_output("overlay-close"),
    )
    .await
    .expect("commit close and record raw evidence");
    assert!(matches!(
        &close_output,
        ResponseItem::FunctionCallOutput { call_id, output }
            if call_id == "overlay-close"
                && output.text_content() == Some("ok")
    ));

    assert_eq!(
        compact_mock.requests().len(),
        0,
        "overlay close must use direct memory without a secondary compact request"
    );

    let history = session.clone_history().await;
    let items = history.raw_items();
    assert_eq!(items.len(), 4);
    assert_eq!(items[0], anchored_user_message(1, "overlay close prefix"));
    assert!(matches!(
        &items[1],
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && matches!(
                    content.as_slice(),
                    [ContentItem::InputText { text }]
                        if text.contains("Spine Memory 1.1.1")
                            && text.contains("## Node Memory\ntest node memory")
                            && !text.contains("overlay close inner work")
                            && !text.contains("overlay secondary close memory")
                )
    ));
    assert_eq!(items[2], spine_call(SPINE_TOOL_CLOSE, "overlay-close"));
    assert!(matches!(
        &items[3],
        ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "overlay-close"
    ));

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    assert!(resumed.history.iter().any(|item| {
        matches!(item, RolloutItem::ResponseItem(ResponseItem::FunctionCall { call_id, .. }) if call_id == "overlay-open")
    }));
    assert!(resumed.history.iter().any(|item| {
        matches!(item, RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, .. }) if call_id == "overlay-close")
    }));
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    let tree = runtime.render_tree().expect("render tree");
    assert!(tree.contains("Done overlay child"), "{tree}");
    assert!(tree.contains("Memory.md"), "{tree}");
    let (_, raw_end, context_start, context_end) = SpineStore::for_rollout(&rollout_path)
        .expect("load spine store")
        .suffix_mem_cover_for_test("1.1.1")
        .expect("read spine mem records")
        .expect("closed child suffix memory should be recorded");
    assert_eq!(context_start, 1);
    assert_eq!(context_end, 4);
    assert_eq!(raw_end, 4);
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        items
    );
}

#[tokio::test]
async fn spine_tree_tool_appends_context_pressure_from_last_usage() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;
    session.on_init().await.expect("initialize spine");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 102_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 61_200,
                ..TokenUsage::default()
            },
            model_context_window: Some(258_000),
        }));
    }

    let tree = session.spine_tree().await.expect("tree");
    assert!(tree.contains("Context window:"), "{tree}");
    assert!(tree.contains("80% left"), "{tree}");
    assert!(tree.contains("61.2K used / 258K"), "{tree}");
    assert!(
        !tree.contains("102K"),
        "context pressure must not use cumulative total token usage: {tree}"
    );
}

#[tokio::test]
async fn spine_tree_tool_omits_context_pressure_when_window_unknown() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;
    session.on_init().await.expect("initialize spine");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                total_tokens: 102_000,
                ..TokenUsage::default()
            },
            last_token_usage: TokenUsage {
                total_tokens: 61_200,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }

    let tree = session.spine_tree().await.expect("tree");
    assert!(!tree.contains("Context window:"), "{tree}");
}

#[tokio::test]
async fn spine_tree_tool_node_context_uses_provider_context_delta() {
    assert_spine_tree_tool_node_context_uses_provider_context_delta().await;
}

#[tokio::test]
async fn resume_restores_context_pressure_metadata_for_tree_display() {
    assert_spine_tree_tool_node_context_uses_provider_context_delta().await;
}

async fn assert_spine_tree_tool_node_context_uses_provider_context_delta() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let open_request = spine_call(SPINE_TOOL_OPEN, "delta-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request("delta-open".to_string(), "delta child".to_string())
        .await
        .expect("stage open");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 37_002,
                total_tokens: 37_002,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }
    let open_output = function_output("delta-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");
    while rx.try_recv().is_ok() {}

    session
        .replace_history(Vec::new(), session.reference_context_item().await)
        .await;
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 218_548,
                total_tokens: 218_548,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }

    let tree = session.spine_tree().await.expect("tree");
    assert!(tree.contains("[1.1.1] Current delta child"), "{tree}");
    assert!(tree.contains("(~182K inclusive context)"), "{tree}");
    session
        .emit_spine_tree_snapshot(&turn_context)
        .await
        .expect("emit Spine tree snapshot");
    let event = timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("timeout waiting for Spine tree snapshot")
        .expect("event");
    let snapshot = match event.msg {
        EventMsg::SpineTreeUpdate(snapshot) => snapshot,
        msg => panic!("expected Spine tree update, got {msg:?}"),
    };
    let active = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id)
        .expect("active node");
    let accounting = active.accounting.as_ref().expect("active accounting");
    assert_eq!(accounting.current_node_context_tokens, Some(181_546));
    assert_eq!(
        accounting.current_node_context_baseline_source,
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
    assert_eq!(accounting.current_node_context_problem, None);

    let rollout_path = session
        .current_rollout_path()
        .await
        .expect("rollout path")
        .expect("thread should have rollout path");
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let mut resumed_history = resumed.history;
    let raw_items = spine_raw_items_after_rollback(&resumed_history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(runtime.current_open_input_tokens(), Some(37_002));
    assert_eq!(runtime.current_open_provider_input_tokens(), Some(37_002));

    resumed_history.push(RolloutItem::EventMsg(EventMsg::TokenCount(
        TokenCountEvent {
            info: Some(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage {
                    input_tokens: 218_548,
                    total_tokens: 218_548,
                    ..TokenUsage::default()
                },
                model_context_window: None,
            }),
            rate_limits: None,
        },
    )));

    let (mut resumed_session, _resumed_context, resumed_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = raw_items.iter().map(Option::is_some).collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&rollout_path, &resumed_rollout_path, &raw_live);

    resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: resumed_history,
            rollout_path: Some(rollout_path),
        }))
        .await
        .expect("record resumed history");
    let event = timeout(StdDuration::from_secs(1), resumed_rx.recv())
        .await
        .expect("timeout waiting for resumed Spine tree snapshot")
        .expect("event");
    let snapshot = match event.msg {
        EventMsg::SpineTreeUpdate(snapshot) => snapshot,
        msg => panic!("expected Spine tree update, got {msg:?}"),
    };
    let active = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id)
        .expect("active node");
    let accounting = active.accounting.as_ref().expect("active accounting");
    assert_eq!(accounting.current_node_context_tokens, Some(181_546));
    assert_eq!(
        accounting.current_node_context_baseline_source,
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
    assert_eq!(accounting.current_node_context_problem, None);
}

#[tokio::test]
async fn spine_next_sibling_tree_defers_provider_open_baseline_until_post_replacement_usage() {
    let server = start_mock_server().await;
    let compact_mock = mount_sse_once(
        &server,
        spine_node_memory_summary_sse("next-baseline-summary", "next baseline memory"),
    )
    .await;
    let base_url = format!("{}/v1", server.uri());
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.model_provider.base_url = Some(base_url.clone());
            config.model_provider.supports_websockets = false;
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let open_request = spine_call(SPINE_TOOL_OPEN, "next-baseline-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "next-baseline-open".to_string(),
            "next baseline child".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("next-baseline-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    let child_body = assistant_message("next baseline child body");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_body))
        .await
        .expect("record child body");
    let next_request = spine_call(SPINE_TOOL_NEXT, "next-baseline");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&next_request))
        .await
        .expect("record next request");
    session
        .test_seed_spine_next_control_request(
            "next-baseline".to_string(),
            "next baseline sibling".to_string(),
            "next baseline memory".to_string(),
        )
        .await
        .expect("stage next");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 40_000,
                total_tokens: 40_000,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }
    let next_output = function_output("next-baseline");
    test_on_toolcall_single(&session, &turn_context, &next_output)
        .await
        .expect("commit next");
    assert_eq!(
        compact_mock.requests().len(),
        0,
        "next baseline commit must use direct memory without a secondary compact request"
    );
    let tree = session
        .spine_tree()
        .await
        .expect("tree before post baseline");
    assert!(
        !tree.contains("[1.1.2] Current next baseline sibling (~"),
        "{tree}"
    );

    let post_replacement_usage = TokenUsage {
        input_tokens: 55_500,
        total_tokens: 55_500,
        ..TokenUsage::default()
    };
    session
        .record_token_usage_info(&turn_context, Some(&post_replacement_usage))
        .await;
    while rx.try_recv().is_ok() {}

    let later_same_epoch_usage = TokenUsage {
        input_tokens: 72_000,
        total_tokens: 72_000,
        ..TokenUsage::default()
    };
    session
        .record_token_usage_info(&turn_context, Some(&later_same_epoch_usage))
        .await;
    while rx.try_recv().is_ok() {}
    let tree = session.spine_tree().await.expect("tree");
    assert!(
        tree.contains("[1.1.2] Current next baseline sibling (~16.5K inclusive context)"),
        "{tree}"
    );

    session
        .emit_spine_tree_snapshot(&turn_context)
        .await
        .expect("emit Spine tree snapshot");
    let event = timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("timeout waiting for Spine tree snapshot")
        .expect("event");
    let snapshot = match event.msg {
        EventMsg::SpineTreeUpdate(snapshot) => snapshot,
        msg => panic!("expected Spine tree update, got {msg:?}"),
    };
    let active = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id)
        .expect("active node");
    assert_eq!(active.node_id, "1.1.2");
    let accounting = active.accounting.as_ref().expect("active accounting");
    assert_eq!(accounting.current_node_context_tokens, Some(16_500));
    assert_eq!(
        accounting.current_node_context_baseline_source,
        Some(SpineNodeContextBaselineSource::ProviderAtOpen)
    );
    assert_eq!(accounting.current_node_context_problem, None);
}

#[tokio::test]
async fn spine_tree_tool_appends_inclusive_context_for_open_ancestors() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let outer_request = spine_call(SPINE_TOOL_OPEN, "outer-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&outer_request))
        .await
        .expect("record outer open request");
    session
        .test_seed_spine_open_control_request("outer-open".to_string(), "outer scope".to_string())
        .await
        .expect("stage outer open");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 10_000,
                total_tokens: 10_000,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }
    let outer_output = function_output("outer-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&outer_output))
        .await
        .expect("record outer open output");
    test_on_toolcall_single(&session, &turn_context, &outer_output)
        .await
        .expect("commit outer open");

    let inner_request = spine_call(SPINE_TOOL_OPEN, "inner-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner_request))
        .await
        .expect("record inner open request");
    session
        .test_seed_spine_open_control_request("inner-open".to_string(), "inner scope".to_string())
        .await
        .expect("stage inner open");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 30_000,
                total_tokens: 30_000,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }
    let inner_output = function_output("inner-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&inner_output))
        .await
        .expect("record inner open output");
    test_on_toolcall_single(&session, &turn_context, &inner_output)
        .await
        .expect("commit inner open");
    while rx.try_recv().is_ok() {}

    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 80_000,
                total_tokens: 80_000,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }

    let tree = session.spine_tree().await.expect("tree");
    assert!(
        tree.contains("[1.1.1] Open outer scope (~70.0K inclusive context)"),
        "{tree}"
    );
    assert!(
        tree.contains("[1.1.1.1] Current inner scope (~50.0K inclusive context)"),
        "{tree}"
    );

    session
        .emit_spine_tree_snapshot(&turn_context)
        .await
        .expect("emit Spine tree snapshot");
    let event = timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("timeout waiting for Spine tree snapshot")
        .expect("event");
    let snapshot = match event.msg {
        EventMsg::SpineTreeUpdate(snapshot) => snapshot,
        msg => panic!("expected Spine tree update, got {msg:?}"),
    };
    let outer = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1.1")
        .expect("outer node");
    let inner = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == "1.1.1.1")
        .expect("inner node");
    assert_eq!(
        outer
            .accounting
            .as_ref()
            .and_then(|accounting| accounting.current_node_context_tokens),
        Some(70_000)
    );
    assert_eq!(
        inner
            .accounting
            .as_ref()
            .and_then(|accounting| accounting.current_node_context_tokens),
        Some(50_000)
    );
}

#[tokio::test]
async fn record_token_usage_refreshes_spine_tree_cache_only_snapshot() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let open_request = spine_call(SPINE_TOOL_OPEN, "cache-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request("cache-open".to_string(), "cache scope".to_string())
        .await
        .expect("stage open");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 25_000,
                total_tokens: 25_000,
                ..TokenUsage::default()
            },
            model_context_window: None,
        }));
    }
    let open_output = function_output("cache-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");
    while rx.try_recv().is_ok() {}

    let usage = TokenUsage {
        input_tokens: 90_000,
        total_tokens: 90_000,
        ..TokenUsage::default()
    };
    session
        .record_token_usage_info(&turn_context, Some(&usage))
        .await;

    let event = timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("timeout waiting for cache-only Spine tree snapshot")
        .expect("event");
    assert_eq!(event.id, INITIAL_SUBMIT_ID);
    let snapshot = match event.msg {
        EventMsg::SpineTreeUpdate(snapshot) => snapshot,
        msg => panic!("expected Spine tree update, got {msg:?}"),
    };
    let active = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id)
        .expect("active node");
    assert_eq!(
        active
            .accounting
            .as_ref()
            .and_then(|accounting| accounting.current_node_context_tokens),
        Some(65_000)
    );
}

#[tokio::test]
async fn spine_tree_tool_hides_context_problem_but_snapshot_keeps_it() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let open_request = spine_call(SPINE_TOOL_OPEN, "open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record conversation items");
    session
        .test_seed_spine_open_control_request("open".to_string(), "invalid range".to_string())
        .await
        .expect("stage open");
    let open_output = function_output("open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record conversation items");
    let commit = test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open should defer missing token usage");
    assert_eq!(
        commit.recording(),
        SpineToolOutputRecording::WithoutSpineObserve
    );

    while rx.try_recv().is_ok() {}
    let history_before = session.clone_history().await.raw_items().to_vec();
    let tree = session.spine_tree().await.expect("tree");
    assert!(!tree.contains("context problem"), "{tree}");
    assert!(tree.contains("[1.1.1] Current invalid range"), "{tree}");

    session
        .record_token_usage_info(
            &turn_context,
            Some(&TokenUsage {
                input_tokens: 90_000,
                total_tokens: 90_000,
                ..TokenUsage::default()
            }),
        )
        .await;

    let event = timeout(StdDuration::from_secs(1), rx.recv())
        .await
        .expect("timeout waiting for Spine tree snapshot")
        .expect("event");
    let snapshot = match event.msg {
        EventMsg::SpineTreeUpdate(snapshot) => snapshot,
        msg => panic!("expected Spine tree update, got {msg:?}"),
    };
    let active = snapshot
        .nodes
        .iter()
        .find(|node| node.node_id == snapshot.active_node_id)
        .expect("active node");
    let accounting = active.accounting.as_ref().expect("active accounting");
    assert_eq!(accounting.current_node_context_baseline_source, None);
    assert!(accounting.current_node_context_tokens.is_none());
    assert_eq!(
        accounting.current_node_context_problem,
        Some(SpineNodeContextProblem::MissingOpenContextBaseline)
    );
    assert_eq!(session.clone_history().await.raw_items(), history_before);
}

#[tokio::test]
async fn spine_pressure_prompt_overlay_is_temporarily_disabled() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let open_request = spine_call(SPINE_TOOL_OPEN, "pressure-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "pressure-open".to_string(),
            "pressure scope".to_string(),
        )
        .await
        .expect("stage open");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 10_000,
                total_tokens: 10_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(200_000),
        }));
    }
    let open_output = function_output("pressure-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open");

    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 60_000,
                total_tokens: 60_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(200_000),
        }));
    }
    assert!(
        session
            .spine_pressure_prompt_overlay(ModeKind::Default)
            .await
            .is_none(),
        "pressure overlay is temporarily disabled"
    );
    let history_before = session.clone_history().await.raw_items().to_vec();

    assert!(
        session
            .spine_pressure_prompt_overlay(ModeKind::Default)
            .await
            .is_none(),
        "same node and same 50k band must not duplicate"
    );

    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 85_000,
                total_tokens: 85_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(200_000),
        }));
    }
    assert!(
        session
            .spine_pressure_prompt_overlay(ModeKind::Default)
            .await
            .is_none(),
        "pressure overlay remains disabled across pressure bands"
    );
    assert_eq!(session.clone_history().await.raw_items(), history_before);
}

#[tokio::test]
async fn spine_status_prompt_reports_cursor_parent_summary_and_pressure_without_persisting() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let parent_open_request = spine_call(SPINE_TOOL_OPEN, "status-parent-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&parent_open_request))
        .await
        .expect("record parent open request");
    session
        .test_seed_spine_open_control_request(
            "status-parent-open".to_string(),
            "parent \"scope\" <drift> & focus".to_string(),
        )
        .await
        .expect("stage parent open");
    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 10_000,
                total_tokens: 10_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(200_000),
        }));
    }
    let parent_open_output = function_output("status-parent-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&parent_open_output))
        .await
        .expect("record parent open output");
    test_on_toolcall_single(&session, &turn_context, &parent_open_output)
        .await
        .expect("commit parent open");

    let child_open_request = spine_call(SPINE_TOOL_OPEN, "status-child-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_open_request))
        .await
        .expect("record child open request");
    session
        .test_seed_spine_open_control_request(
            "status-child-open".to_string(),
            "child \"scope\" <leaf> & focus".to_string(),
        )
        .await
        .expect("stage child open");
    let child_open_output = function_output("status-child-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&child_open_output))
        .await
        .expect("record child open output");
    test_on_toolcall_single(&session, &turn_context, &child_open_output)
        .await
        .expect("commit child open");

    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 42_000,
                total_tokens: 42_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(200_000),
        }));
    }
    let pending_item = user_message("pending context left delta");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&pending_item))
        .await
        .expect("record pending user");
    let history_before = session.clone_history().await.raw_items().to_vec();
    let current_usage = session.get_total_token_usage().await;
    let auto_compact_limit = turn_context
        .model_info
        .auto_compact_token_limit()
        .expect("auto compact limit");
    let expected_left = auto_compact_limit.saturating_sub(current_usage);
    let overlay = session
        .spine_status_prompt_overlay(&turn_context)
        .await
        .expect("status overlay");
    let text = pressure_overlay_text(&overlay.item);
    assert!(text.starts_with("<spine_status "), "{text}");
    assert!(text.contains(r#"cursor="1.1.1.1""#), "{text}");
    assert!(
        text.contains(r#"summary="child &quot;scope&quot; &lt;leaf&gt; &amp; focus""#),
        "{text}"
    );
    assert!(text.contains(r#"parent="1.1.1""#), "{text}");
    assert!(
        text.contains(r#"parent_summary="parent &quot;scope&quot; &lt;drift&gt; &amp; focus""#),
        "{text}"
    );
    assert!(text.contains(r#"cursor_context="32.0K""#), "{text}");
    assert!(!text.contains(r#"live_node=""#), "{text}");
    assert!(
        text.contains(&format!(
            r#"context_left="{}""#,
            format_si_suffix(expected_left)
        )),
        "{text}"
    );
    assert!(!text.contains(r#"window=""#), "{text}");
    assert_eq!(session.clone_history().await.raw_items(), history_before);
}

#[tokio::test]
async fn spine_pressure_prompt_context_warning_is_temporarily_disabled() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique")).await;

    let open_request = spine_call(SPINE_TOOL_OPEN, "warning-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_request))
        .await
        .expect("record open request");
    session
        .test_seed_spine_open_control_request(
            "warning-open".to_string(),
            "warning scope".to_string(),
        )
        .await
        .expect("stage open");
    let open_output = function_output("warning-open");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&open_output))
        .await
        .expect("record open output");
    test_on_toolcall_single(&session, &turn_context, &open_output)
        .await
        .expect("commit open without token baseline");

    {
        let mut state = session.state.lock().await;
        state.set_token_info(Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage {
                input_tokens: 80_000,
                total_tokens: 80_000,
                ..TokenUsage::default()
            },
            model_context_window: Some(100_000),
        }));
    }
    assert!(
        session
            .spine_pressure_prompt_overlay(ModeKind::Default)
            .await
            .is_none(),
        "context warning overlay is temporarily disabled"
    );
}

fn pressure_overlay_text(item: &ResponseItem) -> &str {
    let ResponseItem::Message { role, content, .. } = item else {
        panic!("expected pressure overlay message");
    };
    assert_eq!(role, "developer");
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("expected one text content item");
    };
    text
}

#[tokio::test]
async fn resume_rejects_committed_raw_without_sidecar_token() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session.on_init().await.expect("initialize spine");

    let store = SpineStore::for_rollout(&rollout_path).expect("spine store");
    let broken_tree_path = store.tree_path_for_test();
    let mut tree_permissions = std::fs::metadata(&broken_tree_path)
        .expect("tree metadata")
        .permissions();
    tree_permissions.set_readonly(true);
    std::fs::set_permissions(&broken_tree_path, tree_permissions)
        .expect("make tree ledger readonly");

    let err = session
        .record_conversation_items(
            &turn_context,
            std::slice::from_ref(&user_message("message that cannot reach sidecar")),
        )
        .await
        .expect_err("sidecar append failure should be fatal");
    assert!(
        matches!(err, CodexErr::SpineAppendFailure { .. }),
        "unexpected append error: {err}"
    );

    let err = session
        .spine_tree()
        .await
        .expect_err("invalidated Spine runtime should fail closed");
    assert!(
        err.to_string().contains("spine runtime is invalid"),
        "unexpected error: {err}"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let err = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect_err("stale sidecar must not resume after raw coverage diverges");
    assert!(
        err.to_string().contains("missing token coverage"),
        "unexpected resume error: {err}"
    );
    let mut tree_permissions = std::fs::metadata(&broken_tree_path)
        .expect("tree metadata")
        .permissions();
    tree_permissions.set_readonly(false);
    std::fs::set_permissions(&broken_tree_path, tree_permissions)
        .expect("restore tree ledger permissions");
}

#[tokio::test]
async fn init_feature_off_has_no_spine_state() {
    let (mut base_session, base_turn_context, _base_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |_config| {},
        )
        .await;
    let base_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut base_session).expect("base session should be unique"),
    )
    .await;
    assert!(base_session.spine.is_none());

    let (mut feature_off_session, feature_off_turn_context, _feature_off_rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .disable(Feature::SpineJit)
                    .expect("disable spine feature");
            },
        )
        .await;
    let feature_off_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut feature_off_session).expect("feature-off session should be unique"),
    )
    .await;
    assert!(feature_off_session.spine.is_none());

    let items = vec![
        user_message("feature off byte prefix"),
        spine_call(SPINE_TOOL_OPEN, "byte-open"),
        function_output("byte-open"),
        user_message("<spine_memory>plain text</spine_memory>"),
        spine_call(SPINE_TOOL_CLOSE, "byte-close"),
        function_output("byte-close"),
        assistant_message("feature off assistant"),
    ];
    for item in &items {
        base_session
            .record_conversation_items(&base_turn_context, std::slice::from_ref(item))
            .await
            .expect("record conversation items");
        feature_off_session
            .record_conversation_items(&feature_off_turn_context, std::slice::from_ref(item))
            .await
            .expect("record conversation items");
    }

    let base_history = base_session.clone_history().await;
    let feature_off_history = feature_off_session.clone_history().await;
    assert_eq!(
        serde_json::to_vec(base_history.raw_items()).expect("base raw history json"),
        serde_json::to_vec(feature_off_history.raw_items()).expect("feature-off raw history json")
    );
    assert_eq!(
        serde_json::to_vec(
            &base_history
                .clone()
                .for_prompt(&base_turn_context.model_info.input_modalities)
        )
        .expect("base prompt history json"),
        serde_json::to_vec(
            &feature_off_history
                .clone()
                .for_prompt(&feature_off_turn_context.model_info.input_modalities)
        )
        .expect("feature-off prompt history json")
    );

    base_session.ensure_rollout_materialized().await;
    base_session
        .flush_rollout()
        .await
        .expect("base rollout should flush");
    feature_off_session.ensure_rollout_materialized().await;
    feature_off_session
        .flush_rollout()
        .await
        .expect("feature-off rollout should flush");

    let InitialHistory::Resumed(base_resumed) =
        RolloutRecorder::get_rollout_history(&base_rollout_path)
            .await
            .expect("read base rollout")
    else {
        panic!("expected base resumed rollout history");
    };
    let InitialHistory::Resumed(feature_off_resumed) =
        RolloutRecorder::get_rollout_history(&feature_off_rollout_path)
            .await
            .expect("read feature-off rollout")
    else {
        panic!("expected feature-off resumed rollout history");
    };
    let base_reconstructed = base_session
        .reconstruct_history_from_rollout(&base_turn_context, &base_resumed.history)
        .await;
    let feature_off_reconstructed = feature_off_session
        .reconstruct_history_from_rollout(&feature_off_turn_context, &feature_off_resumed.history)
        .await;
    assert_eq!(
        serde_json::to_vec(&base_reconstructed.history).expect("base reconstructed json"),
        serde_json::to_vec(&feature_off_reconstructed.history)
            .expect("feature-off reconstructed json")
    );
    assert_eq!(
        base_reconstructed.used_replacement_history,
        feature_off_reconstructed.used_replacement_history
    );
    assert_eq!(
        base_reconstructed.spine_rollback_cuts,
        feature_off_reconstructed.spine_rollback_cuts
    );
    assert!(!SpineStore::has_for_rollout(&base_rollout_path).expect("check base sidecar"));
    assert!(
        !SpineStore::has_for_rollout(&feature_off_rollout_path).expect("check feature-off sidecar")
    );
}

#[tokio::test]
async fn resume_feature_off_replacement_history_unchanged() {
    let (session, _turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .disable(Feature::SpineJit)
                .expect("disable spine feature");
        },
    )
    .await;
    assert!(session.spine.is_none());

    let replacement_history = vec![
        user_message("feature off replacement history"),
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: "<spine_memory>not parsed</spine_memory>".to_string(),
            }],
            phase: None,
        },
    ];
    let rollout_items = vec![RolloutItem::Compacted(CompactedItem {
        message: "host compact checkpoint".to_string(),
        replacement_history: Some(replacement_history.clone()),
    })];

    session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(PathBuf::from("/tmp/feature-off-spine-looking.jsonl")),
        }))
        .await
        .expect("record initial history");

    assert_eq!(
        session.clone_history().await.raw_items(),
        replacement_history
    );
    assert!(session.spine.is_none());
}

#[tokio::test]
async fn init_does_not_shift_fixed_query_as_msg() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
            config.developer_instructions = Some("fixed developer instruction".to_string());
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let initial_context = session.build_initial_context(&turn_context).await;
    assert!(
        developer_input_texts(&initial_context)
            .iter()
            .any(|text| text.contains("fixed developer instruction")),
        "fixed developer instruction should be rendered into prompt prefix"
    );
    assert!(
        session.clone_history().await.raw_items().is_empty(),
        "building fixed query prompt prefix must not append working history"
    );

    assert!(
        !SpineStore::has_for_rollout(&rollout_path).expect("check sidecar"),
        "fixed query must not create Spine sidecar or shift Msg tokens"
    );

    let user = user_message("actual task message");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&user))
        .await
        .expect("record conversation items");
    let msg_count = session
        .spine
        .as_ref()
        .expect("spine enabled")
        .lock()
        .await
        .runtime()
        .expect("runtime exists")
        .parse_stack_msg_leaf_count_for_test();
    assert_eq!(
        msg_count, 1,
        "only the actual working-history message should shift as Msg"
    );
}

#[tokio::test]
async fn spine_raw_ordinals_follow_persisted_rollout_items_not_input_width() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;

    let first = user_message("first persisted message");
    let second = user_message("second persisted message");
    session
        .record_conversation_items(
            &turn_context,
            &[
                first.clone(),
                ResponseItem::CompactionTrigger,
                second.clone(),
            ],
        )
        .await
        .expect("record conversation items");

    let runtime = session
        .spine
        .as_ref()
        .expect("spine enabled")
        .lock()
        .await
        .runtime()
        .expect("runtime exists")
        .parse_stack_debug_for_test();
    assert!(
        runtime.contains("raw_ordinal: 0")
            && runtime.contains("context_index: 0")
            && runtime.contains("raw_ordinal: 1")
            && runtime.contains("context_index: 1"),
        "expected persisted raw ordinals to skip CompactionTrigger without widening raw trace: {runtime}"
    );
    assert!(
        !runtime.contains("raw_ordinal: 2"),
        "non-persisted input item must not advance Spine raw ordinal: {runtime}"
    );

    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    let runtime = SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime")
        .expect("spine sidecar should exist");
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw_items)
            .expect("materialize h(PS)"),
        vec![
            anchored_user_message(1, "first persisted message"),
            anchored_user_message(2, "second persisted message"),
        ]
    );
}

#[tokio::test]
async fn spine_resume_rejects_replacement_history_mismatch() {
    let (_source_session, _source_turn_context, source_rollout_path, mut rollout_items) =
        make_spine_session_with_closed_child("sidecar resume summary").await;
    let stale_replacement = vec![user_message("stale replacement_history should not win")];
    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: "stale compact checkpoint".to_string(),
        replacement_history: Some(stale_replacement.clone()),
    }));

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = spine_raw_items_after_rollback(&rollout_items)
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&source_rollout_path, &resumed_rollout_path, &raw_live);

    let err = resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(source_rollout_path),
        }))
        .await
        .expect_err("replacement_history mismatch should fail closed");
    let err = err.to_string();
    assert!(
        err.contains("missing spine compact checkpoint at raw boundary")
            || err.contains("spine sidecar is missing token coverage"),
        "unexpected resume error: {err}"
    );
}

#[tokio::test]
async fn failed_spine_replay_does_not_mutate_live_session_history() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let live_rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session.on_init().await.expect("initialize spine");
    let live_message = user_message("live history must survive failed replay");
    session
        .record_conversation_items(&turn_context, std::slice::from_ref(&live_message))
        .await
        .expect("record live history");
    let original_history = session.clone_history().await.raw_items().to_vec();

    let (_source_session, _source_turn_context, source_rollout_path, mut rollout_items) =
        make_spine_session_with_closed_child("sidecar replay mismatch").await;
    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: "stale compact checkpoint".to_string(),
        replacement_history: Some(vec![user_message("stale replacement_history should fail")]),
    }));
    let raw_live = spine_raw_items_after_rollback(&rollout_items)
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&source_rollout_path, &live_rollout_path, &raw_live);

    let err = session
        .restore_context_from_rollout(&turn_context, &rollout_items)
        .await
        .expect_err("replay mismatch should fail");
    assert!(
        err.to_string()
            .contains("failed to rebuild Spine runtime from rollout"),
        "unexpected replay error: {err}"
    );
    assert_eq!(
        session.clone_history().await.raw_items(),
        original_history.as_slice()
    );
}

#[tokio::test]
async fn replacement_history_not_used_as_spine_replay_source() {
    assert_rendered_replacement_history_fails_closed().await;
}

#[tokio::test]
async fn resume_rejects_unprovable_rendered_memory() {
    assert_rendered_replacement_history_fails_closed().await;
}

async fn assert_rendered_replacement_history_fails_closed() {
    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    assert!(!SpineStore::has_for_rollout(&rollout_path).expect("check sidecar"));
    let replacement_history = vec![user_message(
        "<spine_memory>rendered snapshot only</spine_memory>",
    )];

    let err = resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: vec![RolloutItem::Compacted(CompactedItem {
                message: "host compact checkpoint".to_string(),
                replacement_history: Some(replacement_history),
            })],
            rollout_path: Some(rollout_path),
        }))
        .await
        .expect_err("missing sidecar should fail closed");
    assert!(
        err.to_string()
            .contains("spine_jit resume requires Spine sidecar"),
        "unexpected resume error: {err}"
    );
}

#[tokio::test]
async fn replacement_history_missing_boundary_proof_fails_closed() {
    let (_source_session, _source_turn_context, source_rollout_path, mut rollout_items) =
        make_spine_session_with_closed_child("sidecar resume summary").await;
    let expected_raw_items = spine_raw_items_after_rollback(&rollout_items);
    let expected_runtime =
        SpineRuntime::load_for_rollout_items(&source_rollout_path, &expected_raw_items, &[])
            .expect("load source spine runtime")
            .expect("source sidecar should exist");
    let expected_materialized = expected_runtime
        .materialize_variable_context_for_test(&expected_raw_items)
        .expect("materialize source h(PS)");

    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: "matching compact checkpoint".to_string(),
        replacement_history: Some(expected_materialized.clone()),
    }));

    let (mut resumed_session, _resumed_context, rx) =
        make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("Test API Key"),
            Vec::new(),
            |config| {
                config
                    .features
                    .enable(Feature::SpineJit)
                    .expect("enable spine feature");
            },
        )
        .await;
    let resumed_rollout_path = attach_thread_persistence(
        Arc::get_mut(&mut resumed_session).expect("session should be unique"),
    )
    .await;
    let raw_live = spine_raw_items_after_rollback(&rollout_items)
        .iter()
        .map(Option::is_some)
        .collect::<Vec<_>>();
    clone_spine_sidecar_for_test(&source_rollout_path, &resumed_rollout_path, &raw_live);

    let err = resumed_session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(source_rollout_path),
        }))
        .await
        .expect_err("matching replacement_history without compact proof should fail closed");
    assert_eq!(
        expected_materialized,
        expected_runtime
            .materialize_variable_context_for_test(&expected_raw_items)
            .expect("source h(PS) should remain stable")
    );
    assert!(
        err.to_string()
            .contains("missing spine compact checkpoint at raw boundary"),
        "unexpected resume error: {err}"
    );
}

#[tokio::test]
async fn spine_resume_non_spine_session_fails_closed() {
    assert_spine_resume_non_spine_session_fails_closed().await;
}

#[tokio::test]
async fn resume_non_spine_session_in_spine_mode_fails_closed() {
    assert_spine_resume_non_spine_session_fails_closed().await;
}

async fn assert_spine_resume_non_spine_session_fails_closed() {
    let (mut session, _turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    let rollout_items = vec![RolloutItem::ResponseItem(user_message(
        "ordinary non-spine history",
    ))];

    let err = session
        .record_initial_history(InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::default(),
            history: rollout_items,
            rollout_path: Some(rollout_path),
        }))
        .await
        .expect_err("missing sidecar should fail closed");
    assert!(
        err.to_string()
            .contains("spine_jit resume requires Spine sidecar"),
        "unexpected resume error: {err}"
    );
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_persists_split_file_system_policy_to_rollout()
 {
    let (mut session, mut turn_context) = make_session_and_context().await;
    let file_system_sandbox_policy = file_system_policy_with_unreadable_glob(&turn_context);
    turn_context.permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
        turn_context.permission_profile.enforcement(),
        &file_system_sandbox_policy,
        turn_context.network_sandbox_policy(),
    );
    let rollout_path = attach_thread_persistence(&mut session).await;

    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record context updates");
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let persisted_file_system_sandbox_policy = resumed.history.iter().find_map(|item| match item {
        RolloutItem::TurnContext(ctx) => ctx.file_system_sandbox_policy.clone(),
        _ => None,
    });
    assert_eq!(
        persisted_file_system_sandbox_policy,
        Some(file_system_sandbox_policy)
    );
}

#[tokio::test]
async fn build_initial_context_prepends_model_switch_message() {
    let (session, turn_context) = make_session_and_context().await;
    let previous_turn_settings = PreviousTurnSettings {
        model: "previous-regular-model".to_string(),
        realtime_active: None,
    };

    session
        .set_previous_turn_settings(Some(previous_turn_settings))
        .await;
    let initial_context = session.build_initial_context(&turn_context).await;

    let ResponseItem::Message { role, content, .. } = &initial_context[0] else {
        panic!("expected developer message");
    };
    assert_eq!(role, "developer");
    let [ContentItem::InputText { text }, ..] = content.as_slice() else {
        panic!("expected developer text");
    };
    assert!(text.contains("<model_switch>"));
}

#[tokio::test]
async fn record_context_updates_and_set_reference_context_item_persists_full_reinjection_to_rollout()
 {
    let (mut session, previous_context) = make_session_and_context().await;
    let next_model = if previous_context.model_info.slug == "gpt-5.4" {
        "gpt-5.2"
    } else {
        "gpt-5.4"
    };
    let turn_context = previous_context
        .with_model(next_model.to_string(), &session.services.models_manager)
        .await;
    let rollout_path = attach_thread_persistence(&mut session).await;

    session
        .persist_rollout_items(&[RolloutItem::EventMsg(EventMsg::UserMessage(
            UserMessageEvent {
                message: "seed rollout".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
            },
        ))])
        .await;
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(/*item*/ None);
    }

    session
        .set_previous_turn_settings(Some(PreviousTurnSettings {
            model: previous_context.model_info.slug.clone(),
            realtime_active: Some(previous_context.realtime_active),
        }))
        .await;
    session
        .record_context_updates_and_set_reference_context_item(&turn_context)
        .await
        .expect("record context updates");
    session.ensure_rollout_materialized().await;
    session.flush_rollout().await.expect("rollout should flush");

    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let persisted_turn_context = resumed.history.iter().find_map(|item| match item {
        RolloutItem::TurnContext(ctx) => Some(ctx.clone()),
        _ => None,
    });

    assert_eq!(
        serde_json::to_value(persisted_turn_context)
            .expect("serialize persisted turn context item"),
        serde_json::to_value(Some(turn_context.to_turn_context_item()))
            .expect("serialize expected turn context item")
    );
}

#[tokio::test]
async fn run_user_shell_command_does_not_set_reference_context_item() {
    let (session, _turn_context, rx) = make_session_and_context_with_rx().await;
    {
        let mut state = session.state.lock().await;
        state.set_reference_context_item(/*item*/ None);
    }

    handlers::run_user_shell_command(&session, "sub-id".to_string(), "echo shell".to_string())
        .await;

    let deadline = StdDuration::from_secs(15);
    let start = std::time::Instant::now();
    loop {
        let remaining = deadline.saturating_sub(start.elapsed());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        if matches!(evt.msg, EventMsg::TurnComplete(_)) {
            break;
        }
    }

    assert!(
        session.reference_context_item().await.is_none(),
        "standalone shell tasks should not mutate previous context"
    );
}

#[tokio::test]
async fn realtime_conversation_list_voices_emits_builtin_list() {
    let (session, _turn_context, rx) = make_session_and_context_with_rx().await;

    handlers::realtime_conversation_list_voices(&session, "sub-id".to_string()).await;

    let event = rx.recv().await.expect("event");
    let voices = match event.msg {
        EventMsg::RealtimeConversationListVoicesResponse(
            RealtimeConversationListVoicesResponseEvent { voices },
        ) => voices,
        msg => panic!("expected list voices response, got {msg:?}"),
    };
    assert_eq!(
        voices,
        RealtimeVoicesList {
            v1: vec![
                RealtimeVoice::Juniper,
                RealtimeVoice::Maple,
                RealtimeVoice::Spruce,
                RealtimeVoice::Ember,
                RealtimeVoice::Vale,
                RealtimeVoice::Breeze,
                RealtimeVoice::Arbor,
                RealtimeVoice::Sol,
                RealtimeVoice::Cove,
            ],
            v2: vec![
                RealtimeVoice::Alloy,
                RealtimeVoice::Ash,
                RealtimeVoice::Ballad,
                RealtimeVoice::Coral,
                RealtimeVoice::Echo,
                RealtimeVoice::Sage,
                RealtimeVoice::Shimmer,
                RealtimeVoice::Verse,
                RealtimeVoice::Marin,
                RealtimeVoice::Cedar,
            ],
            default_v1: RealtimeVoice::Cove,
            default_v2: RealtimeVoice::Marin,
        },
    );
}

#[derive(Clone, Copy)]
struct NeverEndingTask {
    kind: TaskKind,
    listen_to_cancellation_token: bool,
}

impl SessionTask for NeverEndingTask {
    fn kind(&self) -> TaskKind {
        self.kind
    }

    fn span_name(&self) -> &'static str {
        "session_task.never_ending"
    }

    async fn run(
        self: Arc<Self>,
        _session: Arc<SessionTaskContext>,
        _ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> crate::session::turn::TurnOutput {
        if self.listen_to_cancellation_token {
            cancellation_token.cancelled().await;
            return crate::session::turn::TurnOutput::complete(None);
        }
        loop {
            sleep(Duration::from_secs(60)).await;
        }
    }
}

#[derive(Clone, Copy)]
struct GuardianDeniedApprovalTask;

impl SessionTask for GuardianDeniedApprovalTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.guardian_denied_approval"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        _input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> crate::session::turn::TurnOutput {
        let session = session.clone_session();
        for _ in 0..3 {
            crate::guardian::record_guardian_denial_for_test(&session, &ctx, &ctx.sub_id).await;
        }

        cancellation_token.cancelled().await;
        crate::session::turn::TurnOutput::complete(None)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guardian_auto_review_interrupts_after_three_consecutive_denials() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "trigger guardian denials".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(Arc::clone(&tc), input, GuardianDeniedApprovalTask)
        .await;

    let mut observed = Vec::new();
    let aborted = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let event = rx.recv().await.expect("event");
            if let EventMsg::TurnAborted(event) = &event.msg {
                let event = event.clone();
                observed.push(EventMsg::TurnAborted(event.clone()));
                break event;
            }
            observed.push(event.msg);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "guardian denial circuit breaker should interrupt the turn; observed events: {observed:?}"
        )
    });
    assert_eq!(aborted.reason, TurnAbortReason::Interrupted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guardian_helper_review_interrupts_after_three_consecutive_denials() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "keep turn active for helper reviews".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    let session_for_review = Arc::clone(&sess);
    let turn_for_review = Arc::clone(&tc);
    let turn_id = tc.sub_id.clone();
    let review_thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper review runtime");
        runtime.block_on(async move {
            for _ in 0..3 {
                crate::guardian::record_guardian_denial_for_test(
                    &session_for_review,
                    &turn_for_review,
                    &turn_id,
                )
                .await;
            }
        });
    });
    review_thread.join().expect("helper review thread");

    let mut observed = Vec::new();
    let aborted = timeout(StdDuration::from_secs(5), async {
        loop {
            let event = rx.recv().await.expect("event");
            if let EventMsg::TurnAborted(event) = &event.msg {
                let event = event.clone();
                observed.push(EventMsg::TurnAborted(event.clone()));
                break event;
            }
            observed.push(event.msg);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "helper review circuit breaker should interrupt the turn; observed events: {observed:?}"
        )
    });
    assert_eq!(aborted.reason, TurnAbortReason::Interrupted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_log::test]
async fn abort_regular_task_emits_turn_aborted_only() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    // Interrupts persist a model-visible `<turn_aborted>` marker into history, but there is no
    // separate client-visible event for that marker (only `EventMsg::TurnAborted`).
    let evt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("event");
    match evt.msg {
        EventMsg::TurnAborted(e) => assert_eq!(TurnAbortReason::Interrupted, e.reason),
        other => panic!("unexpected event: {other:?}"),
    }
    // No extra events should be emitted after an abort.
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_log::test]
async fn spine_interrupt_marker_is_sidecar_covered_without_raw_event() {
    let (mut session, tc, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    let sess = session;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    let evt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("event");
    match evt.msg {
        EventMsg::TurnAborted(e) => assert_eq!(TurnAbortReason::Interrupted, e.reason),
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(rx.try_recv().is_err());

    assert_session_history_matches_spine_materialization(&sess, &rollout_path).await;
    let history = sess.clone_history().await;
    assert!(
        history.raw_items().iter().any(|item| {
            let ResponseItem::Message { role, content, .. } = item else {
                return false;
            };
            if role != "user" {
                return false;
            }
            content.iter().any(|content_item| {
                let ContentItem::InputText { text } = content_item else {
                    return false;
                };
                TurnAborted::matches_text(text)
            })
        }),
        "expected interrupted-turn marker in history"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_log::test]
async fn turn_abort_clears_stale_spine_pending_transition() {
    let (mut session, tc, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let _rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session.on_init().await.expect("initialize spine");
    let work = user_message("work before interrupted close");
    session
        .record_conversation_items(&tc, std::slice::from_ref(&work))
        .await
        .expect("record work");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "abort-close");
    session
        .record_conversation_items(&tc, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "abort-close".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    assert_pending_spine_commit(&session, "abort-close").await;

    let sess = session;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    let mut observed = Vec::new();
    let aborted = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.expect("event");
            if let EventMsg::TurnAborted(event) = &event.msg {
                let event = event.clone();
                observed.push(EventMsg::TurnAborted(event.clone()));
                break event;
            }
            observed.push(event.msg);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("turn abort should emit TurnAborted; observed events: {observed:?}")
    });
    assert_eq!(TurnAbortReason::Interrupted, aborted.reason);
    assert_no_pending_spine_commit(&sess, "abort-close").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_log::test]
async fn turn_abort_closes_pending_spine_toolcall_with_native_aborted_output() {
    let (mut session, tc, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let _rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session.on_init().await.expect("initialize spine");

    let work = user_message("work before interrupted close");
    session
        .record_conversation_items(&tc, std::slice::from_ref(&work))
        .await
        .expect("record work");
    let close_request = spine_call(SPINE_TOOL_CLOSE, "abort-close-native");
    session
        .record_conversation_items(&tc, std::slice::from_ref(&close_request))
        .await
        .expect("record close request");
    session
        .test_seed_spine_close_control_request(
            "abort-close-native".to_string(),
            "test node memory".to_string(),
        )
        .await
        .expect("stage close");
    assert_pending_spine_commit(&session, "abort-close-native").await;

    let sess = session;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    let mut observed = Vec::new();
    let aborted = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.expect("event");
            if let EventMsg::TurnAborted(event) = &event.msg {
                let event = event.clone();
                observed.push(EventMsg::TurnAborted(event.clone()));
                break event;
            }
            observed.push(event.msg);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("turn abort should emit TurnAborted; observed events: {observed:?}")
    });
    assert_eq!(TurnAbortReason::Interrupted, aborted.reason);
    assert_no_pending_spine_commit(&sess, "abort-close-native").await;

    let history = sess.clone_history().await;
    assert!(
        history.raw_items().iter().any(|item| {
            let ResponseItem::FunctionCallOutput { call_id, output } = item else {
                return false;
            };
            if call_id != "abort-close-native" {
                return false;
            }
            output.success == Some(false)
                && matches!(
                    &output.body,
                    FunctionCallOutputBody::Text(text) if text.contains("aborted by user")
                )
        }),
        "expected native aborted tool output in history after interrupt"
    );
    assert!(
        !history.raw_items().iter().any(|item| {
            let ResponseItem::FunctionCallOutput { call_id, output } = item else {
                return false;
            };
            call_id == "abort-close-native"
                && matches!(
                    &output.body,
                    FunctionCallOutputBody::Text(text) if text.starts_with("SPINE_TOOL_USE_FAILED:")
                )
        }),
        "interrupt cleanup should not synthesize a Spine-specific failure output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[test_log::test]
async fn turn_abort_closes_unmatched_ordinary_tool_requests_before_abort_marker() {
    let (mut session, tc, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session.on_init().await.expect("initialize spine");

    session
        .record_conversation_items(&tc, &[user_message("work before interrupted tools")])
        .await
        .expect("record work");
    session
        .record_conversation_items(
            &tc,
            &[
                function_call("shell_command", "ordinary-abort-1"),
                function_call("shell_command", "ordinary-abort-2"),
            ],
        )
        .await
        .expect("record ordinary tool requests");

    let sess = session;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    let mut observed = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.expect("event");
            if matches!(event.msg, EventMsg::TurnAborted(_)) {
                break;
            }
            observed.push(event.msg);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("turn abort should emit TurnAborted; observed events: {observed:?}")
    });

    let history = sess.clone_history().await;
    let items = history.raw_items();
    let output_1 = items
        .iter()
        .position(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, output }
                    if call_id == "ordinary-abort-1"
                        && output.success == Some(false)
                        && matches!(
                            &output.body,
                            FunctionCallOutputBody::Text(text) if text.contains("aborted by user")
                        )
            )
        })
        .expect("first ordinary request should receive native aborted output");
    let output_2 = items
        .iter()
        .position(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, output }
                    if call_id == "ordinary-abort-2"
                        && output.success == Some(false)
                        && matches!(
                            &output.body,
                            FunctionCallOutputBody::Text(text) if text.contains("aborted by user")
                        )
            )
        })
        .expect("second ordinary request should receive native aborted output");
    let marker = items
        .iter()
        .position(|item| message_text_contains(item, "<turn_aborted>"))
        .expect("interrupted-turn marker should be recorded");

    assert!(
        output_1 < marker && output_2 < marker,
        "ordinary aborted outputs must be recorded before the turn-aborted marker"
    );
    sess.ensure_rollout_materialized().await;
    sess.flush_rollout().await.expect("rollout should flush");
    let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
        .await
        .expect("read rollout history")
    else {
        panic!("expected resumed rollout history");
    };
    let raw_items = spine_raw_items_after_rollback(&resumed.history);
    SpineRuntime::load_for_rollout_items(&rollout_path, &raw_items, &[])
        .expect("load spine runtime after interrupted ordinary tool fallback")
        .expect("spine sidecar should exist");
    let rollout_items = resumed
        .history
        .iter()
        .filter_map(|item| {
            if let RolloutItem::ResponseItem(item) = item {
                Some(item)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let rollout_output_1 = rollout_items
        .iter()
        .position(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, output }
                    if call_id == "ordinary-abort-1"
                        && matches!(
                            &output.body,
                            FunctionCallOutputBody::Text(text) if text.contains("aborted by user")
                        )
            )
        })
        .expect("first ordinary aborted output should be durable");
    let rollout_output_2 = rollout_items
        .iter()
        .position(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { call_id, output }
                    if call_id == "ordinary-abort-2"
                        && matches!(
                            &output.body,
                            FunctionCallOutputBody::Text(text) if text.contains("aborted by user")
                        )
            )
        })
        .expect("second ordinary aborted output should be durable");
    let rollout_marker = rollout_items
        .iter()
        .position(|item| message_text_contains(item, "<turn_aborted>"))
        .expect("interrupted-turn marker should be durable");
    assert!(
        rollout_output_1 < rollout_marker && rollout_output_2 < rollout_marker,
        "ordinary aborted outputs must be durable before the turn-aborted marker"
    );
}

#[tokio::test]
async fn spine_user_prompt_append_creates_sidecar_coverage() {
    let (mut session, turn_context, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    let input = vec![UserInput::Text {
        text: "continue".to_string(),
        text_elements: Vec::new(),
    }];

    session
        .record_user_prompt_and_emit_turn_item(&turn_context, &input, user_message("continue"))
        .await
        .expect("record user prompt");

    assert_session_history_matches_spine_materialization(&session, &rollout_path).await;
}

#[tokio::test]
async fn abort_gracefully_emits_turn_aborted_only() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    // Even if tasks handle cancellation gracefully, interrupts still result in `TurnAborted`
    // being the only client-visible signal.
    let evt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for event")
        .expect("event");
    match evt.msg {
        EventMsg::TurnAborted(e) => assert_eq!(TurnAbortReason::Interrupted, e.reason),
        other => panic!("unexpected event: {other:?}"),
    }
    // No extra events should be emitted after an abort.
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_finish_emits_turn_item_lifecycle_for_leftover_pending_user_input() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    while rx.try_recv().is_ok() {}

    sess.inject_response_items(vec![ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "late pending input".to_string(),
        }],
        phase: None,
    }])
    .await
    .expect("inject pending input into active turn");

    sess.on_task_finished(
        Arc::clone(&tc),
        crate::session::turn::TurnOutput::complete(/*last_agent_message*/ None),
    )
    .await;

    let history = sess.clone_history().await;
    let expected = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "late pending input".to_string(),
        }],
        phase: None,
    };
    assert!(
        history.raw_items().iter().any(|item| item == &expected),
        "expected pending input to be persisted into history on turn completion"
    );

    let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected raw response item event")
        .expect("channel open");
    assert!(matches!(first.msg, EventMsg::RawResponseItem(_)));

    let second = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected item started event")
        .expect("channel open");
    assert!(matches!(
        second.msg,
        EventMsg::ItemStarted(ItemStartedEvent {
            item: TurnItem::UserMessage(UserMessageItem { content, .. }),
            ..
        }) if content == vec![UserInput::Text {
            text: "late pending input".to_string(),
            text_elements: Vec::new(),
        }]
    ));

    let third = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected item completed event")
        .expect("channel open");
    assert!(matches!(
        third.msg,
        EventMsg::ItemCompleted(ItemCompletedEvent {
            item: TurnItem::UserMessage(UserMessageItem { content, .. }),
            ..
        }) if content == vec![UserInput::Text {
            text: "late pending input".to_string(),
            text_elements: Vec::new(),
        }]
    ));

    let fourth = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected legacy user message event")
        .expect("channel open");
    assert!(matches!(
        fourth.msg,
        EventMsg::UserMessage(UserMessageEvent {
            message,
            images,
            text_elements,
            local_images,
        }) if message == "late pending input"
            && images == Some(Vec::new())
            && text_elements.is_empty()
            && local_images.is_empty()
    ));

    let fifth = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected turn complete event")
        .expect("channel open");
    assert!(matches!(
        fifth.msg,
        EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id,
            last_agent_message: None,
            time_to_first_token_ms: None,
            ..
        }) if turn_id == tc.sub_id
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_finish_pending_input_append_failure_does_not_emit_turn_complete() {
    let (mut session, tc, rx) = make_session_and_context_with_auth_and_config_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        |config| {
            config
                .features
                .enable(Feature::SpineJit)
                .expect("enable spine feature");
        },
    )
    .await;
    let rollout_path =
        attach_thread_persistence(Arc::get_mut(&mut session).expect("session should be unique"))
            .await;
    session.on_init().await.expect("initialize spine");
    let sess = session;
    sess.spawn_task(
        Arc::clone(&tc),
        vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }],
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;
    while rx.try_recv().is_ok() {}

    sess.inject_response_items(vec![ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "late pending input".to_string(),
        }],
        phase: None,
    }])
    .await
    .expect("inject pending input into active turn");

    let store = SpineStore::for_rollout(&rollout_path).expect("spine store");
    let broken_tree_path = store.tree_path_for_test();
    let mut tree_permissions = std::fs::metadata(&broken_tree_path)
        .expect("tree metadata")
        .permissions();
    tree_permissions.set_readonly(true);
    std::fs::set_permissions(&broken_tree_path, tree_permissions)
        .expect("make tree ledger readonly");

    sess.on_task_finished(
        Arc::clone(&tc),
        crate::session::turn::TurnOutput::complete(/*last_agent_message*/ None),
    )
    .await;

    let mut tree_permissions = std::fs::metadata(&broken_tree_path)
        .expect("tree metadata")
        .permissions();
    tree_permissions.set_readonly(false);
    std::fs::set_permissions(&broken_tree_path, tree_permissions)
        .expect("restore tree ledger permissions");

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("expected append error")
        .expect("channel open");
    assert!(
        matches!(event.msg, EventMsg::Error(_)),
        "unexpected event: {:?}",
        event.msg
    );
    assert!(
        rx.try_recv().is_err(),
        "fatal pending append failure must not emit normal turn completion"
    );
    assert!(
        sess.inject_response_items(vec![ResponseInputItem::Message {
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "new input".to_string(),
            }],
            phase: None,
        }])
        .await
        .is_err(),
        "failed cleanup should clear the active turn"
    );
}

#[tokio::test]
async fn steer_input_requires_active_turn() {
    let (sess, _tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "steer".to_string(),
        text_elements: Vec::new(),
    }];

    let err = sess
        .steer_input(
            input, /*expected_turn_id*/ None, /*responsesapi_client_metadata*/ None,
        )
        .await
        .expect_err("steering without active turn should fail");

    assert!(matches!(err, SteerInputError::NoActiveTurn(_)));
}

#[tokio::test]
async fn steer_input_enforces_expected_turn_id() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    let steer_input = vec![UserInput::Text {
        text: "steer".to_string(),
        text_elements: Vec::new(),
    }];
    let err = sess
        .steer_input(
            steer_input,
            Some("different-turn-id"),
            /*responsesapi_client_metadata*/ None,
        )
        .await
        .expect_err("mismatched expected turn id should fail");

    match err {
        SteerInputError::ExpectedTurnMismatch { expected, actual } => {
            assert_eq!(
                (expected, actual),
                ("different-turn-id".to_string(), tc.sub_id.clone())
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn steer_input_rejects_non_regular_turns() {
    for (task_kind, turn_kind) in [
        (TaskKind::Review, NonSteerableTurnKind::Review),
        (TaskKind::Compact, NonSteerableTurnKind::Compact),
    ] {
        let (sess, _tc, rx) = make_session_and_context_with_rx().await;
        let input = vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }];
        let turn_context = sess.new_default_turn_with_sub_id("turn".to_string()).await;
        sess.spawn_task(
            turn_context,
            input,
            NeverEndingTask {
                kind: task_kind,
                listen_to_cancellation_token: true,
            },
        )
        .await;

        let steer_input = vec![UserInput::Text {
            text: "steer".to_string(),
            text_elements: Vec::new(),
        }];
        let err = sess
            .steer_input(
                steer_input,
                /*expected_turn_id*/ None,
                /*responsesapi_client_metadata*/ None,
            )
            .await
            .expect_err("steering a non-regular turn should fail");

        assert_eq!(err, SteerInputError::ActiveTurnNotSteerable { turn_kind });

        sess.abort_all_tasks(TurnAbortReason::Interrupted).await;
    }
}

#[tokio::test]
async fn steer_input_returns_active_turn_id() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    let steer_input = vec![UserInput::Text {
        text: "steer".to_string(),
        text_elements: Vec::new(),
    }];
    let turn_id = sess
        .steer_input(
            steer_input,
            Some(&tc.sub_id),
            /*responsesapi_client_metadata*/ None,
        )
        .await
        .expect("steering with matching expected turn id should succeed");

    assert_eq!(turn_id, tc.sub_id);
    assert!(sess.has_pending_input().await);
}

#[tokio::test]
async fn prepend_pending_input_keeps_older_tail_ahead_of_newer_input() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(
        Arc::clone(&tc),
        input,
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    let blocked = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "blocked queued prompt".to_string(),
        }],
        phase: None,
    };
    let later = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "later queued prompt".to_string(),
        }],
        phase: None,
    };
    let newer = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "newer queued prompt".to_string(),
        }],
        phase: None,
    };

    sess.inject_response_items(vec![blocked.clone(), later.clone()])
        .await
        .expect("inject initial pending input into active turn");

    let drained = sess.get_pending_input().await;
    assert_eq!(drained, vec![blocked, later.clone()]);

    sess.inject_response_items(vec![newer.clone()])
        .await
        .expect("inject newer pending input into active turn");

    let mut drained_iter = drained.into_iter();
    let _blocked = drained_iter.next().expect("blocked prompt should exist");
    sess.prepend_pending_input(drained_iter.collect())
        .await
        .expect("requeue later pending input at the front of the queue");

    assert_eq!(sess.get_pending_input().await, vec![later, newer]);
}

#[tokio::test]
async fn queued_response_items_for_next_turn_move_into_next_active_turn() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let queued_item = ResponseInputItem::Message {
        role: "assistant".to_string(),
        content: vec![ContentItem::InputText {
            text: "queued before wake".to_string(),
        }],
        phase: None,
    };

    sess.queue_response_items_for_next_turn(vec![queued_item.clone()])
        .await;

    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    assert_eq!(sess.get_pending_input().await, vec![queued_item]);
}

#[tokio::test]
async fn idle_interrupt_does_not_wake_queued_next_turn_items() {
    let (sess, _tc, rx) = make_session_and_context_with_rx().await;
    let queued_item = ResponseInputItem::Message {
        role: "assistant".to_string(),
        content: vec![ContentItem::InputText {
            text: "queued before interrupt".to_string(),
        }],
        phase: None,
    };

    sess.queue_response_items_for_next_turn(vec![queued_item])
        .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    assert!(sess.active_turn.lock().await.is_none());
    assert!(sess.has_queued_response_items_for_next_turn().await);
}

#[tokio::test]
async fn abort_empty_active_turn_preserves_pending_input() {
    let (sess, _tc, rx) = make_session_and_context_with_rx().await;
    let pending_item = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "late pending input".to_string(),
        }],
        phase: None,
    };
    let turn_state = {
        let mut active = sess.active_turn.lock().await;
        let active_turn = active.get_or_insert_with(ActiveTurn::default);
        Arc::clone(&active_turn.turn_state)
    };
    turn_state
        .lock()
        .await
        .push_pending_input(pending_item.clone());

    sess.abort_all_tasks(TurnAbortReason::Replaced).await;

    assert!(sess.active_turn.lock().await.is_none());
    assert_eq!(
        turn_state.lock().await.take_pending_input(),
        vec![pending_item]
    );
}

#[tokio::test]
async fn interrupt_accounts_active_goal_before_pausing() -> anyhow::Result<()> {
    let (sess, tc, rx, _codex_home) = make_goal_session_and_context_with_rx().await;
    sess.set_thread_goal(
        tc.as_ref(),
        SetGoalRequest {
            objective: Some("Keep improving the benchmark".to_string()),
            status: None,
            token_budget: None,
        },
    )
    .await?;

    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;
    set_total_token_usage(&sess, post_goal_token_usage()).await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    let goal = sess
        .get_thread_goal()
        .await?
        .expect("goal should remain persisted after interrupt");
    assert_eq!(
        codex_protocol::protocol::ThreadGoalStatus::Paused,
        goal.status
    );
    assert_eq!(70, goal.tokens_used);

    assert!(sess.active_turn.lock().await.is_none());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_goal_continuation_runs_again_after_no_tool_turn() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::Goals)
            .expect("goal mode should be enableable in tests");
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    "call-create-goal",
                    "create_goal",
                    r#"{"objective":"write a benchmark note"}"#,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "Draft ready."),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_assistant_message("msg-2", "I am still working on the benchmark note."),
                ev_completed("resp-3"),
            ]),
            sse(vec![
                ev_response_created("resp-4"),
                ev_function_call(
                    "call-complete-goal",
                    "update_goal",
                    r#"{"status":"complete"}"#,
                ),
                ev_completed("resp-4"),
            ]),
            sse(vec![
                ev_assistant_message("msg-3", "Goal complete."),
                ev_completed("resp-5"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "write a benchmark note".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;

    let mut completed_turns = 0;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let event = test.codex.next_event().await?;
            if matches!(event.msg, EventMsg::TurnComplete(_)) {
                completed_turns += 1;
                if completed_turns == 3 {
                    return anyhow::Ok(());
                }
            }
        }
    })
    .await??;

    let continuation_request = responses
        .requests()
        .into_iter()
        .find(|request| request.body_contains_text("<goal_context>"))
        .expect("expected a goal continuation request");
    let body = continuation_request.body_json();
    let goal_context_message = body["input"]
        .as_array()
        .expect("input should be an array")
        .iter()
        .find(|item| item.to_string().contains("<goal_context>"))
        .expect("goal context message should be present");
    assert_eq!(goal_context_message["role"].as_str(), Some("user"));
    assert!(
        goal_context_message
            .to_string()
            .contains("Continue working toward the active thread goal.")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_request_user_input_does_not_spawn_extra_goal_continuation() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::Goals)
            .expect("goal mode should be enableable in tests");
        config
            .features
            .enable(Feature::DefaultModeRequestUserInput)
            .expect("default-mode request_user_input should be enableable in tests");
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    "call-create-goal",
                    "create_goal",
                    r#"{"objective":"write a benchmark note"}"#,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "Draft ready."),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_function_call(
                    "call-ask-user",
                    "request_user_input",
                    r#"{"questions":[{"header":"Choice","id":"next_step","question":"Pick one","options":[{"label":"Outline","description":"Start with an outline."},{"label":"Draft","description":"Write a full draft."}]}]}"#,
                ),
                ev_completed("resp-3"),
            ]),
            sse(vec![
                ev_response_created("resp-4"),
                ev_function_call(
                    "call-complete-goal",
                    "update_goal",
                    r#"{"status":"complete"}"#,
                ),
                ev_completed("resp-4"),
            ]),
            sse(vec![
                ev_assistant_message("msg-2", "Goal complete."),
                ev_completed("resp-5"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "write a benchmark note".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;

    let request_user_input_event = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::RequestUserInput(event) => Some(event.clone()),
        _ => None,
    })
    .await;
    assert_eq!(3, responses.requests().len());
    assert!(
        timeout(Duration::from_millis(200), test.codex.next_event())
            .await
            .is_err(),
        "waiting for request_user_input should keep the turn open without emitting more events"
    );
    assert_eq!(
        3,
        responses.requests().len(),
        "waiting for request_user_input should not start another continuation request"
    );

    test.codex
        .submit(Op::UserInputAnswer {
            id: request_user_input_event.turn_id,
            response: RequestUserInputResponse {
                answers: std::collections::HashMap::from([(
                    "next_step".to_string(),
                    RequestUserInputAnswer {
                        answers: vec!["Outline".to_string()],
                    },
                )]),
            },
        })
        .await?;

    let mut completed_turns = 0;
    timeout(Duration::from_secs(8), async {
        loop {
            let event = test.codex.next_event().await?;
            if matches!(event.msg, EventMsg::TurnComplete(_)) {
                completed_turns += 1;
                if completed_turns == 1 {
                    return anyhow::Ok(());
                }
            }
        }
    })
    .await??;

    assert_eq!(5, responses.requests().len());

    Ok(())
}

async fn set_total_token_usage(sess: &Session, total_token_usage: TokenUsage) {
    let mut state = sess.state.lock().await;
    state.set_token_info(Some(TokenUsageInfo {
        total_token_usage,
        last_token_usage: TokenUsage::default(),
        model_context_window: None,
    }));
}

fn post_goal_token_usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 50,
        cached_input_tokens: 10,
        output_tokens: 30,
        reasoning_output_tokens: 5,
        total_tokens: 75,
    }
}

async fn goal_test_state_db(sess: &Session) -> anyhow::Result<crate::StateDbHandle> {
    if let Some(state_db) = sess.state_db() {
        return Ok(state_db);
    }
    let config = sess.get_config().await;
    codex_state::StateRuntime::init(config.sqlite_home.clone(), config.model_provider_id.clone())
        .await
}

#[tokio::test]
async fn budget_limited_accounting_steers_active_turn_without_aborting() -> anyhow::Result<()> {
    let (sess, tc, rx, _codex_home) = make_goal_session_and_context_with_rx().await;
    sess.set_thread_goal(
        tc.as_ref(),
        SetGoalRequest {
            objective: Some("Keep improving the benchmark".to_string()),
            status: None,
            token_budget: Some(Some(10)),
        },
    )
    .await?;
    sess.goal_runtime_apply(GoalRuntimeEvent::TurnStarted {
        turn_context: tc.as_ref(),
        token_usage: TokenUsage::default(),
    })
    .await?;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;
    while rx.try_recv().is_ok() {}

    set_total_token_usage(
        &sess,
        TokenUsage {
            input_tokens: 20,
            cached_input_tokens: 0,
            output_tokens: 5,
            reasoning_output_tokens: 0,
            total_tokens: 25,
        },
    )
    .await;

    sess.goal_runtime_apply(GoalRuntimeEvent::ToolCompleted {
        turn_context: tc.as_ref(),
        tool_name: "shell_command",
    })
    .await?;

    let pending_input = sess.get_pending_input().await;
    let [ResponseInputItem::Message { role, content, .. }] = pending_input.as_slice() else {
        panic!("expected one budget-limit steering message, got {pending_input:#?}");
    };
    assert_eq!("user", role);
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("expected one text span in budget-limit steering message, got {content:#?}");
    };
    assert!(text.starts_with("<goal_context>"));
    assert!(text.trim_end().ends_with("</goal_context>"));
    assert!(text.contains("budget_limited"));
    assert!(text.to_lowercase().contains("wrap up this turn soon"));
    assert!(sess.active_turn.lock().await.is_some());
    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(event.msg, EventMsg::TurnAborted(_)),
            "budget limit should steer the active turn instead of aborting it"
        );
    }

    let state_db = goal_test_state_db(sess.as_ref()).await?;
    let goal = state_db
        .get_thread_goal(sess.conversation_id)
        .await?
        .expect("goal should remain persisted after accounting");
    assert_eq!(codex_state::ThreadGoalStatus::BudgetLimited, goal.status);
    assert_eq!(25, goal.tokens_used);

    set_total_token_usage(
        &sess,
        TokenUsage {
            input_tokens: 30,
            cached_input_tokens: 0,
            output_tokens: 10,
            reasoning_output_tokens: 0,
            total_tokens: 40,
        },
    )
    .await;
    sess.goal_runtime_apply(GoalRuntimeEvent::ToolCompletedGoal {
        turn_context: tc.as_ref(),
    })
    .await?;

    let goal = state_db
        .get_thread_goal(sess.conversation_id)
        .await?
        .expect("goal should remain persisted after follow-up accounting");
    assert_eq!(codex_state::ThreadGoalStatus::BudgetLimited, goal.status);
    assert_eq!(40, goal.tokens_used);

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_goal_mutation_accounts_active_turn_before_status_change() -> anyhow::Result<()> {
    let (sess, tc, rx, _codex_home) = make_goal_session_and_context_with_rx().await;
    sess.set_thread_goal(
        tc.as_ref(),
        SetGoalRequest {
            objective: Some("Keep improving the benchmark".to_string()),
            status: None,
            token_budget: None,
        },
    )
    .await?;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;
    set_total_token_usage(&sess, post_goal_token_usage()).await;

    sess.goal_runtime_apply(GoalRuntimeEvent::ExternalMutationStarting)
        .await?;

    let state_db = goal_test_state_db(sess.as_ref()).await?;
    let goal = state_db
        .get_thread_goal(sess.conversation_id)
        .await?
        .expect("goal should remain persisted");
    assert_eq!(70, goal.tokens_used);

    let previous_goal = goal.clone();
    let goal_id = goal.goal_id.clone();
    let updated_goal = state_db
        .update_thread_goal(
            sess.conversation_id,
            codex_state::ThreadGoalUpdate {
                objective: None,
                status: Some(codex_state::ThreadGoalStatus::Complete),
                token_budget: None,
                expected_goal_id: Some(goal_id),
            },
        )
        .await?
        .expect("goal status update should succeed");
    sess.goal_runtime_apply(GoalRuntimeEvent::ExternalSet {
        external_set: ExternalGoalSet {
            goal: updated_goal,
            previous_status: ExternalGoalPreviousStatus::from(&previous_goal),
        },
    })
    .await?;

    assert!(sess.active_turn.lock().await.is_some());
    let goal = state_db
        .get_thread_goal(sess.conversation_id)
        .await?
        .expect("goal should remain persisted");
    assert_eq!(codex_state::ThreadGoalStatus::Complete, goal.status);
    assert_eq!(70, goal.tokens_used);

    sess.abort_all_tasks(TurnAbortReason::Replaced).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_objective_change_steers_active_turn() -> anyhow::Result<()> {
    let (sess, tc, rx, _codex_home) = make_goal_session_and_context_with_rx().await;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;

    let state_db = goal_test_state_db(sess.as_ref()).await?;
    let old_goal = state_db
        .replace_thread_goal(
            sess.conversation_id,
            "Keep improving the benchmark",
            codex_state::ThreadGoalStatus::Active,
            /*token_budget*/ Some(10_000),
        )
        .await?;
    let new_goal = state_db
        .replace_thread_goal(
            sess.conversation_id,
            "Write a concise benchmark summary",
            codex_state::ThreadGoalStatus::Active,
            /*token_budget*/ Some(10_000),
        )
        .await?;

    sess.goal_runtime_apply(GoalRuntimeEvent::ExternalSet {
        external_set: ExternalGoalSet {
            goal: new_goal,
            previous_status: ExternalGoalPreviousStatus::from(&old_goal),
        },
    })
    .await?;

    let pending_input = sess.get_pending_input().await;
    assert!(
        pending_input.iter().any(|item| {
            matches!(
                item,
                ResponseInputItem::Message { role, content, .. }
                    if role == "user"
                        && content.iter().any(|content| matches!(
                            content,
                            ContentItem::InputText { text }
                                if text.starts_with("<goal_context>")
                                    && text.trim_end().ends_with("</goal_context>")
                                    && text.contains("The active thread goal objective was edited")
                                    && text.contains("Write a concise benchmark summary")
                        ))
            )
        }),
        "expected objective-updated steering prompt in pending input: {pending_input:?}"
    );

    sess.abort_all_tasks(TurnAbortReason::Replaced).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_active_goal_set_marks_current_turn_for_accounting() -> anyhow::Result<()> {
    let (sess, tc, rx, _codex_home) = make_goal_session_and_context_with_rx().await;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: false,
        },
    )
    .await;
    set_total_token_usage(&sess, post_goal_token_usage()).await;

    let state_db = goal_test_state_db(sess.as_ref()).await?;
    let goal = state_db
        .replace_thread_goal(
            sess.conversation_id,
            "Keep improving the benchmark",
            codex_state::ThreadGoalStatus::Active,
            /*token_budget*/ None,
        )
        .await?;
    sess.goal_runtime_apply(GoalRuntimeEvent::ExternalSet {
        external_set: ExternalGoalSet {
            goal,
            previous_status: ExternalGoalPreviousStatus::NewGoal,
        },
    })
    .await?;

    set_total_token_usage(
        &sess,
        TokenUsage {
            input_tokens: 65,
            cached_input_tokens: 10,
            output_tokens: 40,
            reasoning_output_tokens: 5,
            total_tokens: 110,
        },
    )
    .await;
    sess.goal_runtime_apply(GoalRuntimeEvent::ToolCompleted {
        turn_context: tc.as_ref(),
        tool_name: "shell_command",
    })
    .await?;

    let goal = state_db
        .get_thread_goal(sess.conversation_id)
        .await?
        .expect("goal should remain persisted");
    assert_eq!(codex_state::ThreadGoalStatus::Active, goal.status);
    assert_eq!(25, goal.tokens_used);

    sess.abort_all_tasks(TurnAbortReason::Replaced).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_goal_accounts_current_turn_tokens_before_tool_response() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::Goals)
            .expect("goal mode should be enableable in tests");
    });
    let test = builder.build(&server).await?;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    "call-create-goal",
                    "create_goal",
                    r#"{"objective":"write a report","token_budget":500}"#,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_function_call(
                    "call-complete-goal",
                    "update_goal",
                    r#"{"status":"complete"}"#,
                ),
                ev_completed_with_tokens("resp-2", /*total_tokens*/ 580),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "Goal complete."),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

    test.codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "write a report".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await?;

    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let event = test.codex.next_event().await?;
            if matches!(event.msg, EventMsg::TurnComplete(_)) {
                return anyhow::Ok(());
            }
        }
    })
    .await??;

    let complete_output = responses
        .function_call_output_text("call-complete-goal")
        .expect("complete tool output should be sent to the model");
    let complete_output: serde_json::Value = serde_json::from_str(&complete_output)?;
    assert_eq!(complete_output["goal"]["tokensUsed"], 580);
    assert_eq!(complete_output["goal"]["status"], "complete");
    assert_eq!(complete_output["remainingTokens"], 0);
    assert_eq!(
        complete_output["completionBudgetReport"],
        "Goal achieved. Report final budget usage to the user: tokens used: 580 of 500."
    );
    let requests = responses.requests();
    let completion_followup_request = requests
        .last()
        .expect("completion tool output should be sent in a follow-up request");
    assert!(
        !completion_followup_request.body_contains_text("budget_limited"),
        "completion follow-up should not include budget-limit steering"
    );

    let state_db = codex_state::StateRuntime::init(
        test.config.sqlite_home.clone(),
        test.config.model_provider_id.clone(),
    )
    .await?;
    let persisted_goal = state_db
        .get_thread_goal(test.session_configured.thread_id)
        .await?
        .expect("goal should be persisted");
    assert_eq!(
        codex_state::ThreadGoalStatus::Complete,
        persisted_goal.status
    );
    assert_eq!(580, persisted_goal.tokens_used);

    Ok(())
}

#[tokio::test]
async fn queue_only_mailbox_mail_waits_for_next_turn_after_answer_boundary() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let communication = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("worker path should parse"),
        AgentPath::root(),
        Vec::new(),
        "late queue-only update".to_string(),
        /*trigger_turn*/ false,
    );
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.defer_mailbox_delivery_to_next_turn(&tc.sub_id).await;
    sess.enqueue_mailbox_communication(communication.clone());

    assert!(
        !sess.has_pending_input().await,
        "queue-only mailbox mail should stay buffered once the current turn emitted its answer"
    );
    assert_eq!(sess.get_pending_input().await, Vec::new());

    sess.abort_all_tasks(TurnAbortReason::Replaced).await;

    assert_eq!(
        sess.get_pending_input().await,
        vec![communication.to_response_input_item()],
    );
}

#[tokio::test]
async fn trigger_turn_mailbox_mail_waits_for_next_turn_after_answer_boundary() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.defer_mailbox_delivery_to_next_turn(&tc.sub_id).await;
    sess.enqueue_mailbox_communication(InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("worker path should parse"),
        AgentPath::root(),
        Vec::new(),
        "late trigger update".to_string(),
        /*trigger_turn*/ true,
    ));

    assert!(
        !sess.has_pending_input().await,
        "trigger-turn mailbox mail should not extend the current turn after its answer boundary"
    );

    sess.abort_all_tasks(TurnAbortReason::Replaced).await;

    assert!(sess.has_trigger_turn_mailbox_items().await);
}

#[tokio::test]
async fn steered_input_reopens_mailbox_delivery_for_current_turn() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let communication = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("worker path should parse"),
        AgentPath::root(),
        Vec::new(),
        "queued child update".to_string(),
        /*trigger_turn*/ false,
    );
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.defer_mailbox_delivery_to_next_turn(&tc.sub_id).await;
    sess.enqueue_mailbox_communication(communication.clone());
    sess.steer_input(
        vec![UserInput::Text {
            text: "follow up".to_string(),
            text_elements: Vec::new(),
        }],
        Some(&tc.sub_id),
        /*responsesapi_client_metadata*/ None,
    )
    .await
    .expect("steered input should be accepted");

    assert_eq!(
        sess.get_pending_input().await,
        vec![
            ResponseInputItem::from(vec![UserInput::Text {
                text: "follow up".to_string(),
                text_elements: Vec::new(),
            }]),
            communication.to_response_input_item(),
        ],
    );
}

#[tokio::test]
async fn stale_defer_mailbox_delivery_does_not_override_steered_input() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let communication = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("worker path should parse"),
        AgentPath::root(),
        Vec::new(),
        "queued child update".to_string(),
        /*trigger_turn*/ false,
    );
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.defer_mailbox_delivery_to_next_turn(&tc.sub_id).await;
    sess.enqueue_mailbox_communication(communication.clone());
    sess.steer_input(
        vec![UserInput::Text {
            text: "follow up".to_string(),
            text_elements: Vec::new(),
        }],
        Some(&tc.sub_id),
        /*responsesapi_client_metadata*/ None,
    )
    .await
    .expect("steered input should be accepted");

    sess.defer_mailbox_delivery_to_next_turn(&tc.sub_id).await;

    assert_eq!(
        sess.get_pending_input().await,
        vec![
            ResponseInputItem::from(vec![UserInput::Text {
                text: "follow up".to_string(),
                text_elements: Vec::new(),
            }]),
            communication.to_response_input_item(),
        ],
    );
}

#[tokio::test]
async fn tool_calls_reopen_mailbox_delivery_for_current_turn() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let communication = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("worker path should parse"),
        AgentPath::root(),
        Vec::new(),
        "queued child update".to_string(),
        /*trigger_turn*/ false,
    );
    sess.spawn_task(
        Arc::clone(&tc),
        Vec::new(),
        NeverEndingTask {
            kind: TaskKind::Regular,
            listen_to_cancellation_token: true,
        },
    )
    .await;

    sess.defer_mailbox_delivery_to_next_turn(&tc.sub_id).await;
    sess.enqueue_mailbox_communication(communication.clone());

    let item = ResponseItem::FunctionCall {
        id: None,
        name: "test_tool".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: "call-1".to_string(),
    };
    let mut ctx = HandleOutputCtx {
        sess: Arc::clone(&sess),
        turn_context: Arc::clone(&tc),
        turn_store: Arc::new(codex_extension_api::ExtensionData::new(tc.sub_id.clone())),
        tool_runtime: test_tool_runtime(Arc::clone(&sess), Arc::clone(&tc)),
        cancellation_token: CancellationToken::new(),
    };

    let output = handle_output_item_done(&mut ctx, item, /*previously_active_item*/ None)
        .await
        .expect("tool call should be handled");

    assert!(output.needs_follow_up);
    assert!(output.tool_future.is_some());
    assert_eq!(
        sess.get_pending_input().await,
        vec![communication.to_response_input_item()],
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_review_task_emits_exited_then_aborted_and_records_history() {
    let (sess, tc, rx) = make_session_and_context_with_rx().await;
    let input = vec![UserInput::Text {
        text: "start review".to_string(),
        text_elements: Vec::new(),
    }];
    sess.spawn_task(Arc::clone(&tc), input, ReviewTask::new())
        .await;

    sess.abort_all_tasks(TurnAbortReason::Interrupted).await;

    // Aborting a review task should exit review mode before surfacing the abort to the client.
    // We scan for these events (rather than relying on fixed ordering) since unrelated events
    // may interleave.
    let mut exited_review_mode_idx = None;
    let mut turn_aborted_idx = None;
    let mut idx = 0usize;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let evt = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("event");
        let event_idx = idx;
        idx = idx.saturating_add(1);
        match evt.msg {
            EventMsg::ExitedReviewMode(ev) => {
                assert!(ev.review_output.is_none());
                exited_review_mode_idx = Some(event_idx);
            }
            EventMsg::TurnAborted(ev) => {
                assert_eq!(TurnAbortReason::Interrupted, ev.reason);
                turn_aborted_idx = Some(event_idx);
                break;
            }
            _ => {}
        }
    }
    assert!(
        exited_review_mode_idx.is_some(),
        "expected ExitedReviewMode after abort"
    );
    assert!(
        turn_aborted_idx.is_some(),
        "expected TurnAborted after abort"
    );
    assert!(
        exited_review_mode_idx.unwrap() < turn_aborted_idx.unwrap(),
        "expected ExitedReviewMode before TurnAborted"
    );

    let history = sess.clone_history().await;
    // The `<turn_aborted>` marker is silent in the event stream, so verify it is still
    // recorded in history for the model.
    assert!(
        history.raw_items().iter().any(|item| {
            let ResponseItem::Message { role, content, .. } = item else {
                return false;
            };
            if role != "user" {
                return false;
            }
            content.iter().any(|content_item| {
                let ContentItem::InputText { text } = content_item else {
                    return false;
                };
                TurnAborted::matches_text(text)
            })
        }),
        "expected a model-visible turn aborted marker in history after interrupt"
    );
}

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "test builds a router from session-owned MCP manager state"
)]
async fn fatal_tool_error_stops_turn_and_reports_error() {
    let (session, turn_context, rx) = make_session_and_context_with_rx().await;
    let tools = {
        session
            .services
            .mcp_connection_manager
            .read()
            .await
            .list_all_tools()
            .await
    };
    let deferred_mcp_tools = Some(tools.clone());
    let router = ToolRouter::from_config(
        &turn_context.tools_config,
        crate::tools::router::ToolRouterParams {
            deferred_mcp_tools,
            mcp_tools: Some(tools),
            discoverable_tools: None,
            extension_tool_executors: Vec::new(),
            dynamic_tools: turn_context.dynamic_tools.as_slice(),
        },
    )
    .expect("build tool router");
    let item = ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: "call-1".to_string(),
        name: "shell_command".to_string(),
        input: "{}".to_string(),
    };

    let call = ToolRouter::build_tool_call(item.clone())
        .expect("build tool call")
        .expect("tool call present");
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let err = router
        .dispatch_tool_call_with_code_mode_result(
            Arc::clone(&session),
            Arc::clone(&turn_context),
            CancellationToken::new(),
            tracker,
            call,
            ToolCallSource::Direct,
        )
        .await
        .err()
        .expect("expected fatal error");

    match err {
        FunctionCallError::Fatal(message) => {
            assert_eq!(
                message,
                "tool shell_command invoked with incompatible payload"
            );
        }
        other => panic!("expected FunctionCallError::Fatal, got {other:?}"),
    }
}

async fn sample_rollout(
    session: &Session,
    _turn_context: &TurnContext,
) -> (Vec<RolloutItem>, Vec<ResponseItem>) {
    let mut rollout_items = Vec::new();
    let mut live_history = ContextManager::new();

    // Use the same turn_context source as record_initial_history so model_info (and thus
    // personality_spec) matches reconstruction.
    let reconstruction_turn = session.new_default_turn().await;
    let mut initial_context = session
        .build_initial_context(reconstruction_turn.as_ref())
        .await;
    // Ensure personality_spec is present when Personality is enabled, so expected matches
    // what reconstruction produces (build_initial_context may omit it when baked into model).
    if !initial_context.iter().any(|m| {
        matches!(m, ResponseItem::Message { role, content, .. }
        if role == "developer"
            && content.iter().any(|c| {
                matches!(c, ContentItem::InputText { text } if text.contains("<personality_spec>"))
            }))
    }) && let Some(p) = reconstruction_turn.personality
        && session.features.enabled(Feature::Personality)
        && let Some(personality_message) = reconstruction_turn
            .model_info
            .model_messages
            .as_ref()
            .and_then(|m| m.get_personality_message(Some(p)).filter(|s| !s.is_empty()))
    {
        let msg = crate::context::ContextualUserFragment::into(
            crate::context::PersonalitySpecInstructions::new(personality_message),
        );
        let insert_at = initial_context
            .iter()
            .position(|m| matches!(m, ResponseItem::Message { role, .. } if role == "developer"))
            .map(|i| i + 1)
            .unwrap_or(0);
        initial_context.insert(insert_at, msg);
    }
    for item in &initial_context {
        rollout_items.push(RolloutItem::ResponseItem(item.clone()));
    }
    live_history.record_items(
        initial_context.iter(),
        reconstruction_turn.truncation_policy,
    );

    let user1 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "first user".to_string(),
        }],
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user1),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user1.clone()));

    let assistant1 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply one".to_string(),
        }],
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant1),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant1.clone()));

    let summary1 = "summary one";
    let snapshot1 = live_history
        .clone()
        .for_prompt(&reconstruction_turn.model_info.input_modalities);
    let user_messages1 = collect_user_messages(&snapshot1);
    let rebuilt1 = compact::build_compacted_history(Vec::new(), &user_messages1, summary1);
    live_history.replace(rebuilt1);
    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: summary1.to_string(),
        replacement_history: None,
    }));

    let user2 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "second user".to_string(),
        }],
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user2),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user2.clone()));

    let assistant2 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply two".to_string(),
        }],
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant2),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant2.clone()));

    let summary2 = "summary two";
    let snapshot2 = live_history
        .clone()
        .for_prompt(&reconstruction_turn.model_info.input_modalities);
    let user_messages2 = collect_user_messages(&snapshot2);
    let rebuilt2 = compact::build_compacted_history(Vec::new(), &user_messages2, summary2);
    live_history.replace(rebuilt2);
    rollout_items.push(RolloutItem::Compacted(CompactedItem {
        message: summary2.to_string(),
        replacement_history: None,
    }));

    let user3 = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "third user".to_string(),
        }],
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&user3),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(user3));

    let assistant3 = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant reply three".to_string(),
        }],
        phase: None,
    };
    live_history.record_items(
        std::iter::once(&assistant3),
        reconstruction_turn.truncation_policy,
    );
    rollout_items.push(RolloutItem::ResponseItem(assistant3));

    (
        rollout_items,
        live_history.for_prompt(&reconstruction_turn.model_info.input_modalities),
    )
}

#[tokio::test]
async fn create_goal_tool_rejects_existing_goal() {
    let (session, turn_context, rx, _codex_home) = make_goal_session_and_context_with_rx().await;
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let handler = CreateGoalHandler;

    handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            cancellation_token: CancellationToken::new(),
            tracker: Arc::clone(&tracker),
            call_id: "create-goal-1".to_string(),
            tool_name: codex_tools::ToolName::plain("create_goal"),
            source: ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "objective": "Keep the watcher alive",
                    "token_budget": 123,
                })
                .to_string(),
            },
        })
        .await
        .expect("initial create_goal should succeed");

    let response = handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            cancellation_token: CancellationToken::new(),
            tracker,
            call_id: "create-goal-2".to_string(),
            tool_name: codex_tools::ToolName::plain("create_goal"),
            source: ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "objective": "Replace the watcher",
                    "token_budget": 456,
                })
                .to_string(),
            },
        })
        .await;

    let Err(FunctionCallError::RespondToModel(output)) = response else {
        panic!("expected create_goal to reject an existing goal");
    };
    assert_eq!(
        output,
        "cannot create a new goal because this thread already has a goal; use update_goal only when the existing goal is complete"
    );

    let goal = session
        .get_thread_goal()
        .await
        .expect("read thread goal")
        .expect("goal should still exist");
    assert_eq!(goal.objective, "Keep the watcher alive");
    assert_eq!(goal.token_budget, Some(123));
}

#[tokio::test]
async fn update_goal_tool_rejects_pausing_goal() {
    let (session, turn_context, rx, _codex_home) = make_goal_session_and_context_with_rx().await;
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let create_handler = CreateGoalHandler;
    let update_handler = UpdateGoalHandler;

    create_handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            cancellation_token: CancellationToken::new(),
            tracker: Arc::clone(&tracker),
            call_id: "create-goal".to_string(),
            tool_name: codex_tools::ToolName::plain("create_goal"),
            source: ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "objective": "Keep the watcher alive",
                    "token_budget": 123,
                })
                .to_string(),
            },
        })
        .await
        .expect("initial create_goal should succeed");

    let response = update_handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            cancellation_token: CancellationToken::new(),
            tracker,
            call_id: "pause-goal".to_string(),
            tool_name: codex_tools::ToolName::plain("update_goal"),
            source: ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "status": "paused",
                })
                .to_string(),
            },
        })
        .await;

    let Err(FunctionCallError::RespondToModel(output)) = response else {
        panic!("expected update_goal to reject pausing a goal");
    };
    assert_eq!(
        output,
        "update_goal can only mark the existing goal complete; pause, resume, and budget-limited status changes are controlled by the user or system"
    );

    let goal = session
        .get_thread_goal()
        .await
        .expect("read thread goal")
        .expect("goal should still exist");
    assert_eq!(goal.status, ThreadGoalStatus::Active);
}

#[tokio::test]
async fn update_goal_tool_marks_goal_complete() {
    let (session, turn_context, rx, _codex_home) = make_goal_session_and_context_with_rx().await;
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));
    let create_handler = CreateGoalHandler;
    let update_handler = UpdateGoalHandler;

    create_handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            cancellation_token: CancellationToken::new(),
            tracker: Arc::clone(&tracker),
            call_id: "create-goal".to_string(),
            tool_name: codex_tools::ToolName::plain("create_goal"),
            source: ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "objective": "Keep the watcher alive",
                    "token_budget": 123,
                })
                .to_string(),
            },
        })
        .await
        .expect("initial create_goal should succeed");

    update_handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            cancellation_token: CancellationToken::new(),
            tracker,
            call_id: "complete-goal".to_string(),
            tool_name: codex_tools::ToolName::plain("update_goal"),
            source: ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "status": "complete",
                })
                .to_string(),
            },
        })
        .await
        .expect("update_goal should mark the goal complete");

    let goal = session
        .get_thread_goal()
        .await
        .expect("read thread goal")
        .expect("goal should still exist");
    assert_eq!(goal.status, ThreadGoalStatus::Complete);
}

#[tokio::test]
async fn rejects_escalated_permissions_when_policy_not_on_request() {
    use crate::exec_policy::ExecApprovalRequest;
    use crate::sandboxing::SandboxPermissions;
    use crate::tools::sandboxing::ExecApprovalRequirement;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_protocol::protocol::AskForApproval;
    use codex_tools::ShellCommandBackendConfig;

    let (session, mut turn_context_raw) = make_session_and_context().await;
    // Ensure policy is NOT OnRequest so the early rejection path triggers
    turn_context_raw
        .approval_policy
        .set(AskForApproval::OnFailure)
        .expect("test setup should allow updating approval policy");
    let session = Arc::new(session);
    let mut turn_context = Arc::new(turn_context_raw);

    let command_script = "echo hi";
    let timeout_ms = 1000;
    let sandbox_permissions = SandboxPermissions::RequireEscalated;

    let turn_diff_tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

    let tool_name = "shell_command";
    let call_id = "test-call".to_string();

    let handler = ShellCommandHandler::from(ShellCommandBackendConfig::Classic);
    #[allow(deprecated)]
    let workdir = Some(turn_context.cwd.to_string_lossy().to_string());
    let resp = handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            cancellation_token: CancellationToken::new(),
            tracker: Arc::clone(&turn_diff_tracker),
            call_id,
            tool_name: codex_tools::ToolName::plain(tool_name),
            source: crate::tools::context::ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "command": command_script,
                    "workdir": workdir,
                    "timeout_ms": timeout_ms,
                    "sandbox_permissions": sandbox_permissions,
                    "justification": Some("test"),
                })
                .to_string(),
            },
        })
        .await;

    let Err(FunctionCallError::RespondToModel(output)) = resp else {
        panic!("expected error result");
    };

    let expected = format!(
        "approval policy is {policy:?}; reject command — you should not ask for escalated permissions if the approval policy is {policy:?}",
        policy = turn_context.approval_policy.value()
    );

    pretty_assertions::assert_eq!(output, expected);
    pretty_assertions::assert_eq!(session.granted_turn_permissions().await, None);

    // The rejection should not poison the non-escalated path for the same
    // command. Force DangerFullAccess so this check stays focused on approval
    // policy rather than platform-specific sandbox behavior.
    let turn_context_mut = Arc::get_mut(&mut turn_context).expect("unique turn context Arc");
    turn_context_mut.permission_profile = PermissionProfile::Disabled;

    let file_system_sandbox_policy = turn_context.file_system_sandbox_policy();
    let command = session
        .user_shell()
        .derive_exec_args(command_script, turn_context.tools_config.allow_login_shell);
    let exec_approval_requirement = session
        .services
        .exec_policy
        .create_exec_approval_requirement_for_command(ExecApprovalRequest {
            command: &command,
            approval_policy: turn_context.approval_policy.value(),
            permission_profile: turn_context.permission_profile(),
            file_system_sandbox_policy: &file_system_sandbox_policy,
            #[allow(deprecated)]
            sandbox_cwd: turn_context.cwd.as_path(),
            sandbox_permissions: SandboxPermissions::UseDefault,
            prefix_rule: None,
        })
        .await;
    assert!(matches!(
        exec_approval_requirement,
        ExecApprovalRequirement::Skip { .. }
    ));
}
#[tokio::test]
async fn unified_exec_rejects_escalated_permissions_when_policy_not_on_request() {
    use crate::sandboxing::SandboxPermissions;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_protocol::protocol::AskForApproval;

    let (session, mut turn_context_raw) = make_session_and_context().await;
    turn_context_raw
        .approval_policy
        .set(AskForApproval::OnFailure)
        .expect("test setup should allow updating approval policy");
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context_raw);
    let tracker = Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new()));

    let handler = ExecCommandHandler::default();
    let resp = handler
        .handle(ToolInvocation {
            session: Arc::clone(&session),
            turn: Arc::clone(&turn_context),
            cancellation_token: CancellationToken::new(),
            tracker: Arc::clone(&tracker),
            call_id: "exec-call".to_string(),
            tool_name: codex_tools::ToolName::plain("exec_command"),
            source: crate::tools::context::ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: serde_json::json!({
                    "cmd": "echo hi",
                    "sandbox_permissions": SandboxPermissions::RequireEscalated,
                    "justification": "need unsandboxed execution",
                })
                .to_string(),
            },
        })
        .await;

    let Err(FunctionCallError::RespondToModel(output)) = resp else {
        panic!("expected error result");
    };

    let expected = format!(
        "approval policy is {policy:?}; reject command — you cannot ask for escalated permissions if the approval policy is {policy:?}",
        policy = turn_context.approval_policy.value()
    );

    pretty_assertions::assert_eq!(output, expected);
}

#[tokio::test]
async fn session_start_hooks_only_load_from_trusted_project_layers() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let codex_home = temp.path().join("home");
    let project_root = temp.path().join("project");
    let nested = project_root.join("nested");
    let root_dot_codex = project_root.join(".codex");
    let nested_dot_codex = nested.join(".codex");

    std::fs::create_dir_all(&codex_home)?;
    std::fs::create_dir_all(&nested_dot_codex)?;
    std::fs::write(project_root.join(".git"), "gitdir: here")?;
    write_project_hooks(&root_dot_codex)?;
    write_project_hooks(&nested_dot_codex)?;
    write_project_trust_config(&codex_home, &[(&nested, TrustLevel::Trusted)]).await?;

    let config = ConfigBuilder::default()
        .codex_home(codex_home)
        .fallback_cwd(Some(nested))
        .build()
        .await?;

    let hook_list = codex_hooks::list_hooks(codex_hooks::HooksConfig {
        feature_enabled: true,
        config_layer_stack: Some(config.config_layer_stack.clone()),
        ..codex_hooks::HooksConfig::default()
    });
    let expected_source_path = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        nested_dot_codex.join("hooks.json"),
    )?;
    assert_eq!(
        hook_list
            .hooks
            .iter()
            .map(|hook| &hook.source_path)
            .collect::<Vec<_>>(),
        vec![&expected_source_path],
    );
    assert_eq!(
        hook_list.hooks[0].trust_status,
        codex_protocol::protocol::HookTrustStatus::Untrusted
    );
    assert!(preview_session_start_hooks(&config).await?.is_empty());

    Ok(())
}

#[tokio::test]
async fn session_start_hooks_require_project_trust_without_config_toml() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let project_root = temp.path().join("project");
    let nested = project_root.join("nested");
    let dot_codex = project_root.join(".codex");
    std::fs::create_dir_all(&nested)?;
    std::fs::write(project_root.join(".git"), "gitdir: here")?;
    write_project_hooks(&dot_codex)?;

    let cases = [
        ("unknown", Vec::<(&Path, TrustLevel)>::new(), 0_usize),
        (
            "untrusted",
            vec![(&project_root as &Path, TrustLevel::Untrusted)],
            0_usize,
        ),
        (
            "trusted",
            vec![(&project_root as &Path, TrustLevel::Trusted)],
            1_usize,
        ),
    ];

    for (name, trust_entries, expected_hooks) in cases {
        let codex_home = temp.path().join(format!("home_{name}"));
        std::fs::create_dir_all(&codex_home)?;
        write_project_trust_config(&codex_home, &trust_entries).await?;

        let config = ConfigBuilder::default()
            .codex_home(codex_home)
            .fallback_cwd(Some(nested.clone()))
            .build()
            .await?;

        let hook_list = codex_hooks::list_hooks(codex_hooks::HooksConfig {
            feature_enabled: true,
            config_layer_stack: Some(config.config_layer_stack.clone()),
            ..codex_hooks::HooksConfig::default()
        });
        assert_eq!(
            hook_list.hooks.len(),
            expected_hooks,
            "unexpected discovered hook count for {name}",
        );
        assert!(preview_session_start_hooks(&config).await?.is_empty());
        if expected_hooks == 1 {
            assert_eq!(
                hook_list.hooks[0].trust_status,
                codex_protocol::protocol::HookTrustStatus::Untrusted
            );
        }
    }

    Ok(())
}

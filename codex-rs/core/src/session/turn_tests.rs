use super::*;
use crate::session::spine_bridge::DeferredSpineToolCall;
use crate::session::spine_bridge::DeferredSpineToolGroup;
use crate::session::spine_bridge::InFlightSpineToolOutputPlan;
use crate::spine::SPINE_NAMESPACE;
use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use crate::tools::context::ToolPayload;
use crate::tools::router::ToolCall;
use codex_extension_api::ExtensionData;
use codex_extension_api::TurnItemContributionFuture;
use codex_extension_api::TurnItemContributor;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_tools::ToolName;
use pretty_assertions::assert_eq;
use std::sync::Arc;

struct RewriteAgentMessageContributor;

impl TurnItemContributor for RewriteAgentMessageContributor {
    fn contribute<'a>(
        &'a self,
        _thread_store: &'a ExtensionData,
        _turn_store: &'a ExtensionData,
        item: &'a mut TurnItem,
    ) -> TurnItemContributionFuture<'a> {
        Box::pin(async move {
            if let TurnItem::AgentMessage(agent_message) = item {
                agent_message.content = vec![AgentMessageContent::Text {
                    text: "plan contributed assistant text".to_string(),
                }];
            }
            Ok(())
        })
    }
}

fn assistant_output_text(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some("msg-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

#[test]
fn conflicting_spine_control_rejection_uses_retryable_marker() {
    let call = ToolCall {
        tool_name: ToolName::namespaced(SPINE_NAMESPACE, SPINE_TOOL_OPEN),
        call_id: "open-conflict".to_string(),
        payload: ToolPayload::Function {
            arguments: r#"{"summary":"child"}"#.to_string(),
        },
    };

    let output = Session::conflicting_spine_control_rejection_output(
        &call,
        "multiple Spine control tool requests in one assistant message",
    );

    let ResponseItem::FunctionCallOutput { call_id, output } = output else {
        panic!("expected function output");
    };
    assert_eq!(call_id, "open-conflict");
    assert_eq!(output.success, Some(false));
    let FunctionCallOutputBody::Text(text) = output.body else {
        panic!("expected text output");
    };
    assert!(text.starts_with("SPINE_TOOL_USE_FAILED:"));
    assert!(text.contains("multiple Spine control tool requests"));
    assert!(text.contains("No Spine control action was applied"));
    assert!(text.contains("Retry with valid Spine tool arguments"));
}

fn deferred_function_call(
    namespace: Option<&str>,
    name: &str,
    call_id: &str,
) -> DeferredSpineToolCall {
    let arguments = match (namespace, name) {
        (Some(SPINE_NAMESPACE), SPINE_TOOL_OPEN) => r#"{"summary":"test spine open"}"#,
        _ => "{}",
    };
    DeferredSpineToolCall {
        call: ToolCall {
            tool_name: ToolName::new(namespace.map(str::to_string), name.to_string()),
            call_id: call_id.to_string(),
            payload: ToolPayload::Function {
                arguments: arguments.to_string(),
            },
        },
        in_flight: None,
    }
}

#[test]
fn deferred_spine_group_classifies_conflicting_controls_atomically() {
    let mut ordinary = vec![
        deferred_function_call(None, "shell_command", "shell-1"),
        deferred_function_call(Some(SPINE_NAMESPACE), SPINE_TOOL_TREE, "tree-1"),
    ];
    let normal = Session::take_deferred_spine_tool_group(&mut ordinary).expect("normal group");
    assert!(ordinary.is_empty());
    assert!(matches!(normal, DeferredSpineToolGroup::Normal(group) if group.len() == 2));

    let mut conflicting = vec![
        deferred_function_call(Some(SPINE_NAMESPACE), SPINE_TOOL_OPEN, "open-1"),
        deferred_function_call(Some(SPINE_NAMESPACE), SPINE_TOOL_CLOSE, "close-1"),
    ];
    let conflict =
        Session::take_deferred_spine_tool_group(&mut conflicting).expect("conflicting group");
    assert!(conflicting.is_empty());
    let DeferredSpineToolGroup::ConflictingControls { group, message } = conflict else {
        panic!("expected conflicting controls");
    };
    assert_eq!(group.len(), 2);
    assert!(message.contains("open-1"));
    assert!(message.contains("close-1"));
    assert!(message.contains("No Spine control action was applied"));
}

#[test]
fn deferred_spine_group_commit_prefers_parser_control_call() {
    let group = vec![
        deferred_function_call(None, "shell_command", "shell-1"),
        deferred_function_call(Some(SPINE_NAMESPACE), SPINE_TOOL_OPEN, "open-1"),
        deferred_function_call(None, "list_mcp_resources", "mcp-1"),
    ];

    let commit = match Session::deferred_spine_tool_group_commit(&group) {
        Ok(commit) => commit,
        Err(err) => panic!("group commit: {err}"),
    };

    assert_eq!(commit.commit_call_id, "open-1");
    assert_eq!(
        commit.tool_call_ids,
        vec![
            "shell-1".to_string(),
            "open-1".to_string(),
            "mcp-1".to_string()
        ]
    );
}

#[test]
fn deferred_conflicting_control_commit_prepares_rejection_slots() {
    let group = vec![
        deferred_function_call(Some(SPINE_NAMESPACE), SPINE_TOOL_OPEN, "open-1"),
        deferred_function_call(None, "shell_command", "shell-1"),
        deferred_function_call(Some(SPINE_NAMESPACE), SPINE_TOOL_CLOSE, "close-1"),
    ];
    let mut commit = match Session::deferred_spine_conflicting_control_commit(
        &group,
        "multiple Spine control tool requests in one assistant message",
    ) {
        Ok(commit) => commit,
        Err(err) => panic!("conflicting commit: {err}"),
    };

    assert!(commit.has_prepared_response_slot(0));
    assert!(!commit.has_prepared_response_slot(1));
    assert!(commit.has_prepared_response_slot(2));
    commit
        .fill_response_slot(
            1,
            ResponseItem::FunctionCallOutput {
                call_id: "shell-1".to_string(),
                output: FunctionCallOutputPayload::from_text("shell output".to_string()),
            },
        )
        .unwrap_or_else(|err| panic!("fill response slot: {err}"));

    let (commit_call_id, tool_call_ids, control_call_ids, response_items) = commit
        .into_parts()
        .unwrap_or_else(|err| panic!("commit parts: {err}"));

    assert_eq!(commit_call_id, "open-1");
    assert_eq!(
        tool_call_ids,
        vec![
            "open-1".to_string(),
            "shell-1".to_string(),
            "close-1".to_string()
        ]
    );
    assert_eq!(
        control_call_ids,
        vec!["open-1".to_string(), "close-1".to_string()]
    );
    assert!(matches!(
        &response_items[0],
        ResponseItem::FunctionCallOutput { call_id, output }
            if call_id == "open-1" && output.success == Some(false)
    ));
    assert!(matches!(
        &response_items[1],
        ResponseItem::FunctionCallOutput { call_id, .. } if call_id == "shell-1"
    ));
    assert!(matches!(
        &response_items[2],
        ResponseItem::FunctionCallOutput { call_id, output }
            if call_id == "close-1" && output.success == Some(false)
    ));
}

#[test]
fn deferred_spine_tool_request_plan_splits_control_from_native_tools() {
    let control = deferred_function_call(Some(SPINE_NAMESPACE), SPINE_TOOL_OPEN, "open-1");
    let ordinary = deferred_function_call(None, "shell_command", "shell-1");

    let control_plan = Session::deferred_spine_tool_request_plan(&control.call);
    assert!(control_plan.records_control_overlay);
    assert!(!control_plan.starts_native_tool);

    let ordinary_plan = Session::deferred_spine_tool_request_plan(&ordinary.call);
    assert!(!ordinary_plan.records_control_overlay);
    assert!(ordinary_plan.starts_native_tool);
}

#[test]
fn deferred_spine_tool_call_extraction_obeys_feature_gate() {
    let non_tool = ResponseItem::Message {
        id: Some("msg-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "not a tool call".to_string(),
        }],
        phase: None,
    };
    let item = ResponseItem::FunctionCall {
        id: Some("call-item".to_string()),
        name: SPINE_TOOL_OPEN.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: r#"{"summary":"child"}"#.to_string(),
        call_id: "open-1".to_string(),
    };

    let disabled = Session::deferred_spine_tool_call_for_enabled(false, non_tool)
        .expect("disabled extraction should not parse");
    assert!(disabled.is_none());

    let enabled = Session::deferred_spine_tool_call_for_enabled(true, item)
        .expect("enabled extraction should parse function call")
        .expect("tool call");
    assert_eq!(enabled.call_id, "open-1");
    assert_eq!(
        enabled.tool_name.namespace.as_deref(),
        Some(SPINE_NAMESPACE)
    );
    assert_eq!(enabled.tool_name.name, SPINE_TOOL_OPEN);
}

#[test]
fn deferred_spine_drain_gate_obeys_feature_and_queue_state() {
    let deferred = vec![deferred_function_call(None, "shell_command", "shell-1")];

    assert!(
        !Session::should_drain_pending_deferred_spine_tool_calls_for_enabled(
            false, &deferred, false
        )
    );
    assert!(!Session::should_drain_pending_deferred_spine_tool_calls_for_enabled(true, &[], false));
    assert!(
        !Session::should_drain_pending_deferred_spine_tool_calls_for_enabled(true, &deferred, true)
    );
    assert!(
        Session::should_drain_pending_deferred_spine_tool_calls_for_enabled(true, &deferred, false)
    );
}

#[test]
fn in_flight_spine_tool_output_plan_obeys_feature_policy() {
    assert_eq!(
        Session::in_flight_spine_tool_output_plan_for_enabled(false, false, false),
        InFlightSpineToolOutputPlan::RecordOrdinaryToolOutput {
            apply_trim_projection: false
        }
    );
    assert_eq!(
        Session::in_flight_spine_tool_output_plan_for_enabled(false, true, false),
        InFlightSpineToolOutputPlan::RecordOrdinaryToolOutput {
            apply_trim_projection: true
        }
    );
    assert_eq!(
        Session::in_flight_spine_tool_output_plan_for_enabled(false, true, true),
        InFlightSpineToolOutputPlan::RecordControlOverlayOnly
    );
    assert_eq!(
        Session::in_flight_spine_tool_output_plan_for_enabled(true, false, true),
        InFlightSpineToolOutputPlan::RecordSpineToolOutput
    );
}

#[test]
fn spine_control_overlay_request_item_keeps_only_spine_controls() {
    let control = ResponseItem::FunctionCall {
        id: Some("call-item".to_string()),
        name: SPINE_TOOL_OPEN.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: r#"{"summary":"child"}"#.to_string(),
        call_id: "open-1".to_string(),
    };
    let ordinary = ResponseItem::FunctionCall {
        id: Some("ordinary-item".to_string()),
        name: "shell_command".to_string(),
        namespace: None,
        arguments: "{}".to_string(),
        call_id: "shell-1".to_string(),
    };

    assert_eq!(
        Session::spine_control_overlay_request_item(&control),
        Some(control)
    );
    assert_eq!(Session::spine_control_overlay_request_item(&ordinary), None);
}

#[test]
fn spine_control_overlay_disabled_drops_carriers() {
    let mut overlay = SpineControlOverlay::new(false);
    let request = ResponseItem::FunctionCall {
        id: Some("call-item".to_string()),
        name: SPINE_TOOL_TREE.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: "{}".to_string(),
        call_id: "call-spine-tree".to_string(),
    };
    let output = ResponseItem::FunctionCallOutput {
        call_id: "call-spine-tree".to_string(),
        output: FunctionCallOutputPayload::from_text("tree output".to_string()),
    };

    overlay.push_request(request);
    overlay.push_output_if_matching(&output);

    assert_eq!(overlay.take_for_next_prompt(), Vec::<ResponseItem>::new());
}

#[test]
fn spine_control_overlay_factory_applies_feature_gate() {
    let request = ResponseItem::FunctionCall {
        id: Some("call-item".to_string()),
        name: SPINE_TOOL_TREE.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: "{}".to_string(),
        call_id: "call-spine-tree".to_string(),
    };

    let mut disabled = Session::spine_control_overlay_for_enabled(false);
    disabled.push_request(request.clone());
    assert_eq!(disabled.take_for_next_prompt(), Vec::<ResponseItem>::new());

    let mut enabled = Session::spine_control_overlay_for_enabled(true);
    enabled.push_request(request.clone());
    assert_eq!(enabled.take_for_next_prompt(), vec![request]);
}

#[test]
fn spine_control_overlay_detects_matching_output_before_push() {
    let mut overlay = SpineControlOverlay::new(true);
    let request = ResponseItem::FunctionCall {
        id: Some("call-item".to_string()),
        name: SPINE_TOOL_TREE.to_string(),
        namespace: Some(SPINE_NAMESPACE.to_string()),
        arguments: "{}".to_string(),
        call_id: "call-spine-tree".to_string(),
    };
    let matching = ResponseItem::FunctionCallOutput {
        call_id: "call-spine-tree".to_string(),
        output: FunctionCallOutputPayload::from_text("tree output".to_string()),
    };
    let unrelated = ResponseItem::FunctionCallOutput {
        call_id: "other-call".to_string(),
        output: FunctionCallOutputPayload::from_text("other output".to_string()),
    };

    overlay.push_request(request.clone());

    assert!(overlay.contains_matching_request(&matching));
    assert!(!overlay.contains_matching_request(&unrelated));
    overlay.push_output_if_matching(&matching);
    assert_eq!(overlay.take_for_next_prompt(), vec![request, matching]);
}

#[test]
fn spine_jit_deferred_tool_requests_close_before_later_non_tool_items() {
    let turn = include_str!("turn.rs");
    let function_call_branch = turn
        .split("record_deferred_tool_call(ctx.sess.as_ref(), ctx.turn_context.as_ref(), &item)")
        .nth(1)
        .and_then(|tail| tail.split("let output_result =").next())
        .expect("SpineJit function_call branch before normal item handling");

    assert!(
        function_call_branch.contains("!Session::is_spine_parser_control_tool_call(&call)")
            && function_call_branch.contains("spawn_tool_call("),
        "ordinary Spine JIT tool requests should start native in-flight execution before grouped commit"
    );
    assert!(
        function_call_branch
            .contains("deferred_tool_calls.push(DeferredToolCall { call, in_flight });")
            && function_call_branch.contains("continue;"),
        "Spine JIT tool requests should still be recorded into the deferred grouped commit collector"
    );
    let output_item_done = turn
        .split("ResponseEvent::OutputItemDone(item) =>")
        .nth(1)
        .and_then(|tail| tail.split("let output_result =").next())
        .expect("OutputItemDone section before normal item handling");
    let in_loop_drain_block = output_item_done
        .split("drain_pending_deferred_spine_tool_calls(")
        .nth(1)
        .and_then(|tail| {
            tail.split("if let Some(state) = plan_mode_state.as_mut()")
                .next()
        })
        .expect("in-loop deferred drain block before plan-mode handling");
    assert!(
        in_loop_drain_block.contains("Err(err) => break Err(err)")
            && !in_loop_drain_block.contains(".await?"),
        "Spine JIT in-loop deferred drain failures must return through the stream outcome path, not bypass unified cleanup with ?"
    );

    assert!(
        output_item_done.contains("deferred_tool_call.is_none()")
            && output_item_done.contains("drain_pending_deferred_spine_tool_calls("),
        "Spine JIT must close durable deferred tool requests before any later non-tool item is recorded or handled"
    );

    let turn_end = turn
        .split("if deferred_spine_tool_group.is_none()")
        .nth(1)
        .and_then(|tail| tail.split("drain_in_flight(").next())
        .expect("SpineJit end-of-turn deferred drain fallback");
    assert!(
        turn_end.contains("Session::take_deferred_spine_tool_group(&mut deferred_tool_calls)")
            && turn_end.contains("drain_deferred_spine_tool_group_kind("),
        "Spine JIT must also close durable deferred tool requests on stream exit before cancellation or return"
    );

    let after_stream = turn
        .split("let outcome: Result<SamplingRequestResult, SamplingRequestError> = loop")
        .nth(1)
        .expect("post-stream cleanup section");
    let deferred_drain_pos = after_stream
        .find("drain_deferred_spine_tool_group_kind(")
        .expect("deferred Spine group is drained after stream exits");
    let cancellation_check_pos = after_stream
        .find("if cancellation_token.is_cancelled()")
        .expect("turn cancellation check exists");
    assert!(
        deferred_drain_pos < cancellation_check_pos,
        "durable Spine tool requests must be closed before TurnAborted/cancellation return"
    );

    let output_item_done = turn
        .split("ResponseEvent::OutputItemDone(item) =>")
        .nth(1)
        .and_then(|tail| tail.split("let mut ctx = HandleOutputCtx").next())
        .expect("OutputItemDone pre-recording section");
    let plan_mode_pos = output_item_done
        .find("handle_assistant_item_done_in_plan_mode(")
        .expect("plan-mode assistant handler is present");
    let pre_plan_drain_pos = output_item_done
        .find("drain_pending_deferred_spine_tool_calls(")
        .expect("pre-plan-mode deferred drain is present");
    assert!(
        pre_plan_drain_pos < plan_mode_pos,
        "plan-mode assistant-message handling must not bypass durable Spine tool request closure"
    );
}

#[tokio::test]
async fn plan_mode_uses_contributed_turn_item_for_last_agent_message() {
    let (mut session, turn_context) = crate::session::tests::make_session_and_context().await;
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::new();
    builder.turn_item_contributor(Arc::new(RewriteAgentMessageContributor));
    session.services.extensions = Arc::new(builder.build());
    let turn_store = ExtensionData::new(turn_context.sub_id.clone());
    let mut state = PlanModeStreamState::new(&turn_context.sub_id);
    let mut last_agent_message = None;
    let item = assistant_output_text("original assistant text");

    let handled = handle_assistant_item_done_in_plan_mode(
        &session,
        &turn_context,
        &turn_store,
        &item,
        &mut state,
        /*previously_active_item*/ None,
        &mut last_agent_message,
    )
    .await
    .expect("plan-mode assistant item should record");

    assert!(handled);
    assert_eq!(
        last_agent_message.as_deref(),
        Some("plan contributed assistant text")
    );
}

use super::*;

fn developer_fixed_prefix_item(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn custom_tool_call(call_id: &str) -> ResponseItem {
    ResponseItem::CustomToolCall {
        id: None,
        status: None,
        call_id: call_id.to_string(),
        name: "custom_tool".to_string(),
        input: "input".to_string(),
    }
}

fn observe_full_host_item(
    runtime: &mut SpineRuntime,
    raw: &mut Vec<Option<ResponseItem>>,
    host_history: &[ResponseItem],
    full_host_index: usize,
) -> usize {
    let item = host_history
        .get(full_host_index)
        .expect("full host index must exist");
    let raw_ordinal = u64::try_from(raw.len()).expect("raw ordinal fits u64");
    let mutable_index =
        spine_mutable_context_index_for_full_history_index(host_history, full_host_index)
            .expect("full host item must map to mutable context index");
    raw.push(Some(item.clone()));
    runtime.observe_raw_items(1).expect("record raw item");
    runtime
        .observe_context_item(raw_ordinal, mutable_index, item)
        .expect("observe mutable context item");
    mutable_index
}

#[test]
fn observe_fixed_prefix_msg_uses_mutable_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();
    let host_history = vec![
        developer_fixed_prefix_item("fixed developer prefix"),
        text_item("mutable user item"),
    ];

    let context_index = observe_full_host_item(&mut runtime, &mut raw, &host_history, 1);

    assert_eq!(
        context_index, 0,
        "host append full index 1 must become parser-visible mutable index 0"
    );
    assert_eq!(
        materialized_trace_signature(&runtime, &raw),
        vec!["user:mutable user item"],
        "fixed-prefix host items must not enter h(PS)"
    );
}

#[test]
fn observe_fixed_prefix_function_toolcall_uses_mutable_indices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();
    let host_history = vec![
        developer_fixed_prefix_item("fixed developer prefix"),
        ordinary_call("shell", "function-call"),
        function_output("function-call"),
    ];

    let request_context = observe_full_host_item(&mut runtime, &mut raw, &host_history, 1);
    let response_context = observe_full_host_item(&mut runtime, &mut raw, &host_history, 2);
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall(
                "function-call",
                vec![tool_req(0, request_context), tool_resp(1, response_context)],
            ),
            &raw,
        )
        .expect("observe completed function toolcall");

    assert_eq!((request_context, response_context), (0, 1));
    assert_eq!(
        materialized_trace_signature(&runtime, &raw),
        vec![
            "tool-call:shell:function-call",
            "tool-output:function-call:ok",
        ],
        "completed function toolcall anchors must stay in mutable index space"
    );
}

#[test]
fn observe_fixed_prefix_custom_toolcall_uses_mutable_indices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();
    let host_history = vec![
        developer_fixed_prefix_item("fixed developer prefix"),
        custom_tool_call("custom-call"),
        custom_tool_output_text("custom-call", "custom ok"),
    ];

    let request_context = observe_full_host_item(&mut runtime, &mut raw, &host_history, 1);
    let response_context = observe_full_host_item(&mut runtime, &mut raw, &host_history, 2);
    runtime
        .observe_completed_toolcall_with_raw_items(
            completed_toolcall(
                "custom-call",
                vec![tool_req(0, request_context), tool_resp(1, response_context)],
            ),
            &raw,
        )
        .expect("observe completed custom toolcall");

    assert_eq!((request_context, response_context), (0, 1));
    assert_eq!(
        runtime
            .materialize_variable_context_for_test(&raw)
            .expect("materialize h(PS)"),
        vec![host_history[1].clone(), host_history[2].clone()],
        "completed custom toolcall anchors must stay in mutable index space"
    );
}

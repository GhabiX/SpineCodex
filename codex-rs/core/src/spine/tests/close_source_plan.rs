use super::*;

#[test]
fn close_source_plan_uses_current_hps_projection_indices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");
    let mut raw = Vec::new();

    for index in 0..5 {
        append_msg_with_context_index(&mut runtime, &mut raw, &format!("prefix {index}"), index);
    }
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_OPEN, "open-gap");
    runtime
        .stage_open("open-gap".to_string(), "gap task".to_string())
        .expect("stage open");
    observe_function_output(&mut runtime, &mut raw, "open-gap");
    runtime
        .maybe_commit_output("open-gap", None)
        .expect("commit open");

    append_msg(&mut runtime, &mut raw, "first live item");
    append_msg(&mut runtime, &mut raw, "second live item");
    observe_spine_request(&mut runtime, &mut raw, SPINE_TOOL_CLOSE, "close-gap");
    runtime
        .stage_close("close-gap".to_string(), "test node memory".to_string())
        .expect("stage close");

    let host_history = runtime
        .materialize_history(&raw)
        .expect("materialize current h(PS)");

    let source_plan = pending_close_source_plan(&runtime, &host_history, "close-gap", "1.1.1");
    let contexts = source_plan
        .entries
        .iter()
        .map(|entry| entry.context_index)
        .collect::<Vec<_>>();
    assert_eq!(contexts, vec![5, 6, 7, 8]);
    assert_eq!(source_plan.source_context_range, 5..9);
    assert_eq!(source_plan.source_raw_range, 5..9);
    let user_evidence = source_plan
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            SpineCompactSourceEntryKind::RawResponseItem {
                item,
                from_user: true,
                user_anchor,
                ..
            } => Some((item, user_anchor)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(user_evidence.len(), 2);
    assert_eq!(user_evidence[0].1, &Some(6));
    assert!(matches!(
        user_evidence[0].0,
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }] if text == "[U6]\nfirst live item"
            )
    ));
    assert_eq!(user_evidence[1].1, &Some(7));
    assert!(matches!(
        user_evidence[1].0,
        ResponseItem::Message { content, .. }
            if matches!(
                content.as_slice(),
                [ContentItem::InputText { text }] if text == "[U7]\nsecond live item"
            )
    ));
}

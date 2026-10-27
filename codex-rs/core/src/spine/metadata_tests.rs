//! Native provider metadata belongs to the source item, not the reducer's text view.
use super::*;
use codex_history::CompactedItem;
use codex_history::ResponseItemEnvelope;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn legacy_items() -> Vec<Value> {
    vec![
        json!({
            "type": "message", "role": "assistant",
            "content": [{"type": "output_text", "text": "answer"}]
        }),
        json!({
            "type": "function_call", "name": "shell", "call_id": "function",
            "arguments": "{\"command\":\"pwd\"}"
        }),
        json!({
            "type": "local_shell_call", "call_id": "shell", "status": "completed",
            "action": {
                "type": "exec", "command": ["pwd"], "timeout_ms": null,
                "working_directory": null, "env": null, "user": null
            }
        }),
        json!({
            "type": "tool_search_call", "call_id": "search", "execution": "client",
            "arguments": {"query": "files"}
        }),
        json!({
            "type": "custom_tool_call", "call_id": "custom", "name": "apply_patch",
            "input": "patch"
        }),
        json!({"type": "web_search_call"}),
        json!({"type": "image_generation_call", "status": "completed", "result": "image"}),
    ]
}

fn metadata_items() -> Vec<ResponseItemEnvelope> {
    legacy_items()
        .into_iter()
        .enumerate()
        .map(|(index, mut item)| {
            item["encrypted_content"] = json!(format!("ciphertext-{index}"));
            if index <= 1 {
                item["status"] = json!("completed");
            }
            if index == 0 {
                item["content"][0]["annotations"] = json!([
                    {"type": "url_citation", "url": "https://example.test/source", "start_index": 0}
                ]);
                item["content"][0]["logprobs"] = json!([
                    {"token": "answer", "logprob": -1, "bytes": [97], "provider": {"rank": 1}}
                ]);
                // An explicitly empty array is distinct from an omitted metadata field.
                item["content"].as_array_mut().unwrap().push(json!({
                    "type": "output_text", "text": "", "annotations": [], "logprobs": []
                }));
            }
            let decoded: ResponseItem = serde_json::from_value(item.clone()).unwrap();
            assert_eq!(serde_json::to_value(&decoded).unwrap(), item);
            decoded.into()
        })
        .collect()
}

fn native_rollout(items: &[ResponseItemEnvelope]) -> Vec<RolloutItem> {
    items
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect()
}

fn coordinator() -> coordinator::CodexSpineCoordinator {
    coordinator::CodexSpineCoordinator::new_with_observer(
        "metadata-thread",
        spine_core::host::SpineConfig::v1()
            .with_feature(spine_core::host::Feature::Jit)
            .unwrap(),
        observer::CodexSpineObserverHandler::default(),
    )
    .unwrap()
}

fn finish_sampling(
    live: &mut coordinator::CodexSpineCoordinator,
    attempt: coordinator::SpineSamplingAttempt,
    rollout: &mut Vec<RolloutItem>,
) -> coordinator::InstalledCanonicalCommit {
    let prepared = live.prepare_canonical_sampling(attempt).unwrap();
    rollout.push(prepared.rollout_item());
    live.install_canonical_sampling(prepared).unwrap()
}

fn assert_cold_replay(rollout: &[RolloutItem], expected: &[ResponseItemEnvelope]) {
    let persisted = serde_json::to_vec(rollout).unwrap();
    let reloaded: Vec<RolloutItem> = serde_json::from_slice(&persisted).unwrap();
    let effective = effective_rollout(&reloaded);
    let coordinator::ReplayMode::Canonical { thread, records } =
        coordinator::replay_mode(&effective).unwrap()
    else {
        panic!("expected canonical replay");
    };
    let mut resumed = coordinator();
    // No native-history fallback: the archived sources must reconstruct the entire request.
    let replayed = resumed
        .replay_canonical(&effective, &[], thread, records)
        .unwrap();
    assert_eq!(replayed.context.items, expected);
    let attempt = resumed.begin_sampling().unwrap();
    resumed
        .sampling_started_rollout_item(&attempt, &[])
        .unwrap();
    let continued = resumed.prepare_canonical_sampling(attempt).unwrap();
    assert_eq!(
        resumed
            .install_canonical_sampling(continued)
            .unwrap()
            .context
            .items,
        expected
    );
}

#[test]
fn legacy_metadata_absence_keeps_native_wire_shape() {
    for wire in legacy_items() {
        let native: ResponseItem = serde_json::from_value(wire.clone()).unwrap();
        let rollout = vec![RolloutItem::ResponseItem(native.into())];
        let projected = materialize_context(
            &[ContextItem::Native {
                source: NativeItemRef::Rollout {
                    ordinal: RawBoundary(0),
                },
            }],
            &effective_rollout(&rollout),
            None,
            &BTreeMap::new(),
            "",
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&projected[0]).unwrap(), wire);
    }
}

#[test]
fn native_metadata_survives_text_view_and_source_span_materialization() {
    let native = metadata_items();
    let rollout = native_rollout(&native);
    let effective = effective_rollout(&rollout);
    let projected = materialize_context(
        &[ContextItem::SourceSpan {
            span: spine_core::host::RawSpan {
                start: RawBoundary(0),
                end: RawBoundary((native.len() - 1) as u64),
            },
        }],
        &effective,
        None,
        &BTreeMap::new(),
        "",
    )
    .unwrap();
    assert_eq!(
        projected,
        native
            .iter()
            .map(|item| item.item.clone())
            .collect::<Vec<_>>()
    );
    let message = message_from_response_item(0, &native[0]);
    assert_eq!(message.content, "answer\n");
    let projected_message = materialize_context(
        &[ContextItem::Message {
            message,
            user_anchor: None,
        }],
        &effective,
        None,
        &BTreeMap::new(),
        "",
    )
    .unwrap();
    assert_eq!(projected_message, vec![native[0].item.clone()]);
}

#[test]
fn canonical_metadata_survives_seed_and_post_start_cold_replay() {
    for in_seed in [true, false] {
        let native = metadata_items();
        let mut live = coordinator();
        let mut rollout = Vec::new();
        if in_seed {
            live.observe_response_items(&native).unwrap();
            rollout.extend(native_rollout(&native));
        }
        let attempt = live.begin_sampling().unwrap();
        rollout.push(live.sampling_started_rollout_item(&attempt, &[]).unwrap());
        if !in_seed {
            live.observe_response_items(&native).unwrap();
            rollout.extend(native_rollout(&native));
        }
        let installed = finish_sampling(&mut live, attempt, &mut rollout);
        assert_eq!(installed.context.items, native);
        assert_cold_replay(&rollout, &native);
    }
}

#[test]
fn compact_metadata_survives_seed_and_rollout_replacement_cold_replay() {
    for in_seed in [true, false] {
        let replacement = metadata_items();
        let mut live = coordinator();
        let mut rollout = Vec::new();
        if !in_seed {
            // A successful pre-compaction generation must have a source tail.
            // This branch tests a later compact, not an empty-input generation.
            let input = vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: "before compaction".to_string(),
                    }],
                    encrypted_content: None,
                    status: None,
                    phase: None,
                    internal_chat_message_metadata_passthrough: None,
                }
                .into(),
            ];
            live.observe_response_items(&input).unwrap();
            rollout.extend(native_rollout(&input));
            let attempt = live.begin_sampling().unwrap();
            rollout.push(live.sampling_started_rollout_item(&attempt, &[]).unwrap());
            finish_sampling(&mut live, attempt, &mut rollout);
        }
        let compact = live.prepare_compact(&replacement).unwrap();
        live.install_compact(compact);
        rollout.push(RolloutItem::Compacted(CompactedItem {
            message: "summary".to_string(),
            replacement_history: Some(replacement.clone()),
            retained_context: None,
            resume_metadata: None,
            guardian_history: None,
            mcp_resource_origins: None,
            compaction_response_id: None,
            latest_token_usage_record: None,
            window_number: Some(1),
            first_window_id: None,
            previous_window_id: None,
            window_id: None,
        }));
        let attempt = live.begin_sampling().unwrap();
        rollout.push(live.sampling_started_rollout_item(&attempt, &[]).unwrap());
        let installed = finish_sampling(&mut live, attempt, &mut rollout);
        assert_eq!(installed.context.items, replacement);
        assert_cold_replay(&rollout, &replacement);
    }
}

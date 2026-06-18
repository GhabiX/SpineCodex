use crate::spine::SpineCloseCompact;
use crate::spine::SpineCompactSourceEntryKind;
use crate::spine::SpineCompactSourcePlan;
use crate::spine::SpineError;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

pub(crate) fn spine_close_memory_from_tool_arg(
    node_id: &str,
    source_plan: &SpineCompactSourcePlan,
    node_memory: &str,
) -> Result<SpineCloseCompact, SpineError> {
    if source_plan.node_id.to_string() != node_id {
        return Err(SpineError::CompactFailure(format!(
            "spine.close source plan node {} does not match close node {node_id}",
            source_plan.node_id
        )));
    }
    if source_plan.entries.is_empty() {
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact source plan is empty for node {node_id}"
        )));
    }
    let skeleton = SpineCompactMemorySkeleton::from_source_plan(node_id, source_plan)?;
    let body = skeleton.assemble(node_memory)?;
    Ok(SpineCloseCompact {
        body,
        source_context_range: source_plan.source_context_range.clone(),
        source_raw_range: source_plan.source_raw_range.clone(),
        memory_output_tokens: None,
    })
}

fn response_item_text(item: &ResponseItem) -> Option<&str> {
    let ResponseItem::Message { content, .. } = item else {
        return None;
    };
    match content.as_slice() {
        [ContentItem::InputText { text }] | [ContentItem::OutputText { text }] => Some(text),
        _ => None,
    }
}

#[cfg(test)]
fn validate_source_plan_against_history(
    source_plan: &SpineCompactSourcePlan,
    raw_items: &[ResponseItem],
    _close_call_id: &str,
) -> Result<(), SpineError> {
    let expected_len = source_plan.source_context_range.len();
    if source_plan.entries.len() != expected_len {
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact source entry count {} does not match source context range length {expected_len} for [{}..{})",
            source_plan.entries.len(),
            source_plan.source_context_range.start,
            source_plan.source_context_range.end
        )));
    }
    for (expected_ordinal, entry) in source_plan.entries.iter().enumerate() {
        if entry.source_ordinal != expected_ordinal {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} does not match expected ordinal {expected_ordinal}",
                entry.source_ordinal
            )));
        }
        let expected_context_index = source_plan
            .source_context_range
            .start
            .checked_add(expected_ordinal)
            .ok_or_else(|| {
                SpineError::CompactFailure(
                    "spine.close compact source context index overflow".to_string(),
                )
            })?;
        if entry.context_index != expected_context_index {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} has context_index {}, expected {expected_context_index} for contiguous source context range [{}..{})",
                entry.source_ordinal,
                entry.context_index,
                source_plan.source_context_range.start,
                source_plan.source_context_range.end
            )));
        }
        let Some(host_item) = raw_items.get(entry.context_index) else {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry ordinal {} context_index {} exceeds history length {}",
                entry.source_ordinal,
                entry.context_index,
                raw_items.len()
            )));
        };
        let expected_item = entry.visible_response_item();
        if host_item != &expected_item {
            return Err(SpineError::CompactFailure(format!(
                "spine.close compact source entry mismatch at ordinal {} context_index {} source_hash {}",
                entry.source_ordinal, entry.context_index, entry.source_hash
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SpineCompactMemorySkeleton {
    node_id: String,
    blocks: Vec<SpineCompactMemoryBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SpineCompactMemoryBlock {
    UserMessage {
        body: String,
        user_anchor: Option<u64>,
    },
    ChildMemory {
        body: String,
    },
}

impl SpineCompactMemorySkeleton {
    fn from_source_plan(
        node_id: &str,
        source_plan: &SpineCompactSourcePlan,
    ) -> Result<Self, SpineError> {
        let mut blocks = Vec::new();
        for entry in &source_plan.entries {
            match &entry.kind {
                SpineCompactSourceEntryKind::RawResponseItem {
                    item,
                    from_user: true,
                    user_anchor,
                    ..
                } => {
                    if let Some(text) = response_item_text(item) {
                        blocks.push(SpineCompactMemoryBlock::UserMessage {
                            body: text.to_string(),
                            user_anchor: *user_anchor,
                        });
                    }
                }
                SpineCompactSourceEntryKind::RawResponseItem {
                    from_user: false, ..
                } => {}
                SpineCompactSourceEntryKind::ChildMemory {
                    body,
                    ..
                } => {
                    blocks.push(SpineCompactMemoryBlock::ChildMemory {
                        body: body.clone(),
                    });
                }
            }
        }
        Ok(Self {
            node_id: node_id.to_string(),
            blocks,
        })
    }

    fn assemble(&self, node_memory: &str) -> Result<String, SpineError> {
        let mut body = format!("# Spine Memory {}\n", self.node_id);
        for block in &self.blocks {
            match block {
                SpineCompactMemoryBlock::UserMessage {
                    body: text,
                    user_anchor,
                } => {
                    let heading = user_anchor
                        .map(|anchor| format!("## User Message [U{anchor}]"))
                        .unwrap_or_else(|| "## User Message".to_string());
                    push_memory_block(&mut body, &heading, text);
                }
                SpineCompactMemoryBlock::ChildMemory { body: child } => {
                    push_memory_block(&mut body, "## Child Memory", child);
                }
            }
        }
        validate_generated_node_memory_body(node_memory)?;
        push_memory_block(&mut body, "## Node Memory", node_memory);
        if !body.ends_with('\n') {
            body.push('\n');
        }
        Ok(body)
    }
}

fn push_memory_block(body: &mut String, heading: &str, block_body: &str) {
    body.push('\n');
    body.push_str(heading);
    body.push('\n');
    body.push_str(block_body.trim_matches('\n'));
    body.push('\n');
}

fn validate_generated_node_memory_body(body: &str) -> Result<(), SpineError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(SpineError::CompactFailure(
            "spine.close compact produced empty node_memory".to_string(),
        ));
    }
    if let Some(marker) = forbidden_generated_body_structure_marker(body) {
        return Err(SpineError::CompactFailure(format!(
            "spine.close compact node_memory contains forbidden structure marker {marker:?}"
        )));
    }
    Ok(())
}

fn forbidden_generated_body_structure_marker(body: &str) -> Option<String> {
    const FORBIDDEN_SUBSTRINGS: &[&str] = &[
        "# Spine Memory ",
        "---------- SPINE MEMORY COMPACT ----------",
        "---------- Spine Compact Directive ----------",
        "---------- Spine Close Target ----------",
        "## User Message",
        "## Child Memory",
        "## Memory Slot",
        "## Node Memory",
        "USER_MSG",
    ];
    if let Some(marker) = FORBIDDEN_SUBSTRINGS
        .iter()
        .find(|marker| body.contains(**marker))
    {
        return Some((*marker).to_string());
    }

    body.lines()
        .map(str::trim)
        .find(|line| is_forbidden_generated_body_tag_line(line))
        .map(str::to_string)
}

fn is_forbidden_generated_body_tag_line(line: &str) -> bool {
    line == "<spine_memory>"
        || line == "</spine_memory>"
        || line.starts_with("<SPINE_SLOT_")
        || line.starts_with("</SPINE_SLOT_")
        || line.starts_with("<SPINE_NODE_MEMORY")
        || line.starts_with("</SPINE_NODE_MEMORY")
        || line.starts_with("<USER_MSG_")
        || line.starts_with("</USER_MSG_")
}

#[cfg(test)]
mod spine_close_slot_map_tests {
    use super::*;
    use crate::spine::NodeId;
    use crate::spine::SPINE_TOOL_CLOSE;
    use crate::spine::SPINE_TOOL_NEXT;
    use crate::spine::SPINE_TOOL_OPEN;
    use crate::spine::SPINE_TOOL_TREE;
    use crate::spine::is_spine_close_like_tool_name;

    fn node_id(path: &[u32]) -> NodeId {
        serde_json::from_value(serde_json::json!(path)).expect("node id")
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

    fn source_entry(
        context_index: usize,
        source_ordinal: usize,
        item: ResponseItem,
        from_user: bool,
    ) -> crate::spine::SpineCompactSourcePlanEntry {
        source_entry_with_user_anchor(
            context_index,
            source_ordinal,
            item,
            from_user,
            from_user.then_some(1),
        )
    }

    fn source_entry_with_user_anchor(
        context_index: usize,
        source_ordinal: usize,
        item: ResponseItem,
        from_user: bool,
        user_anchor: Option<u64>,
    ) -> crate::spine::SpineCompactSourcePlanEntry {
        crate::spine::SpineCompactSourcePlanEntry {
            context_index,
            source_ordinal,
            source_hash: format!("hash-{source_ordinal}"),
            kind: SpineCompactSourceEntryKind::RawResponseItem {
                item,
                raw_ordinal: u64::try_from(context_index).expect("context index fits u64"),
                from_user,
                user_anchor,
            },
        }
    }

    fn child_memory_entry(
        context_index: usize,
        source_ordinal: usize,
        body: &str,
    ) -> crate::spine::SpineCompactSourcePlanEntry {
        crate::spine::SpineCompactSourcePlanEntry {
            context_index,
            source_ordinal,
            source_hash: format!("child-hash-{source_ordinal}"),
            kind: SpineCompactSourceEntryKind::ChildMemory {
                node_id: node_id(&[1, 1, 1]),
                compact_id: "mem-1-1-1".to_string(),
                source_raw_range: 2..3,
                body: body.to_string(),
                body_hash: "body-hash".to_string(),
            },
        }
    }

    fn source_plan(
        entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
    ) -> SpineCompactSourcePlan {
        SpineCompactSourcePlan {
            node_id: node_id(&[1, 1]),
            source_context_range: 2..2 + entries.len(),
            source_raw_range: 2..2 + u64::try_from(entries.len()).expect("entries len fits u64"),
            entries,
        }
    }

    fn source_plan_with_context_range(
        source_context_range: std::ops::Range<usize>,
        entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
    ) -> SpineCompactSourcePlan {
        SpineCompactSourcePlan {
            node_id: node_id(&[1, 1]),
            source_raw_range: u64::try_from(source_context_range.start)
                .expect("range start fits u64")
                ..u64::try_from(source_context_range.end).expect("range end fits u64"),
            source_context_range,
            entries,
        }
    }

    #[test]
    fn skeleton_preserves_exact_user_child_and_direct_node_memory() {
        let plan = source_plan(vec![
            source_entry(2, 0, user_message("USER_EXACT\nline 2"), true),
            source_entry(3, 1, assistant_message("assistant details"), false),
            child_memory_entry(4, 2, "# Spine Memory 1.1.1\n\nchild body\n"),
        ]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let body = skeleton
            .assemble("node continuation facts")
            .expect("assembled body");
        assert!(body.contains("# Spine Memory 1.1"));
        assert!(body.contains("## User Message [U1]\nUSER_EXACT\nline 2"));
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild body"));
        assert!(body.contains("## Node Memory\nnode continuation facts"));
        assert!(!body.contains("assistant details"));
        assert!(!body.contains("## Memory Slot"));
    }

    #[test]
    fn direct_close_memory_assembles_user_anchor_evidence() {
        let plan = source_plan(vec![
            source_entry_with_user_anchor(2, 0, user_message("[U7]\napprove"), true, Some(7)),
            source_entry(3, 1, assistant_message("tool detail"), false),
        ]);

        let compact = spine_close_memory_from_tool_arg(
            "1.1",
            &plan,
            "After [U7], implementation continued and tests passed.",
        )
        .expect("direct memory assembly");

        assert!(compact.body.contains("## User Message [U7]\n[U7]\napprove"));
        assert!(
            compact
                .body
                .contains("## Node Memory\nAfter [U7], implementation continued and tests passed.")
        );
        assert_eq!(compact.source_context_range, 2..4);
        assert_eq!(compact.source_raw_range, 2..4);
        assert_eq!(compact.memory_output_tokens, None);
    }

    #[test]
    fn exact_only_skeleton_requires_node_memory() {
        let plan = source_plan(vec![
            source_entry_with_user_anchor(2, 0, user_message("only user one"), true, Some(1)),
            child_memory_entry(3, 1, "# Spine Memory 1.1.1\n\nchild exact\n"),
            source_entry_with_user_anchor(4, 2, user_message("only user two"), true, Some(2)),
        ]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let body = skeleton
            .assemble("node memory for exact-only suffix")
            .expect("assembled body");
        assert!(body.contains("## User Message [U1]\nonly user one"));
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
        assert!(body.contains("## User Message [U2]\nonly user two"));
        assert!(!body.contains("## Memory Slot"));
        assert!(body.contains("## Node Memory\nnode memory for exact-only suffix"));
    }

    #[test]
    fn child_only_skeleton_does_not_invent_user_message() {
        let plan = source_plan(vec![child_memory_entry(
            2,
            0,
            "# Spine Memory 1.1.1\n\nchild exact\n",
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let body = skeleton
            .assemble("preserved close instruction facts")
            .expect("assembled body");
        assert!(body.contains("## Child Memory\n# Spine Memory 1.1.1\n\nchild exact"));
        assert!(body.contains("## Node Memory\npreserved close instruction facts"));
        assert!(!body.contains("## Memory Slot"));
        assert!(!body.contains("## User Message"));
    }

    #[test]
    fn direct_node_memory_allows_markdown_content() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let body = skeleton
            .assemble(
                r#"Node memory can contain Markdown bullets:
- next step remains open

```json
{"quoted":"json is content, not protocol"}
```"#,
            )
            .expect("markdown node memory should be accepted");

        assert!(body.contains("## Node Memory\nNode memory can contain Markdown bullets"));
        assert!(body.contains(r#"{"quoted":"json is content, not protocol"}"#));
    }

    #[test]
    fn direct_node_memory_allows_inline_protocol_marker_discussion() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let body = skeleton
            .assemble("The failure involved a literal <SPINE_SLOT_ substring in the node memory body.")
            .expect("inline protocol marker discussion should be accepted");

        assert!(
            body.contains("## Node Memory\nThe failure involved a literal <SPINE_SLOT_ substring")
        );
    }

    #[test]
    fn direct_node_memory_rejects_structure_pollution() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");
        let err = skeleton
            .assemble("## User Message\npolluted")
            .expect_err("node memory pollution must be rejected");

        assert!(
            err.to_string()
                .contains("contains forbidden structure marker"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn direct_node_memory_rejects_standalone_body_control_tags() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let nested_slot_tag = skeleton
            .assemble("before\n<SPINE_SLOT_1>\nafter")
            .expect_err("standalone slot tag inside node memory must fail");
        assert!(
            nested_slot_tag
                .to_string()
                .contains("contains forbidden structure marker"),
            "unexpected error: {nested_slot_tag}"
        );

        let runtime_tag = skeleton
            .assemble("before\n<spine_memory>\nafter")
            .expect_err("standalone runtime memory tag inside slot body must fail");
        assert!(
            runtime_tag
                .to_string()
                .contains("contains forbidden structure marker"),
            "unexpected error: {runtime_tag}"
        );
    }

    #[test]
    fn direct_node_memory_rejects_empty_and_user_msg_structure() {
        let plan = source_plan(vec![source_entry(
            2,
            0,
            assistant_message("assistant details"),
            false,
        )]);
        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");

        let empty_node = skeleton
            .assemble("  ")
            .expect_err("empty node memory must fail");
        assert!(empty_node.to_string().contains("empty node_memory"));

        let user_msg = skeleton
            .assemble("before\n<USER_MSG_1>\ndo not return this\n</USER_MSG_1>\nafter")
            .expect_err("returned USER_MSG must fail");
        assert!(
            user_msg
                .to_string()
                .contains("contains forbidden structure marker")
        );
    }

    #[test]
    fn multimodal_user_entry_is_not_exact_preserved() {
        let plan = source_plan(vec![crate::spine::SpineCompactSourcePlanEntry {
            context_index: 2,
            source_ordinal: 0,
            source_hash: "hash-0".to_string(),
            kind: SpineCompactSourceEntryKind::RawResponseItem {
                item: ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![
                        ContentItem::InputText {
                            text: "text".to_string(),
                        },
                        ContentItem::InputText {
                            text: "second".to_string(),
                        },
                    ],
                    phase: None,
                },
                raw_ordinal: 2,
                from_user: true,
                user_anchor: Some(1),
            },
        }]);

        let skeleton =
            SpineCompactMemorySkeleton::from_source_plan("1.1", &plan).expect("skeleton");
        let body = skeleton
            .assemble("node multimodal continuation")
            .expect("assembled body");
        assert!(body.contains("## Node Memory\nnode multimodal continuation"));
        assert!(!body.contains("## User Message"));
    }

    #[test]
    fn close_like_tool_name_filters_only_close_and_next() {
        assert!(is_spine_close_like_tool_name(SPINE_TOOL_CLOSE));
        assert!(is_spine_close_like_tool_name(SPINE_TOOL_NEXT));
        assert!(!is_spine_close_like_tool_name(SPINE_TOOL_OPEN));
        assert!(!is_spine_close_like_tool_name(SPINE_TOOL_TREE));
    }

    #[test]
    fn source_plan_validator_rejects_non_contiguous_context_indices() {
        let raw_items = vec![
            user_message("prefix 0"),
            user_message("prefix 1"),
            assistant_message("source 2"),
            assistant_message("gap 3"),
            assistant_message("source 4"),
        ];
        let plan = source_plan_with_context_range(
            2..5,
            vec![
                source_entry(2, 0, raw_items[2].clone(), false),
                source_entry(4, 1, raw_items[4].clone(), false),
            ],
        );

        let err = validate_source_plan_against_history(&plan, &raw_items, "close")
            .expect_err("non-contiguous real context indices must fail");
        assert!(
            err.to_string()
                .contains("source entry count 2 does not match source context range length 3"),
            "unexpected non-contiguous context error: {err}"
        );
    }

    #[test]
    fn source_plan_validator_rejects_duplicate_context_indices() {
        let raw_items = vec![
            user_message("prefix 0"),
            user_message("prefix 1"),
            assistant_message("source 2"),
            assistant_message("source 3"),
        ];
        let plan = source_plan_with_context_range(
            2..4,
            vec![
                source_entry(2, 0, raw_items[2].clone(), false),
                source_entry(2, 1, raw_items[2].clone(), false),
            ],
        );

        let err = validate_source_plan_against_history(&plan, &raw_items, "close")
            .expect_err("duplicate context indices must fail");
        assert!(
            err.to_string().contains("has context_index 2, expected 3"),
            "unexpected duplicate context error: {err}"
        );
    }
}

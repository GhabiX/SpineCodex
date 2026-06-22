use super::*;

pub(super) fn source_entry(
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

pub(super) fn source_entry_with_user_anchor(
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

pub(super) fn child_memory_entry(
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

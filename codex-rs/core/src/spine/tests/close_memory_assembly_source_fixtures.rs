use super::*;
use crate::spine::io::hash_response_items;
use crate::spine::render::memory_response_item;

pub(crate) fn source_entry(
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

pub(crate) fn source_entry_with_user_anchor(
    context_index: usize,
    source_ordinal: usize,
    item: ResponseItem,
    from_user: bool,
    user_anchor: Option<u64>,
) -> crate::spine::SpineCompactSourcePlanEntry {
    let source_hash =
        hash_response_items(std::slice::from_ref(&item)).expect("hash source response item");
    crate::spine::SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash,
        kind: SpineCompactSourceEntryKind::RawResponseItem {
            item,
            raw_ordinal: u64::try_from(context_index).expect("context index fits u64"),
            from_user,
            user_anchor,
        },
    }
}

pub(crate) fn child_memory_entry(
    context_index: usize,
    source_ordinal: usize,
    body: &str,
) -> crate::spine::SpineCompactSourcePlanEntry {
    let visible_item = memory_response_item(body);
    let source_hash = hash_response_items(&[visible_item]).expect("hash child memory item");
    crate::spine::SpineCompactSourcePlanEntry {
        context_index,
        source_ordinal,
        source_hash,
        kind: SpineCompactSourceEntryKind::ChildMemory {
            node_id: node_id(&[1, 1, 1]),
            compact_id: "mem-1-1-1".to_string(),
            source_raw_range: 2..3,
            rendered_context_item_count: None,
            body: body.to_string(),
            body_hash: "body-hash".to_string(),
        },
    }
}

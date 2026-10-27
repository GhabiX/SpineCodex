use super::message_from_response_item;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ResponseItem;
use spine_core::host::RawBoundary;
use spine_core::host::SpineChar;

pub(crate) fn response_item_to_char_and_source(
    item: &ResponseItemEnvelope,
    boundary: RawBoundary,
) -> (SpineChar, ResponseItemEnvelope) {
    let mut source_item = item.clone();
    if let ResponseItem::Reasoning { content, .. } = &mut source_item.item
        && content.is_none()
    {
        // Request serialization omits an empty reasoning content list. Preserve that wire shape
        // after rollout replay, where an omitted field deserializes as `None`.
        *content = Some(Vec::new());
    }
    let character = match &item.item {
        ResponseItem::Message { .. } | ResponseItem::Reasoning { .. } => {
            SpineChar::Message(message_from_response_item(boundary.0 as usize, item))
        }
        _ => SpineChar::Opaque { boundary },
    };
    (character, source_item)
}

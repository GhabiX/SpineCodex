use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ResponseItem;

use crate::spine::SpineError;

pub(crate) fn anchored_user_message_item(
    item: &ResponseItem,
    user_anchor: u64,
) -> Result<ResponseItem, SpineError> {
    let mut item = item.clone();
    let ResponseItem::Message { role, content, .. } = &mut item else {
        return Err(SpineError::InvalidEvent(format!(
            "user anchor U{user_anchor} attached to non-message response item"
        )));
    };
    if role != "user" {
        return Err(SpineError::InvalidEvent(format!(
            "user anchor U{user_anchor} attached to non-user message"
        )));
    }
    prefix_user_anchor(content, user_anchor);
    Ok(item)
}

pub(crate) fn user_message_memory_body(item: &ResponseItem) -> Option<String> {
    let ResponseItem::Message { content, .. } = item else {
        return None;
    };
    Some(render_user_content_for_memory(content))
}

fn prefix_user_anchor(content: &mut Vec<ContentItem>, user_anchor: u64) {
    let anchor_prefix = format!("[U{user_anchor}]");
    for content_item in content.iter_mut() {
        match content_item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                if !text.starts_with(&anchor_prefix) {
                    *text = format!("{anchor_prefix}\n{text}");
                }
                return;
            }
            ContentItem::InputImage { .. } => {}
        }
    }

    content.insert(
        0,
        ContentItem::InputText {
            text: format!(
                "{anchor_prefix}\n{}",
                render_user_content_for_memory(content)
            ),
        },
    );
}

fn render_user_content_for_memory(content: &[ContentItem]) -> String {
    let out = content
        .iter()
        .filter_map(memory_content_part)
        .collect::<Vec<_>>()
        .join("\n");
    if out.is_empty() {
        "<empty user message>".to_string()
    } else {
        out
    }
}

fn memory_content_part(item: &ContentItem) -> Option<String> {
    let part = match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
            text.trim_matches('\n').to_string()
        }
        ContentItem::InputImage { detail, .. } => match detail {
            Some(detail) => format!("<image omitted detail={}>", image_detail_label(*detail)),
            None => "<image omitted>".to_string(),
        },
    };
    (!part.is_empty()).then_some(part)
}

fn image_detail_label(detail: ImageDetail) -> &'static str {
    match detail {
        ImageDetail::Auto => "auto",
        ImageDetail::Low => "low",
        ImageDetail::High => "high",
        ImageDetail::Original => "original",
    }
}

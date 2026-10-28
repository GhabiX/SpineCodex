use super::message_from_response_item;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ResponseItem;
use spine_core::host::MessageRole;
use spine_core::host::RawBoundary;
use spine_core::host::SpawnArrival;
use spine_core::host::SpineChar;

pub(crate) fn response_item_to_char_and_source(
    item: &ResponseItemEnvelope,
    boundary: RawBoundary,
) -> Result<(SpineChar, ResponseItemEnvelope), String> {
    let mut source_item = item.clone();
    if let ResponseItem::Reasoning { content, .. } = &mut source_item.item
        && content.is_none()
    {
        // Request serialization omits an empty reasoning content list. Preserve that wire shape
        // after rollout replay, where an omitted field deserializes as `None`.
        *content = Some(Vec::new());
    }
    let arrival = item
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.spine_arrival.as_ref());
    let character = match (arrival, &item.item) {
        (Some(arrival), _) => SpineChar::Arrival {
            boundary,
            arrival: serde_json::from_value::<SpawnArrival>(arrival.clone()).map_err(|error| {
                format!(
                    "invalid Spine arrival at raw boundary {}: {error}",
                    boundary.0
                )
            })?,
        },
        (None, ResponseItem::Message { .. } | ResponseItem::Reasoning { .. }) => {
            SpineChar::Message(message_from_response_item(boundary.0 as usize, item))
        }
        (None, _) => SpineChar::Opaque { boundary },
    };
    Ok((character, source_item))
}

/// The history item that carries `arrival` into its parent: the rendered
/// arrival as a contextual user message, typed by its metadata.
pub(crate) fn arrival_item(arrival: &SpawnArrival) -> Result<ResponseItemEnvelope, String> {
    Ok(ResponseItemEnvelope {
        item: crate::context::ContextualUserFragment::into(
            crate::context::SpineArrivalFragment::new(arrival)?,
        ),
        metadata: Some(codex_history::CodexHarnessMetadata {
            spine_arrival: Some(
                serde_json::to_value(arrival)
                    .map_err(|error| format!("failed to encode a Spine arrival: {error}"))?,
            ),
            ..Default::default()
        }),
    })
}

/// The assistant messages of the sampling in progress, held until it ends.
/// They are the agent's reply when the sampling completes and contains
/// nothing else; any other item settles them as ordinary assistant messages.
pub(crate) struct SamplingReplies<T> {
    held: Vec<(SpineChar, T)>,
    other_item: bool,
}

impl<T> Default for SamplingReplies<T> {
    fn default() -> Self {
        Self {
            held: Vec::new(),
            other_item: false,
        }
    }
}

impl<T> SamplingReplies<T> {
    /// Returns the characters this one settles, in source order.
    pub(crate) fn observe(&mut self, character: SpineChar, item: T) -> Vec<(SpineChar, T)> {
        if !self.other_item
            && matches!(&character, SpineChar::Message(message) if message.role == MessageRole::Assistant)
        {
            self.held.push((character, item));
            return Vec::new();
        }
        self.other_item = true;
        let mut settled = std::mem::take(&mut self.held);
        settled.push((character, item));
        settled
    }

    /// Settles the held characters when the sampling ends; `completed` says
    /// whether it ended with a complete response.
    pub(crate) fn finish(self, completed: bool) -> Vec<(SpineChar, T)> {
        let reply = completed && !self.other_item;
        self.held
            .into_iter()
            .map(|(character, item)| match character {
                SpineChar::Message(mut message) if reply && !message.content.trim().is_empty() => {
                    message.role = MessageRole::Reply;
                    (SpineChar::Message(message), item)
                }
                character => (character, item),
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "context_handler_tests.rs"]
mod tests;

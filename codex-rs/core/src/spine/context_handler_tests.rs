use super::*;
use pretty_assertions::assert_eq;
use spine_core::host::Message;

fn assistant(boundary: u64, content: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role: MessageRole::Assistant,
        content: content.to_string(),
    })
}

fn roles(settled: &[(SpineChar, ())]) -> Vec<Option<MessageRole>> {
    settled
        .iter()
        .map(|(character, ())| match character {
            SpineChar::Message(message) => Some(message.role),
            _ => None,
        })
        .collect()
}

#[test]
fn a_completed_sampling_of_only_assistant_text_is_the_reply() {
    let mut replies = SamplingReplies::default();
    assert!(replies.observe(assistant(0, ""), ()).is_empty());
    assert!(replies.observe(assistant(1, "the answer"), ()).is_empty());

    // The empty one stands for reasoning: it holds no text, so it stays assistant.
    assert_eq!(
        roles(&replies.finish(/*completed*/ true)),
        [Some(MessageRole::Assistant), Some(MessageRole::Reply)]
    );
}

#[test]
fn a_tool_item_or_an_unfinished_sampling_leaves_assistant_text_as_is() {
    let mut replies = SamplingReplies::default();
    assert!(replies.observe(assistant(0, "let me check"), ()).is_empty());
    let settled = replies.observe(
        SpineChar::Opaque {
            boundary: RawBoundary(1),
        },
        (),
    );
    assert_eq!(roles(&settled), [Some(MessageRole::Assistant), None]);
    let settled = replies.observe(assistant(2, "done"), ());
    assert_eq!(roles(&settled), [Some(MessageRole::Assistant)]);
    assert!(replies.finish(/*completed*/ true).is_empty());

    let mut replies = SamplingReplies::default();
    assert!(replies.observe(assistant(0, "partial"), ()).is_empty());
    assert_eq!(
        roles(&replies.finish(/*completed*/ false)),
        [Some(MessageRole::Assistant)]
    );
}

#[test]
fn an_arrival_item_maps_back_to_its_arrival() {
    let arrival = SpawnArrival {
        producer: spine_core::host::NodeId::root_epoch(1).child(2),
        outcome: spine_core::host::SpawnOutcome::Completed,
        memory: Some("found it".to_string()),
        diagnostic: None,
        execution_ref: None,
    };
    let item = arrival_item(&arrival).unwrap();
    let ResponseItem::Message { role, content, .. } = &item.item else {
        panic!("an arrival is a message");
    };
    assert_eq!(role, "user");
    assert_eq!(
        content,
        &[codex_protocol::models::ContentItem::InputText {
            text:
                "<spine_arrival node_id=\"1.2\" outcome=\"completed\">\nfound it\n</spine_arrival>"
                    .to_string(),
        }]
    );
    assert_eq!(
        response_item_to_char_and_source(&item, RawBoundary(7))
            .unwrap()
            .0,
        SpineChar::Arrival {
            boundary: RawBoundary(7),
            arrival,
        }
    );
}

use super::*;
use crate::MessageRole;
use crate::RawSpan;
use pretty_assertions::assert_eq;

fn message(boundary: u64, role: MessageRole, content: &str) -> SpineChar {
    SpineChar::Message(Message {
        boundary: RawBoundary(boundary),
        role,
        content: content.to_string(),
    })
}

fn turn_aborted(boundary: u64) -> SpineChar {
    SpineChar::TurnAborted(Message {
        boundary: RawBoundary(boundary),
        role: MessageRole::ContextualUser,
        content: "<turn_aborted>interrupted</turn_aborted>".to_string(),
    })
}

fn request(boundary: u64, call_id: &str, name: &str) -> SpineChar {
    SpineChar::ToolRequest(ToolRequestChar {
        boundary: RawBoundary(boundary),
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments: "{}".to_string(),
    })
}

fn response(boundary: u64, call_id: &str, output: &str) -> SpineChar {
    SpineChar::ToolResponse(ToolResponseChar {
        boundary: RawBoundary(boundary),
        call_id: call_id.to_string(),
        outcome: ToolOutcome::Succeeded,
        output: output.to_string(),
    })
}

#[test]
fn one_item_character_adds_exactly_one_stack_cell() {
    let mut parser = SpineCharParser::default();

    let step = parser
        .eat(message(1, MessageRole::User, "request"))
        .unwrap();

    assert_eq!(step.stack_size(), 1);
    assert_eq!(parser.stack().len(), 1);
    assert_eq!(
        step.events(),
        &[RolloutEvent::Message(Message {
            boundary: RawBoundary(1),
            role: MessageRole::User,
            content: "request".to_string(),
        })]
    );
}

#[test]
fn assistant_prefix_waits_and_joins_the_following_tool_group() {
    let mut parser = SpineCharParser::default();

    let assistant = parser
        .eat(message(1, MessageRole::Assistant, "working"))
        .unwrap();
    assert!(assistant.events().is_empty());
    assert_eq!(assistant.pending_boundaries(), &[RawBoundary(1)]);

    let request = parser.eat(request(2, "call", "shell")).unwrap();
    assert!(request.events().is_empty());
    assert_eq!(
        request.pending_boundaries(),
        &[RawBoundary(1), RawBoundary(2)]
    );

    let completed = parser.eat(response(3, "call", "done")).unwrap();
    assert_eq!(completed.pending_boundaries(), &[]);
    let [RolloutEvent::SourceSpan { span, .. }] = completed.events() else {
        panic!("expected one completed source span");
    };
    assert_eq!(
        *span,
        RawSpan {
            start: RawBoundary(1),
            end: RawBoundary(3),
        }
    );
    assert_eq!(
        completed.completed_calls()[0].calls,
        vec![ToolUse {
            call_id: "call".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
            call_ordinal: None,
            outcome: Some(ToolOutcome::Succeeded),
            output: Some("done".to_string()),
            output_boundary: Some(RawBoundary(3)),
        }]
    );
    assert_eq!(parser.stack().len(), 3);
}

#[test]
fn parallel_tool_group_reduces_only_after_every_response() {
    let mut parser = SpineCharParser::default();
    parser.eat(request(1, "a", "shell")).unwrap();
    parser.eat(request(2, "b", "shell")).unwrap();

    let partial = parser.eat(response(3, "b", "second")).unwrap();
    assert!(partial.events().is_empty());

    let complete = parser.eat(response(4, "a", "first")).unwrap();
    let [RolloutEvent::SourceSpan { span, .. }] = complete.events() else {
        panic!("expected one completed source span");
    };
    assert_eq!(span.start, RawBoundary(1));
    assert_eq!(span.end, RawBoundary(4));
    assert_eq!(complete.completed_calls()[0].calls.len(), 2);
    assert_eq!(parser.stack().len(), 4);
}

#[test]
fn opaque_and_synthetic_chars_are_one_cell_semantic_inputs() {
    let mut parser = SpineCharParser::default();
    let opaque = parser
        .eat(SpineChar::Opaque {
            boundary: RawBoundary(1),
        })
        .unwrap();
    assert_eq!(
        opaque.events(),
        &[RolloutEvent::Opaque {
            boundary: RawBoundary(1)
        }]
    );

    let item = ContextItem::SyntheticNode {
        node_id: crate::NodeId::root_epoch(1),
        summary: "root".to_string(),
        status: crate::NodeStatus::Live,
    };
    let synthetic = parser
        .eat(SpineChar::Synthetic {
            boundary: RawBoundary(2),
            item: item.clone(),
        })
        .unwrap();
    assert_eq!(
        synthetic.events(),
        &[RolloutEvent::Synthetic {
            boundary: RawBoundary(2),
            item,
        }]
    );
    assert_eq!(synthetic.stack_size(), 2);
}

#[test]
fn pending_boundaries_keep_context_order_when_responses_are_out_of_call_order() {
    let mut parser = SpineCharParser::default();
    parser.eat(request(1, "a", "shell")).unwrap();
    parser.eat(request(2, "b", "shell")).unwrap();

    let partial = parser.eat(response(3, "a", "first")).unwrap();

    assert_eq!(
        partial.pending_boundaries(),
        &[RawBoundary(1), RawBoundary(2), RawBoundary(3)]
    );
}

#[test]
fn failed_character_does_not_commit_parser_state() {
    let mut parser = SpineCharParser::default();
    parser.eat(request(1, "call", "shell")).unwrap();
    let before = parser.clone();

    let result = parser.eat(message(2, MessageRole::User, "interrupt"));

    assert!(matches!(
        result,
        Err(CharParseError::IncompleteToolGroup {
            boundary: RawBoundary(2)
        })
    ));
    assert_eq!(parser, before);
}

#[test]
fn turn_abort_discards_incomplete_tool_group_without_fabricating_an_outcome() {
    let mut parser = SpineCharParser::default();
    parser
        .eat(message(1, MessageRole::Assistant, "starting"))
        .unwrap();
    parser.eat(request(2, "call", "shell")).unwrap();

    let aborted = parser.eat(turn_aborted(3)).unwrap();

    assert_eq!(
        aborted.events(),
        &[RolloutEvent::Message(Message {
            boundary: RawBoundary(3),
            role: MessageRole::ContextualUser,
            content: "<turn_aborted>interrupted</turn_aborted>".to_string(),
        })]
    );
    assert!(aborted.pending_boundaries().is_empty());
    assert_eq!(parser.stack().len(), 3);
    assert!(
        parser
            .eat(message(4, MessageRole::User, "continue"))
            .is_ok()
    );
}

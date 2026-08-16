use super::*;
use pretty_assertions::assert_eq;

#[test]
fn typed_fragments_own_exact_rendering() {
    let node = SpineNodeFragment::new(
        &NodeId::root_epoch(1).child(1),
        "child <scope>",
        NodeStatus::Live,
        NodeContextCost::Percentage(13),
        "Node guidance.",
    )
    .unwrap();
    let memory = SpineMemoryFragment::new(&NodeId::root_epoch(1), "finished").unwrap();
    let opened = SpineNodeFragment::new(
        &NodeId::root_epoch(1).child(1),
        "child <scope>",
        NodeStatus::Opened,
        NodeContextCost::Unavailable,
        "Node guidance.",
    )
    .unwrap();

    assert_eq!(
        node.render(),
        "<spine_node id=\"1.1\" summary=\"child &lt;scope&gt;\" status=\"live\">\nNode guidance.\n</spine_node>"
    );
    assert_eq!(
        memory.render(),
        "<spine_memory node_id=\"1\">\nfinished\n</spine_memory>"
    );
    assert_eq!(
        opened.render(),
        "<spine_node id=\"1.1\" summary=\"child &lt;scope&gt;\" status=\"opened\">\nNode guidance.\n</spine_node>"
    );
}

#[test]
fn typed_node_fragment_is_stable_across_token_accounting_updates() {
    let before = SpineNodeFragment::new(
        &NodeId::root_epoch(1).child(1),
        "active",
        NodeStatus::Opened,
        NodeContextCost::Percentage(10),
        "",
    )
    .unwrap();
    let after = SpineNodeFragment::new(
        &NodeId::root_epoch(1).child(1),
        "active",
        NodeStatus::Opened,
        NodeContextCost::Percentage(90),
        "",
    )
    .unwrap();

    assert_eq!(before.render(), after.render());
    assert_eq!(
        before.render(),
        "<spine_node id=\"1.1\" summary=\"active\" status=\"opened\">\n</spine_node>"
    );
}

#[test]
fn final_rendered_fragment_has_a_hard_byte_limit() {
    let accepted = SpineMemoryFragment::new(
        &NodeId::root_epoch(1),
        &"x".repeat(MAX_SPINE_FRAGMENT_BYTES - 64),
    )
    .unwrap();
    let result = SpineMemoryFragment::new(
        &NodeId::root_epoch(1),
        &"x".repeat(MAX_SPINE_FRAGMENT_BYTES),
    );

    assert!(accepted.render().len() <= MAX_SPINE_FRAGMENT_BYTES);
    assert!(result.is_err());
}

#[test]
fn user_anchor_is_a_typed_fragment_with_exact_legacy_rendering() {
    let anchor = SpineUserAnchor::new(u64::MAX);

    assert_eq!(anchor.role(), "user");
    assert_eq!(anchor.render(), format!("[U{}]\n", u64::MAX));
    assert!(SpineUserAnchor::matches_text(&anchor.render()));
}

#[test]
fn complete_model_item_gate_counts_unicode_and_json_escaping() {
    let unicode = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "🦀".repeat(1_900),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let escaped = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "\0".repeat(1_900),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };

    assert!(spine_model_item_wire_bytes(&unicode).unwrap() < MAX_SPINE_MODEL_ITEM_WIRE_BYTES);
    assert!(validate_spine_model_item(&unicode).is_ok());
    assert!(spine_model_item_wire_bytes(&escaped).unwrap() > MAX_SPINE_MODEL_ITEM_WIRE_BYTES);
    assert!(validate_spine_model_item(&escaped).is_err());
}

#[test]
fn complete_model_item_gate_has_exact_just_under_and_over_boundaries() {
    let empty = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: String::new(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let fixed_tokens = spine_model_item_wire_bytes(&empty).unwrap();
    let text_tokens = MAX_SPINE_MODEL_ITEM_WIRE_BYTES - fixed_tokens;
    let accepted = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "x".repeat(text_tokens),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    assert_eq!(
        spine_model_item_wire_bytes(&accepted).unwrap(),
        MAX_SPINE_MODEL_ITEM_WIRE_BYTES
    );
    assert!(validate_spine_model_item(&accepted).is_ok());

    let rejected = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "x".repeat(text_tokens + 1),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    assert!(validate_spine_model_item(&rejected).is_err());
}

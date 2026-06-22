use super::*;

#[test]
fn multimodal_user_entry_is_preserved_as_runtime_text() {
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
                    ContentItem::InputImage {
                        image_url: "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR".to_string(),
                        detail: Some(codex_protocol::models::ImageDetail::High),
                    },
                ],
                phase: None,
            },
            raw_ordinal: 2,
            from_user: true,
            user_anchor: Some(1),
        },
    }]);

    let skeleton = SpineMemoryAssemblySkeleton::from_source_plan("1.1", &plan).expect("skeleton");
    let body = skeleton
        .assemble("node multimodal continuation")
        .expect("assembled body");
    assert!(body.contains("## User Message [U1]\ntext\nsecond\n<image omitted detail=high>"));
    assert!(body.contains("## Node Memory\nnode multimodal continuation"));
    assert!(!body.contains("RAW_IMAGE_SHOULD_NOT_APPEAR"));
}

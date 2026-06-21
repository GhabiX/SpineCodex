use super::*;

#[test]
fn multimodal_user_message_receives_anchor_without_dropping_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = multimodal_user_item();
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let raw = vec![Some(item)];
    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [
                    ContentItem::InputText { text },
                    ContentItem::InputImage { image_url, detail: Some(ImageDetail::High) },
                    ContentItem::InputText { text: second },
                ] if text == "[U1]\nfirst text"
                    && image_url == "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR"
                    && second == "second text"
            )
    ));
}

#[test]
fn image_only_user_message_receives_synthetic_anchor_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rollout = rollout_path(&dir);
    let item = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image_url: "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR".to_string(),
            detail: Some(ImageDetail::Low),
        }],
        phase: None,
    };
    let mut runtime = SpineRuntime::load_or_create(&rollout, 0).expect("create spine");

    runtime.observe_raw_items(1).expect("observe raw");
    runtime
        .observe_context_item(0, 0, &item)
        .expect("observe context item");

    let raw = vec![Some(item)];
    let materialized = runtime.materialize_history(&raw).expect("materialize");
    assert!(matches!(
        materialized.as_slice(),
        [ResponseItem::Message { content, .. }]
            if matches!(
                content.as_slice(),
                [
                    ContentItem::InputText { text },
                    ContentItem::InputImage { image_url, detail: Some(ImageDetail::Low) },
                ] if text == "[U1]\n<image omitted detail=low>"
                    && image_url == "data:image/png;base64,RAW_IMAGE_SHOULD_NOT_APPEAR"
            )
    ));
}

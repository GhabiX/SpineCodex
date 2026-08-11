use super::*;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::unbounded_channel;

fn line_text(line: Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn view_with_capacity(
    max_threads: usize,
) -> (
    ExperimentalFeaturesView,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let (tx, rx) = unbounded_channel();
    let view = ExperimentalFeaturesView::new(
        vec![ExperimentalFeatureItem {
            feature: Feature::SpineSpawn,
            name: "Spine Spawn".to_string(),
            description: "Spawn branches atomically.".to_string(),
            enabled: true,
            max_concurrent_threads_per_session: Some(max_threads),
        }],
        AppEventSender::new(tx),
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    (view, rx)
}

#[test]
fn capacity_is_clamped_and_adjusted_as_total_threads() {
    let (mut view, _rx) = view_with_capacity(/*max_threads*/ 1);
    assert_eq!(view.features[0].max_concurrent_threads_per_session, Some(3));
    assert!(
        view.build_rows()[0]
            .name
            .contains("Concurrent branch agents: 2")
    );

    view.decrement_selected_capacity();
    assert_eq!(view.features[0].max_concurrent_threads_per_session, Some(3));
    view.increment_selected_capacity();
    assert_eq!(view.features[0].max_concurrent_threads_per_session, Some(4));
}

#[test]
fn closing_emits_feature_flag_and_capacity_together() {
    let (mut view, mut rx) = view_with_capacity(/*max_threads*/ 5);

    assert_eq!(view.on_ctrl_c(), CancellationEvent::Handled);
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateFeatureFlags {
            updates,
            spine_spawn_max_concurrent_threads_per_session: Some(5),
        }) if updates == vec![(Feature::SpineSpawn, true)]
    ));
}

#[test]
fn capacity_hint_uses_configured_horizontal_bindings() {
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.move_left = vec![key_hint::plain(KeyCode::Char('h'))];
    keymap.move_right = vec![key_hint::plain(KeyCode::Char('l'))];

    let hint = line_text(experimental_popup_hint_line(&keymap, true, u16::MAX));

    assert!(
        hint.contains("h/l branch agents"),
        "expected configured horizontal bindings in hint, got {hint:?}"
    );
}

#[test]
fn experimental_hint_never_exceeds_available_width() {
    let keymap = crate::keymap::RuntimeKeymap::defaults().list;
    for available_width in 0..=80 {
        let hint = experimental_popup_hint_line(&keymap, true, available_width);
        assert!(
            hint.width() <= usize::from(available_width),
            "hint width {} exceeded available width {available_width}: {:?}",
            hint.width(),
            line_text(hint)
        );
    }
}

#[test]
fn tiny_capacity_hint_stays_atomic() {
    let keymap = crate::keymap::RuntimeKeymap::defaults().list;
    assert_eq!(
        line_text(experimental_popup_hint_line(&keymap, true, 3)),
        "←/→"
    );
    assert!(line_text(experimental_popup_hint_line(&keymap, true, 2)).is_empty());
}

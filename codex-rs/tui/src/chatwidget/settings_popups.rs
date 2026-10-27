//! Settings-adjacent popup surfaces for `ChatWidget`.
//!
//! This keeps theme and experimental-feature UI out of the main
//! orchestration module without changing their event wiring.

use super::*;
use crate::bottom_pane::ExperimentalFeatureItem;
use codex_features::FEATURES;

impl ChatWidget {
    pub(super) fn open_theme_picker(&mut self) {
        let codex_home = codex_utils_home_dir::find_codex_home().ok();
        let params = crate::theme_picker::build_theme_picker_params(
            self.local_settings.tui.theme.as_deref(),
            codex_home.as_deref(),
            self.last_rendered_width.get(),
        );
        self.bottom_pane.show_selection_view(params);
    }

    pub(crate) fn open_experimental_popup(&mut self) {
        let Some(thread_id) = self.thread_id() else {
            self.add_info_message(
                "Experimental features are unavailable until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        self.app_event_tx.send(AppEvent::FetchExperimentalFeatures {
            thread_id,
            response_tx,
        });
        let features: Vec<ExperimentalFeatureItem> = FEATURES
            .iter()
            .filter(|spec| {
                self.config.features.enabled(Feature::SpineJit)
                    && matches!(
                        spec.id,
                        Feature::SpineJit
                            | Feature::SpineSpawn
                            | Feature::SpinetreeMemoryProjection
                    )
            })
            .filter_map(|spec| {
                let name = spec.stage.experimental_menu_name()?;
                let description = spec.stage.experimental_menu_description()?;
                Some(ExperimentalFeatureItem {
                    key: spec.key.to_string(),
                    name: name.to_string(),
                    description: description.to_string(),
                    enabled: self.config.features.enabled(spec.id),
                    writable: true,
                    max_concurrent_threads_per_session: (spec.id == Feature::SpineSpawn)
                        .then_some(self.config.spine_spawn.max_concurrent_threads_per_session),
                })
            })
            .collect();
        let view = ExperimentalFeaturesView::new(
            features,
            thread_id,
            self.config.features.enabled(Feature::SpineJit),
            Some(response_rx),
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }
}

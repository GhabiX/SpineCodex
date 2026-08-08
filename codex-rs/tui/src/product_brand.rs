use crate::legacy_core::config::Config;
use codex_features::Feature;
use ratatui::prelude::Span;
use ratatui::style::Color;
use ratatui::style::Stylize;

/// Shared foreground color for Spine product identity and Spine-owned activity.
pub(crate) const SPINE_BRAND_COLOR: Color = Color::LightGreen;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ProductBrand {
    #[default]
    Codex,
    Spine,
}

impl ProductBrand {
    pub(crate) fn from_config(config: &Config) -> Self {
        if config.features.enabled(Feature::SpineJit) {
            Self::Spine
        } else {
            Self::Codex
        }
    }

    pub(crate) fn title_spans(self) -> Vec<Span<'static>> {
        match self {
            Self::Codex => vec![Span::from("OpenAI Codex").bold()],
            Self::Spine => vec![
                Span::from("Spine").fg(SPINE_BRAND_COLOR).bold(),
                Span::from(" Codex").bold(),
            ],
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Codex => "OpenAI Codex",
            Self::Spine => "Spine Codex",
        }
    }
}

//! Centralized motion primitives for the TUI.
//!
//! Callers choose an explicit reduced-motion fallback here instead of reaching
//! directly for time-varying spinner or shimmer helpers.

use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::time::Duration;
use std::time::Instant;

use ratatui::style::Modifier;
use ratatui::style::Stylize;
use ratatui::text::Span;

use crate::product_brand::SPINE_BRAND_COLOR;
use crate::shimmer::green_shimmer_spans;
use crate::shimmer::green_then_default_shimmer_spans;
use crate::shimmer::motion_green_style;
use crate::shimmer::shimmer_spans;
use crate::shimmer::spine_brand_shimmer_spans;

pub(crate) const ORGANIC_ACTIVITY_WORDS: &[&str] = &[
    "Germinating",
    "Budding",
    "Sprouting",
    "Rooting",
    "Branching",
    "Unfurling",
    "Blooming",
    "Flourishing",
    "Sketching",
    "Shaping",
    "Layering",
    "Weaving",
    "Composing",
    "Rendering",
    "Unfolding",
    "Evolving",
    "Awakening",
    "Becoming",
    "Emerging",
    "Stirring",
    "Quickening",
    "Kindling",
    "Growing",
    "Greening",
    "Blossoming",
    "Ripening",
    "Renewing",
    "Cultivating",
    "Nurturing",
    "Deepening",
    "Flowing",
    "Gathering",
    "Coalescing",
    "Distilling",
    "Refining",
    "Crystallizing",
    "Illuminating",
    "Glimmering",
    "Resonating",
    "Materializing",
];

pub(crate) fn activity_word_for_identity(identity: &str) -> &'static str {
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    ORGANIC_ACTIVITY_WORDS[hasher.finish() as usize % ORGANIC_ACTIVITY_WORDS.len()]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MotionMode {
    Animated,
    Reduced,
}

impl MotionMode {
    pub(crate) fn from_animations_enabled(animations_enabled: bool) -> Self {
        if animations_enabled {
            Self::Animated
        } else {
            Self::Reduced
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReducedMotionIndicator {
    Hidden,
    StaticBullet,
}

pub(crate) fn activity_indicator(
    start_time: Option<Instant>,
    motion_mode: MotionMode,
    reduced_motion_indicator: ReducedMotionIndicator,
) -> Option<Span<'static>> {
    match motion_mode {
        MotionMode::Animated => Some(animated_activity_indicator(start_time)),
        MotionMode::Reduced => match reduced_motion_indicator {
            ReducedMotionIndicator::Hidden => None,
            ReducedMotionIndicator::StaticBullet => Some("•".dim()),
        },
    }
}

pub(crate) fn shimmer_text(text: &str, motion_mode: MotionMode) -> Vec<Span<'static>> {
    match motion_mode {
        MotionMode::Animated => shimmer_spans(text),
        MotionMode::Reduced => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text.to_string().into()]
            }
        }
    }
}

pub(crate) fn green_growth_marker(elapsed: Duration, motion_mode: MotionMode) -> Span<'static> {
    let green = motion_green_style();
    if motion_mode == MotionMode::Reduced {
        return Span::styled("ϒ", green);
    }

    let phase_ms = elapsed.as_millis() % 2_700;
    if phase_ms < 900 {
        Span::styled(".", green.add_modifier(Modifier::DIM))
    } else if phase_ms < 1_200 {
        Span::styled(".", green.add_modifier(Modifier::BOLD))
    } else if phase_ms < 1_360 {
        Span::styled("ʏ", green.add_modifier(Modifier::DIM))
    } else if phase_ms < 1_520 {
        Span::styled("Ү", green)
    } else if phase_ms < 2_420 {
        Span::styled("ϒ", green.add_modifier(Modifier::BOLD))
    } else if phase_ms < 2_530 {
        Span::styled("ϒ", green)
    } else if phase_ms < 2_615 {
        Span::styled("Ү", green)
    } else {
        Span::styled("ʏ", green.add_modifier(Modifier::DIM))
    }
}

pub(crate) fn green_shimmer_text(text: &str, motion_mode: MotionMode) -> Vec<Span<'static>> {
    match motion_mode {
        MotionMode::Animated => green_shimmer_spans(text),
        MotionMode::Reduced => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![Span::styled(text.to_string(), motion_green_style())]
            }
        }
    }
}

pub(crate) fn green_then_default_shimmer_text(
    green_text: &str,
    default_text: &str,
    motion_mode: MotionMode,
) -> Vec<Span<'static>> {
    match motion_mode {
        MotionMode::Animated => green_then_default_shimmer_spans(green_text, default_text),
        MotionMode::Reduced => {
            let mut spans = Vec::with_capacity(2);
            if !green_text.is_empty() {
                spans.push(Span::styled(green_text.to_string(), motion_green_style()));
            }
            if !default_text.is_empty() {
                spans.push(default_text.to_string().into());
            }
            spans
        }
    }
}

pub(crate) fn spine_brand_shimmer_text(text: &str, motion_mode: MotionMode) -> Vec<Span<'static>> {
    match motion_mode {
        MotionMode::Animated => spine_brand_shimmer_spans(text),
        MotionMode::Reduced => {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![Span::from(text.to_string()).fg(SPINE_BRAND_COLOR)]
            }
        }
    }
}

fn animated_activity_indicator(start_time: Option<Instant>) -> Span<'static> {
    let elapsed = start_time.map(|st| st.elapsed()).unwrap_or_default();
    if supports_color::on_cached(supports_color::Stream::Stdout)
        .map(|level| level.has_16m)
        .unwrap_or(false)
    {
        shimmer_spans("•")
            .into_iter()
            .next()
            .unwrap_or_else(|| "•".into())
    } else {
        let blink_on = (elapsed.as_millis() / 600).is_multiple_of(2);
        if blink_on { "•".into() } else { "◦".dim() }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn reduced_motion_activity_indicator_uses_explicit_fallback() {
        assert_eq!(
            activity_indicator(
                /*start_time*/ None,
                MotionMode::Reduced,
                ReducedMotionIndicator::Hidden,
            ),
            None
        );
        assert_eq!(
            activity_indicator(
                /*start_time*/ None,
                MotionMode::Reduced,
                ReducedMotionIndicator::StaticBullet,
            ),
            Some("•".dim())
        );
    }

    #[test]
    fn reduced_motion_shimmer_text_is_plain_text() {
        assert_eq!(
            shimmer_text("Loading", MotionMode::Reduced),
            vec!["Loading".into()]
        );
        assert_eq!(
            shimmer_text("", MotionMode::Reduced),
            Vec::<Span<'static>>::new()
        );
    }

    #[test]
    fn activity_word_is_stable_for_an_identity() {
        let word = activity_word_for_identity("turn-1");
        assert!(ORGANIC_ACTIVITY_WORDS.contains(&word));
        assert_eq!(activity_word_for_identity("turn-1"), word);
    }

    #[test]
    fn reduced_motion_segmented_shimmer_preserves_both_palettes() {
        assert_eq!(
            green_then_default_shimmer_text("Blooming", ": Planning", MotionMode::Reduced),
            vec![
                Span::styled("Blooming", motion_green_style()),
                ": Planning".into(),
            ]
        );
    }

    #[test]
    fn spine_brand_shimmer_text_uses_brand_color_without_dimming() {
        for motion_mode in [MotionMode::Animated, MotionMode::Reduced] {
            let spans = spine_brand_shimmer_text("Growing", motion_mode);
            assert_eq!(
                spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                "Growing"
            );
            assert!(
                spans
                    .iter()
                    .all(|span| span.style.fg == Some(SPINE_BRAND_COLOR))
            );
            assert!(
                spans
                    .iter()
                    .all(|span| !span.style.add_modifier.contains(Modifier::DIM))
            );
        }
        assert!(spine_brand_shimmer_text("", MotionMode::Animated).is_empty());
        assert!(spine_brand_shimmer_text("", MotionMode::Reduced).is_empty());
    }

    #[test]
    fn green_growth_marker_follows_cycle() {
        let green = motion_green_style();
        let dim_green = green.add_modifier(Modifier::DIM);
        let bold_green = green.add_modifier(Modifier::BOLD);
        let cases = [
            (0, ".", dim_green),
            (899, ".", dim_green),
            (900, ".", bold_green),
            (1_199, ".", bold_green),
            (1_200, "ʏ", dim_green),
            (1_359, "ʏ", dim_green),
            (1_360, "Ү", green),
            (1_519, "Ү", green),
            (1_520, "ϒ", bold_green),
            (2_419, "ϒ", bold_green),
            (2_420, "ϒ", green),
            (2_529, "ϒ", green),
            (2_530, "Ү", green),
            (2_614, "Ү", green),
            (2_615, "ʏ", dim_green),
            (2_699, "ʏ", dim_green),
            (2_700, ".", dim_green),
        ];

        for (elapsed_ms, glyph, style) in cases {
            assert_eq!(
                green_growth_marker(Duration::from_millis(elapsed_ms), MotionMode::Animated),
                Span::styled(glyph, style),
                "unexpected growth marker at {elapsed_ms} ms"
            );
        }
    }

    #[test]
    fn green_growth_marker_is_static_with_reduced_motion() {
        let expected = Span::styled("ϒ", motion_green_style());
        for elapsed_ms in [0, 1_520, 2_420, 2_699, 2_700] {
            assert_eq!(
                green_growth_marker(Duration::from_millis(elapsed_ms), MotionMode::Reduced),
                expected
            );
        }
    }

    #[test]
    fn green_growth_marker_glyphs_are_one_column_wide() {
        use unicode_width::UnicodeWidthStr;

        for glyph in [".", "ʏ", "Ү", "ϒ"] {
            assert_eq!(UnicodeWidthStr::width(glyph), 1, "{glyph}");
        }
    }

    #[test]
    fn animation_primitives_are_only_used_by_motion_module() {
        let direct_spinner = regex_lite::Regex::new(r"(^|[^A-Za-z0-9_])spinner\s*\(").unwrap();
        let direct_shimmer =
            regex_lite::Regex::new(r"(^|[^A-Za-z0-9_])shimmer_spans\s*\(").unwrap();
        let lib_rs = codex_utils_cargo_bin::find_resource!("src/lib.rs")
            .expect("failed to locate TUI source");
        let src_dir = lib_rs.parent().expect("lib.rs should have a parent");

        let mut source_files = Vec::new();
        collect_rust_files(src_dir, &mut source_files).expect("failed to collect TUI source files");

        let mut violations = Vec::new();
        for path in source_files {
            let relative_path = path
                .strip_prefix(src_dir)
                .expect("source file should be under src")
                .to_string_lossy()
                .replace('\\', "/");
            if animation_primitive_allowlisted_path(&relative_path) {
                continue;
            }

            let contents = fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {relative_path}: {err}"));
            for (line_number, line) in contents.lines().enumerate() {
                let code = line.split_once("//").map_or(line, |(code, _)| code);
                if direct_spinner.is_match(code) {
                    violations.push(format!(
                        "{relative_path}:{} contains a direct `spinner(...)` call; use crate::motion instead",
                        line_number + 1
                    ));
                }
                if direct_shimmer.is_match(code) {
                    violations.push(format!(
                        "{relative_path}:{} contains a direct `shimmer_spans(...)` call; use crate::motion instead",
                        line_number + 1
                    ));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "direct animation primitive usage found:\n{}",
            violations.join("\n")
        );
    }

    fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                collect_rust_files(&path, files)?;
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
        Ok(())
    }

    fn animation_primitive_allowlisted_path(relative_path: &str) -> bool {
        matches!(relative_path, "motion.rs" | "shimmer.rs")
    }
}

use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::color::blend;
use crate::product_brand::SPINE_BRAND_COLOR;
use crate::terminal_palette::default_bg;
use crate::terminal_palette::default_fg;

static PROCESS_START: OnceLock<Instant> = OnceLock::new();
const MOTION_GREEN_RGB: (u8, u8, u8) = (32, 160, 80);

fn elapsed_since_start() -> Duration {
    let start = PROCESS_START.get_or_init(Instant::now);
    start.elapsed()
}

pub(crate) fn shimmer_spans(text: &str) -> Vec<Span<'static>> {
    let base_color = default_fg().unwrap_or((128, 128, 128));
    let highlight_color = default_bg().unwrap_or((255, 255, 255));
    shimmer_spans_with_palette(
        text,
        base_color,
        highlight_color,
        ShimmerFallback::Intensity,
    )
}

pub(crate) fn green_shimmer_spans(text: &str) -> Vec<Span<'static>> {
    shimmer_spans_with_palette(
        text,
        MOTION_GREEN_RGB,
        (160, 255, 190),
        ShimmerFallback::Solid(Color::Green),
    )
}

pub(crate) fn green_then_default_shimmer_spans(
    green_text: &str,
    default_text: &str,
) -> Vec<Span<'static>> {
    let green_len = green_text.chars().count();
    let total_len = green_len + default_text.chars().count();
    if total_len == 0 {
        return Vec::new();
    }

    let pos = sweep_position(total_len);
    let mut spans = shimmer_spans_with_palette_at_position(
        green_text,
        /*offset*/ 0,
        pos,
        MOTION_GREEN_RGB,
        (160, 255, 190),
        ShimmerFallback::Solid(Color::Green),
    );
    spans.extend(shimmer_spans_with_palette_at_position(
        default_text,
        green_len,
        pos,
        default_fg().unwrap_or((128, 128, 128)),
        default_bg().unwrap_or((255, 255, 255)),
        ShimmerFallback::Intensity,
    ));
    spans
}

pub(crate) fn spine_brand_shimmer_spans(text: &str) -> Vec<Span<'static>> {
    shimmer_spans_with_style(text, spine_brand_style_for_intensity)
}

pub(crate) fn motion_green_style() -> Style {
    let color = if supports_color::on_cached(supports_color::Stream::Stdout)
        .map(|level| level.has_16m)
        .unwrap_or(false)
    {
        Color::Rgb(MOTION_GREEN_RGB.0, MOTION_GREEN_RGB.1, MOTION_GREEN_RGB.2)
    } else {
        Color::Green
    };
    Style::default().fg(color)
}

#[derive(Clone, Copy)]
enum ShimmerFallback {
    Intensity,
    Solid(Color),
}

fn shimmer_spans_with_palette(
    text: &str,
    base_color: (u8, u8, u8),
    highlight_color: (u8, u8, u8),
    fallback: ShimmerFallback,
) -> Vec<Span<'static>> {
    let text_len = text.chars().count();
    if text_len == 0 {
        return Vec::new();
    }

    shimmer_spans_with_palette_at_position(
        text,
        /*offset*/ 0,
        sweep_position(text_len),
        base_color,
        highlight_color,
        fallback,
    )
}

fn shimmer_spans_with_palette_at_position(
    text: &str,
    offset: usize,
    pos: usize,
    base_color: (u8, u8, u8),
    highlight_color: (u8, u8, u8),
    fallback: ShimmerFallback,
) -> Vec<Span<'static>> {
    let has_true_color = supports_color::on_cached(supports_color::Stream::Stdout)
        .map(|level| level.has_16m)
        .unwrap_or(false);

    shimmer_spans_with_style_at_position(text, offset, pos, |intensity| {
        if has_true_color {
            let highlight = intensity.clamp(0.0, 1.0);
            let (r, g, b) = blend(highlight_color, base_color, highlight * 0.9);
            #[allow(clippy::disallowed_methods)]
            let style = Style::default().fg(Color::Rgb(r, g, b));
            style.add_modifier(Modifier::BOLD)
        } else {
            fallback_style(intensity, fallback)
        }
    })
}

fn shimmer_spans_with_style(
    text: &str,
    style_for_intensity: impl Fn(f32) -> Style,
) -> Vec<Span<'static>> {
    let text_len = text.chars().count();
    if text_len == 0 {
        return Vec::new();
    }

    shimmer_spans_with_style_at_position(
        text,
        /*offset*/ 0,
        sweep_position(text_len),
        style_for_intensity,
    )
}

fn shimmer_spans_with_style_at_position(
    text: &str,
    offset: usize,
    pos: usize,
    style_for_intensity: impl Fn(f32) -> Style,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let padding = 10usize;
    let mut spans = Vec::with_capacity(chars.len());
    for (i, ch) in chars.iter().enumerate() {
        let intensity = band_intensity(offset + i, pos, padding);
        spans.push(Span::styled(ch.to_string(), style_for_intensity(intensity)));
    }
    spans
}

fn sweep_position(text_len: usize) -> usize {
    let padding = 10usize;
    let period = text_len + padding * 2;
    let sweep_seconds = 2.0f32;
    let pos_f =
        (elapsed_since_start().as_secs_f32() % sweep_seconds) / sweep_seconds * period as f32;
    pos_f as usize
}

fn band_intensity(index: usize, pos: usize, padding: usize) -> f32 {
    let dist = ((index + padding) as isize - pos as isize).unsigned_abs() as f32;
    let band_half_width = 5.0;
    if dist <= band_half_width {
        let x = std::f32::consts::PI * (dist / band_half_width);
        0.5 * (1.0 + x.cos())
    } else {
        0.0
    }
}

fn fallback_style(intensity: f32, fallback: ShimmerFallback) -> Style {
    match fallback {
        ShimmerFallback::Intensity => color_for_level(intensity),
        ShimmerFallback::Solid(color) => color_for_level(intensity).fg(color),
    }
}

fn color_for_level(intensity: f32) -> Style {
    // Tune fallback styling so the shimmer band reads even without RGB support.
    if intensity < 0.2 {
        Style::default().add_modifier(Modifier::DIM)
    } else if intensity < 0.6 {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn spine_brand_style_for_intensity(intensity: f32) -> Style {
    let style = Style::default().fg(SPINE_BRAND_COLOR);
    if intensity < 0.6 {
        style
    } else {
        style.add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
#[path = "shimmer_tests.rs"]
mod tests;

use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;
use std::cell::Cell;

use crate::ascii_animation::AsciiAnimation;
use crate::key_hint::KeyBindingListExt;
use crate::onboarding::keys;
use crate::onboarding::onboarding_screen::KeyboardHandler;
use crate::onboarding::onboarding_screen::StepStateProvider;
use crate::product_brand::ProductBrand;
use crate::tui::FrameRequester;

use super::onboarding_screen::StepState;

const MIN_ANIMATION_HEIGHT: u16 = 37;
const MIN_ANIMATION_WIDTH: u16 = 60;

pub(crate) struct WelcomeWidget {
    pub is_logged_in: bool,
    brand: ProductBrand,
    animation: AsciiAnimation,
    animations_enabled: bool,
    animations_suppressed: Cell<bool>,
    layout_area: Cell<Option<Rect>>,
}

impl KeyboardHandler for WelcomeWidget {
    /// Rotate the welcome animation when the fixed toggle shortcut fires.
    ///
    /// The key list includes compatibility variants for terminals that report
    /// modifier bits differently.
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if !self.animations_enabled {
            return;
        }
        if key_event.kind == KeyEventKind::Press && keys::TOGGLE_ANIMATION.is_pressed(key_event) {
            tracing::warn!("Welcome background to press '.'");
            let _ = self.animation.pick_random_variant();
        }
    }
}

impl WelcomeWidget {
    pub(crate) fn new(
        is_logged_in: bool,
        request_frame: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            is_logged_in,
            brand: ProductBrand::default(),
            animation: AsciiAnimation::new(request_frame),
            animations_enabled,
            animations_suppressed: Cell::new(false),
            layout_area: Cell::new(None),
        }
    }

    pub(crate) fn update_layout_area(&self, area: Rect) {
        self.layout_area.set(Some(area));
    }

    pub(crate) fn with_brand(mut self, brand: ProductBrand) -> Self {
        self.brand = brand;
        self
    }

    pub(crate) fn set_animations_suppressed(&self, suppressed: bool) {
        self.animations_suppressed.set(suppressed);
    }

    fn welcome_line(&self) -> Line<'static> {
        let mut spans = vec!["  ".into(), "Welcome to ".into()];
        match self.brand {
            ProductBrand::Codex => spans.extend([
                "Codex".bold(),
                ", OpenAI's command-line coding agent".into(),
            ]),
            ProductBrand::Spine => {
                spans.extend(["Spine".green().bold(), " Codex".bold()]);
            }
        }
        Line::from(spans)
    }
}

impl WidgetRef for &WelcomeWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        if self.animations_enabled && !self.animations_suppressed.get() {
            self.animation.schedule_next_frame();
        }

        let layout_area = self.layout_area.get().unwrap_or(area);
        // Skip the animation entirely when the viewport is too small so we don't clip frames.
        let show_animation = self.animations_enabled
            && !self.animations_suppressed.get()
            && layout_area.height >= MIN_ANIMATION_HEIGHT
            && layout_area.width >= MIN_ANIMATION_WIDTH;

        let mut lines: Vec<Line> = Vec::new();
        if show_animation {
            let frame = self.animation.current_frame();
            lines.extend(frame.lines().map(Into::into));
            lines.push("".into());
        }
        lines.push(self.welcome_line());

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

impl StepStateProvider for WelcomeWidget {
    fn get_step_state(&self) -> StepState {
        match self.is_logged_in {
            true => StepState::Hidden,
            false => StepState::Complete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyModifiers;
    use pretty_assertions::assert_eq;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    static VARIANT_A: [&str; 1] = ["frame-a"];
    static VARIANT_B: [&str; 1] = ["frame-b"];
    static VARIANTS: [&[&str]; 2] = [&VARIANT_A, &VARIANT_B];

    fn row_containing(buf: &Buffer, needle: &str) -> Option<u16> {
        (0..buf.area.height).find(|&y| {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            row.contains(needle)
        })
    }

    #[test]
    fn welcome_renders_animation_on_first_draw() {
        let widget = WelcomeWidget::new(
            /*is_logged_in*/ false,
            FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );
        let area = Rect::new(0, 0, MIN_ANIMATION_WIDTH, MIN_ANIMATION_HEIGHT);
        let mut buf = Buffer::empty(area);
        let frame_lines = widget.animation.current_frame().lines().count() as u16;
        (&widget).render(area, &mut buf);

        let welcome_row = row_containing(&buf, "Welcome");
        assert_eq!(welcome_row, Some(frame_lines + 1));
    }

    #[test]
    fn welcome_skips_animation_below_height_breakpoint() {
        let widget = WelcomeWidget::new(
            /*is_logged_in*/ false,
            FrameRequester::test_dummy(),
            /*animations_enabled*/ true,
        );
        let area = Rect::new(0, 0, MIN_ANIMATION_WIDTH, MIN_ANIMATION_HEIGHT - 1);
        let mut buf = Buffer::empty(area);
        (&widget).render(area, &mut buf);

        let welcome_row = row_containing(&buf, "Welcome");
        assert_eq!(welcome_row, Some(0));
    }

    #[test]
    fn welcome_brand_tracks_product_brand() {
        let codex = WelcomeWidget::new(
            /*is_logged_in*/ false,
            FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        );
        let codex_line = codex.welcome_line();
        assert_eq!(
            codex_line.to_string(),
            "  Welcome to Codex, OpenAI's command-line coding agent"
        );

        let spine = WelcomeWidget::new(
            /*is_logged_in*/ false,
            FrameRequester::test_dummy(),
            /*animations_enabled*/ false,
        )
        .with_brand(ProductBrand::Spine);
        let spine_line = spine.welcome_line();
        assert_eq!(spine_line.to_string(), "  Welcome to Spine Codex");
        assert_eq!(spine_line.spans[2].style.fg, Some(Color::Green));
    }

    #[test]
    fn ctrl_dot_changes_animation_variant() {
        let mut widget = WelcomeWidget {
            is_logged_in: false,
            brand: ProductBrand::default(),
            animation: AsciiAnimation::with_variants(
                FrameRequester::test_dummy(),
                &VARIANTS,
                /*variant_idx*/ 0,
            ),
            animations_enabled: true,
            animations_suppressed: Cell::new(false),
            layout_area: Cell::new(None),
        };

        let before = widget.animation.current_frame();
        widget.handle_key_event(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::CONTROL));
        let after = widget.animation.current_frame();

        assert_ne!(
            before, after,
            "expected ctrl+. to switch welcome animation variant"
        );
    }

    #[test]
    fn ctrl_shift_dot_changes_animation_variant() {
        let mut widget = WelcomeWidget {
            is_logged_in: false,
            brand: ProductBrand::default(),
            animation: AsciiAnimation::with_variants(
                FrameRequester::test_dummy(),
                &VARIANTS,
                /*variant_idx*/ 0,
            ),
            animations_enabled: true,
            animations_suppressed: Cell::new(false),
            layout_area: Cell::new(None),
        };

        let before = widget.animation.current_frame();
        widget.handle_key_event(KeyEvent::new(
            KeyCode::Char('.'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        let after = widget.animation.current_frame();

        assert_ne!(
            before, after,
            "expected ctrl+shift+. to switch welcome animation variant"
        );
    }
}

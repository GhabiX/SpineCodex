use pretty_assertions::assert_eq;
use ratatui::style::Modifier;
use ratatui::style::Style;

use super::*;

#[test]
fn segmented_shimmer_uses_global_indices_across_the_boundary() {
    let style_for_intensity = |intensity| {
        if intensity == 1.0 {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };

    let left = shimmer_spans_with_style_at_position(
        "abc",
        /*offset*/ 0,
        /*pos*/ 13,
        style_for_intensity,
    );
    let right = shimmer_spans_with_style_at_position(
        "de",
        /*offset*/ 3,
        /*pos*/ 13,
        style_for_intensity,
    );

    assert_eq!(
        left.iter()
            .map(|span| span.style.add_modifier)
            .collect::<Vec<_>>(),
        vec![Modifier::empty(); 3]
    );
    assert_eq!(
        right
            .iter()
            .map(|span| span.style.add_modifier)
            .collect::<Vec<_>>(),
        vec![Modifier::BOLD, Modifier::empty()]
    );
}

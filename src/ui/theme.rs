//! Brand palette, taken from the gradient stops of `assets/abacus-mark.svg` so the
//! TUI chrome matches the mark. Bead status colours stay semantic (they mirror
//! `bd graph`) and live on `model::Derived`, not here.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

pub const CORAL: Color = Color::Rgb(0xFD, 0x5A, 0x6F);
pub const MAGENTA: Color = Color::Rgb(0xFF, 0x4F, 0xF0);
pub const VIOLET: Color = Color::Rgb(0xAF, 0x80, 0xFD);
pub const CYAN: Color = Color::Rgb(0x00, 0xE5, 0xFF);
/// Selected rows and cards: the cool violet glow behind the mark.
pub const SELECTION: Color = Color::Rgb(0x34, 0x1E, 0x4A);

/// Mark gradient as `(position, rgb)` stops, left to right.
const GRADIENT: [(f32, (u8, u8, u8)); 4] = [
    (0.0, (0xFD, 0x5A, 0x6F)),
    (0.5, (0xFF, 0x4F, 0xF0)),
    (0.72, (0xAF, 0x80, 0xFD)),
    (1.0, (0x00, 0xE5, 0xFF)),
];

pub fn gradient_at(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (mut lo, mut hi) = (GRADIENT[0], GRADIENT[GRADIENT.len() - 1]);
    for pair in GRADIENT.windows(2) {
        if t >= pair[0].0 && t <= pair[1].0 {
            (lo, hi) = (pair[0], pair[1]);
            break;
        }
    }
    let span = (hi.0 - lo.0).max(f32::EPSILON);
    let f = (t - lo.0) / span;
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
    Color::Rgb(
        mix(lo.1.0, hi.1.0),
        mix(lo.1.1, hi.1.1),
        mix(lo.1.2, hi.1.2),
    )
}

/// `text` with each character coloured along the mark's gradient.
pub fn wordmark(text: &str) -> Vec<Span<'static>> {
    let n = text.chars().count().max(2) - 1;
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            Span::styled(
                c.to_string(),
                Style::new()
                    .fg(gradient_at(i as f32 / n as f32))
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

pub fn heading() -> Style {
    Style::new().fg(CORAL).add_modifier(Modifier::BOLD)
}

pub fn selected() -> Style {
    Style::new().bg(SELECTION).add_modifier(Modifier::BOLD)
}

pub fn border(focused: bool) -> Style {
    Style::new().fg(if focused { MAGENTA } else { Color::DarkGray })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_hits_its_stops_and_clamps() {
        assert_eq!(gradient_at(0.0), CORAL);
        assert_eq!(gradient_at(0.5), MAGENTA);
        assert_eq!(gradient_at(1.0), CYAN);
        assert_eq!(gradient_at(-1.0), CORAL);
        assert_eq!(gradient_at(2.0), CYAN);
    }
}

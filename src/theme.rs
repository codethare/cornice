//! Theme: the palette and the ratios derived from bar height.

use crate::geom::Color;
use crate::text::TextStyle;

/// Card width bounds; the derived width is a multiple of the bar height in between.
pub const MIN_CARD_W: i32 = 80;
pub const MAX_CARD_W: i32 = 420;
/// Apple does not publish macOS notification-banner dimensions. Keep cornice's existing 10× internal ratio;
/// a fixed width gives every card in the vertical column the same optical span.
const CARD_W_PER_HEIGHT: i32 = 10;
/// A soft, continuous-cornered desktop banner rather than a bar-height capsule.
const CARD_RADIUS_PER_HEIGHT: f32 = 1.2;

#[derive(Clone)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub accent: Color,
    pub font: TextStyle,
    /// Supporting text: `foreground` at 72% alpha. Apple's hierarchy puts the notification's title in the primary
    /// label and its body in the secondary one; a semibold face was not usable here — the requested weight resolved
    /// outside the configured family (measured: "Card 6" went from 33.4 px to 53.6 px in `monospace`), which breaks
    /// the monospace grid instead of emphasising the title.
    pub secondary: Color,
    pub height: i32,
    pub padding: i32,
    pub spacing: i32,
    /// Gap between notification cards, default height/5
    pub card_gap: i32,
    /// Notification card padding, default height/2
    pub card_padding: i32,
    /// Notification card width and corner radius (see the constants above).
    pub card_w: i32,
    pub card_radius: i32,
}

impl Theme {
    /// A short desktop banner still needs enough vertical room for one balanced line.
    pub fn card_min_h(&self) -> i32 {
        let line = (self.font.size * 1.35).ceil() as i32;
        (self.height * 5 / 2).max(line + self.card_padding * 2)
    }

    pub fn defaults(height: i32) -> Self {
        let foreground = Color::rgba(0xdc, 0xdc, 0xdc, 0xff);
        Self {
            background: Color::rgba(0x1a, 0x1a, 0x1a, 0xee),
            foreground,
            secondary: Color::rgba(foreground.r, foreground.g, foreground.b, (foreground.a as f32 * 0.72) as u8),
            accent: Color::rgba(0x88, 0xc0, 0xd0, 0xff),
            font: TextStyle::new(11.0, "monospace"),
            height,
            padding: 8,
            spacing: 6,
            card_gap: (height / 5).max(2),
            card_padding: (height / 2).max(4),
            card_w: (CARD_W_PER_HEIGHT * height).clamp(MIN_CARD_W, MAX_CARD_W),
            card_radius: (height as f32 * CARD_RADIUS_PER_HEIGHT).round() as i32,
        }
    }
}

/// `"Inter 11"` → `("Inter", 11.0)`; falls back to the default size when there is no number.
pub fn parse_font(s: &str, fallback_size: f32) -> TextStyle {
    let mut parts = s.rsplit_once(' ');
    match parts.as_mut() {
        Some((family, size)) if size.parse::<f32>().is_ok() => TextStyle::new(size.parse().unwrap(), family.trim()),
        _ => TextStyle::new(fallback_size, s.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_parsing() {
        let f = parse_font("Inter 11", 10.0);
        assert_eq!(f.family, "Inter");
        assert_eq!(f.size, 11.0);
        let f = parse_font("Noto Sans CJK SC 13.5", 10.0);
        assert_eq!(f.family, "Noto Sans CJK SC");
        assert_eq!(f.size, 13.5);
        let f = parse_font("monospace", 10.0);
        assert_eq!(f.family, "monospace");
        assert_eq!(f.size, 10.0);
    }

    #[test]
    fn derived_proportions() {
        let t = Theme::defaults(30);
        assert_eq!(t.card_gap, 6);
        assert_eq!(t.card_padding, 15);
        // The card ratios are fixed cornice proportions rather than content-derived dimensions.
        assert_eq!(t.card_w, 300);
        assert_eq!(t.card_radius, 36);
        assert_eq!(t.card_min_h(), 75);
        assert!(t.secondary.a < t.foreground.a, "the body is the secondary label, not the primary one");
        // The derived values stay inside their bounds on a tall bar, and stay usable on a short one.
        assert_eq!(Theme::defaults(120).card_w, MAX_CARD_W);
        assert_eq!(Theme::defaults(4).card_w, MIN_CARD_W);
    }
}

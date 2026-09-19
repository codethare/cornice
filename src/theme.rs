//! Theme: the palette and the ratios derived from bar height.

use crate::geom::Color;
use crate::text::TextStyle;

#[derive(Clone)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub accent: Color,
    pub font: TextStyle,
    pub height: i32,
    pub radius: i32,
    pub padding: i32,
    pub spacing: i32,
    /// Gap between notification cards, default height/5
    pub card_gap: i32,
    /// Notification card padding, default height/2
    pub card_padding: i32,
}

impl Theme {
    pub fn defaults(height: i32) -> Self {
        Self {
            background: Color::rgba(0x1a, 0x1a, 0x1a, 0xee),
            foreground: Color::rgba(0xdc, 0xdc, 0xdc, 0xff),
            accent: Color::rgba(0x88, 0xc0, 0xd0, 0xff),
            font: TextStyle::new(11.0, "monospace"),
            height,
            radius: height / 2,
            padding: 8,
            spacing: 6,
            card_gap: (height / 5).max(2),
            card_padding: (height / 2).max(4),
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
        assert_eq!(t.radius, 15);
        assert_eq!(t.card_gap, 6);
        assert_eq!(t.card_padding, 15);
    }
}

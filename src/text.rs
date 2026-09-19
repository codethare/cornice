//! cosmic-text wrapper: text shaping, rasterisation and width-based truncation.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache};

use crate::canvas::Canvas;
use crate::geom::Color;

#[derive(Clone, Default)]
pub struct TextStyle { pub size: f32, pub family: String }

impl TextStyle {
    pub fn new(size: f32, family: impl Into<String>) -> Self {
        Self { size, family: family.into() }
    }
}

pub struct TextEngine {
    font_system: FontSystem,
    swash_cache: SwashCache,
}

impl Default for TextEngine { fn default() -> Self { Self::new() } }

impl TextEngine {
    pub fn new() -> Self {
        Self { font_system: FontSystem::new(), swash_cache: SwashCache::new() }
    }

    /// The returned `Attrs` borrows `style` rather than self — otherwise it would hold an immutable borrow of self until
    /// would conflict with `buffer.borrow_with(&mut self.font_system)` once the immutable borrow of `font_system` ends.
    fn attrs(style: &TextStyle) -> Attrs<'_> {
        if style.family.is_empty() {
            Attrs::new()
        } else {
            Attrs::new().family(Family::Name(style.family.as_str()))
        }
    }

    fn line_height(style: &TextStyle) -> f32 { (style.size * 1.35).ceil() }

    /// Returns (text width, line height).
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> (f32, f32) {
        let line = Self::line_height(style);
        if text.is_empty() {
            return (0.0, line);
        }
        let attrs = Self::attrs(style);
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(style.size, line));
        let mut buffer = buffer.borrow_with(&mut self.font_system);
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        let mut w: f32 = 0.0;
        for run in buffer.layout_runs() {
            w = w.max(run.line_w);
        }
        (w, line)
    }

    /// `top` is the top edge of the text box; text is drawn within `y = top .. top+line_height`.
    pub fn draw(&mut self, canvas: &mut Canvas, text: &str, x: i32, top: i32, style: &TextStyle, color: Color) {
        if text.is_empty() {
            return;
        }
        let line = Self::line_height(style);
        let attrs = Self::attrs(style);
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(style.size, line));
        let mut buffer = buffer.borrow_with(&mut self.font_system);
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        // The baseline sits about 0.75 of the line height below the box top, close enough for a flat horizontal bar
        let baseline = top + (line * 0.75) as i32;
        buffer.draw(&mut self.swash_cache, cosmic_text::Color::rgba(color.r, color.g, color.b, color.a), |gx, gy, gw, gh, gc| {
            // The colour the callback hands back carries the pixel's coverage alpha; scale the caller's colour by it, then blend the whole block
            let a = gc.a();
            if a == 0 {
                return;
            }
            let c = Color::rgba(color.r, color.g, color.b, (color.a as u32 * a as u32 / 255) as u8);
            canvas.fill_rect(crate::geom::Rect::new(x + gx, baseline + gy, gw as i32, gh as i32), c);
        });
    }
}

/// Chops characters off the end and appends `…` when too wide; the measure function is injected for unit tests.
pub fn truncate_to_width(text: &str, max_w: f32, mut measure: impl FnMut(&str) -> f32) -> String {
    if text.is_empty() || max_w <= 0.0 {
        return String::new();
    }
    if measure(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut end = chars.len();
    while end > 0 {
        end -= 1;
        let candidate: String = chars[..end].iter().collect::<String>() + "…";
        if measure(&candidate) <= max_w {
            return candidate;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake measure function that counts every char as 10px wide, decoupling truncation logic from fonts.
    fn fake(text: &str) -> f32 { text.chars().count() as f32 * 10.0 }

    #[test]
    fn truncate_keeps_short_text() {
        assert_eq!(truncate_to_width("abc", 100.0, fake), "abc");
        assert_eq!(truncate_to_width("abc", 30.0, fake), "abc");
    }

    #[test]
    fn truncate_adds_ellipsis_within_budget() {
        let out = truncate_to_width("abcdefghij", 35.0, fake);
        assert!(out.ends_with('…'), "{out}");
        assert!(fake(&out) <= 35.0, "{out} width {}", fake(&out));
        assert_eq!(out, "ab…");
    }

    #[test]
    fn truncate_degenerate_budget() {
        assert_eq!(truncate_to_width("abcdef", 0.0, fake), "");
        assert_eq!(truncate_to_width("", 50.0, fake), "");
    }
}

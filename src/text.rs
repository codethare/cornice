//! cosmic-text wrapper: text shaping, rasterisation and width-based truncation.

use std::collections::HashMap;

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

/// Where the capital letters actually sit inside the line box, in pixels.
///
/// A line box is `line_height` tall whatever the string is, but the ink inside it is not: the leading
/// above the cap and the descent below the baseline are empty for a string like `09:19`. Padding the
/// box and centring the box therefore put the ink off by `(line_height - cap) / 2` — the classic "a
/// label in a button looks low" defect. CSS now fixes this with `text-box: trim-both cap alphabetic`;
/// these metrics are the same trim, applied by hand (cap height is 65–75% of the em, so the error is
/// 2–4 px at UI sizes): https://developer.mozilla.org/en-US/docs/Web/CSS/text-box-trim
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CapMetrics {
    /// Distance from the line-box top to the top of a capital `H`.
    pub top: f32,
    /// Cap height (the `H` ink height).
    pub cap: f32,
}

pub struct TextEngine {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// Measured once per (family, size): rasterising `H` is the only reliable way to learn the ink
    /// offsets, and it is done on the first frame that needs them.
    cap_cache: HashMap<(String, u32), CapMetrics>,
    /// Advance width of `…` per (family, size); every truncated string needs it.
    ellipsis_cache: HashMap<(String, u32), f32>,
}

impl Default for TextEngine { fn default() -> Self { Self::new() } }

impl TextEngine {
    pub fn new() -> Self {
        Self { font_system: FontSystem::new(), swash_cache: SwashCache::new(), cap_cache: HashMap::new(), ellipsis_cache: HashMap::new() }
    }

    /// Rasterise a single `H` into a scratch canvas and read the ink rows back out.
    fn measure_cap(&mut self, style: &TextStyle) -> CapMetrics {
        let line = Self::line_height(style);
        let (w, _) = self.measure("H", style);
        let pw = (w.ceil() as i32 + 4).max(4);
        let ph = (line.ceil() as i32 + 4).max(4);
        let mut data = vec![0u8; (pw * ph * 4) as usize];
        {
            let mut canvas = Canvas::new(&mut data, pw, ph);
            // Drawn at y = 1 so a glyph whose ink starts on the very first row is still distinguishable from "no ink".
            self.draw(&mut canvas, "H", 1, 1, style, Color::rgba(0xff, 0xff, 0xff, 0xff));
        }
        let mut top = None;
        let mut bottom = 0;
        for y in 0..ph {
            for x in 0..pw {
                if data[((y * pw + x) * 4) as usize + 3] != 0 {
                    top.get_or_insert(y);
                    bottom = y;
                }
            }
        }
        match top {
            Some(t) => CapMetrics { top: (t - 1) as f32, cap: (bottom - t + 1) as f32 },
            // No font at all: fall back to the box ratios rather than panicking.
            None => CapMetrics { top: (line - style.size) / 2.0, cap: style.size },
        }
    }

    pub fn cap_metrics(&mut self, style: &TextStyle) -> CapMetrics {
        let key = (style.family.clone(), style.size.to_bits());
        if let Some(m) = self.cap_cache.get(&key) {
            return *m;
        }
        let m = self.measure_cap(style);
        self.cap_cache.insert(key, m);
        m
    }

    /// The `top` to pass to `draw` so the *cap height* is centred in `box_y .. box_y + box_h`,
    /// instead of the line box (which carries ascender and descender space the ink never uses).
    pub fn optical_top(&mut self, style: &TextStyle, box_y: i32, box_h: i32) -> i32 {
        let m = self.cap_metrics(style);
        let centre = box_y as f32 + box_h as f32 / 2.0;
        (centre - m.cap / 2.0 - m.top).round() as i32
    }

    fn ellipsis_width(&mut self, style: &TextStyle) -> f32 {
        let key = (style.family.clone(), style.size.to_bits());
        if let Some(width) = self.ellipsis_cache.get(&key) {
            return *width;
        }
        let width = self.measure("…", style).0;
        self.ellipsis_cache.insert(key, width);
        width
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
        // cosmic-text's draw callback already reports each glyph image relative to the text box top (its own
        // baseline included), so `top` is used as-is: adding 0.75*line here drew every string one baseline too
        // low — the bar's text 2px off the bottom edge, and the card's action pill on top of the last body line.
        let baseline = top;
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

    /// Chops characters off the end and appends `…` when too wide. One shaping pass, then the glyph
    /// x positions decide the cut: re-shaping every candidate string (the obvious loop) costs ~8 ms per
    /// notification card per frame, and an enter animation draws every frame.
    pub fn fit_text(&mut self, text: &str, style: &TextStyle, max_w: f32) -> String {
    if text.is_empty() || max_w <= 0.0 {
        return String::new();
    }
    // Measured before the buffer exists: `borrow_with` holds a mutable borrow of the font system.
    let ellipsis = self.ellipsis_width(style);
    let budget = max_w - ellipsis;
    if budget <= 0.0 {
        return if ellipsis <= max_w { "…".to_string() } else { String::new() };
    }
    let line = Self::line_height(style);
    let attrs = Self::attrs(style);
    let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(style.size, line));
    let mut buffer = buffer.borrow_with(&mut self.font_system);
    buffer.set_size(None, None);
    buffer.set_text(text, &attrs, Shaping::Advanced, None);

    let mut cut = text.len();
    let mut over = false;
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            if glyph.x + glyph.w > budget {
                cut = glyph.start;
                over = true;
                break;
            }
        }
        if over {
            break;
        }
    }
    if !over {
        return text.to_string();
    }
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = text[..cut].to_string();
    out.push('…');
    out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> (TextEngine, TextStyle) { (TextEngine::new(), TextStyle::new(11.0, "monospace")) }

    #[test]
    fn short_text_is_returned_unchanged() {
        let (mut e, style) = engine();
        assert_eq!(e.fit_text("abc", &style, 100.0), "abc");
        assert_eq!(e.fit_text("", &style, 100.0), "");
        assert_eq!(e.fit_text("abc", &style, 0.0), "");
    }

    #[test]
    fn long_text_is_cut_to_the_width_with_an_ellipsis() {
        let (mut e, style) = engine();
        let long = "abcdefghij".repeat(6);
        let budget = 60.0;
        let out = e.fit_text(&long, &style, budget);
        assert!(out.ends_with('…'), "{out}");
        assert!(out.len() < long.len(), "{out}");
        assert!(long.starts_with(out.trim_end_matches('…')), "the kept prefix must be a prefix: {out}");
        assert!(e.measure(&out, &style).0 <= budget, "{out} is wider than {budget}");
    }

    #[test]
    fn a_budget_smaller_than_the_ellipsis_still_returns_what_fits() {
        let (mut e, style) = engine();
        let ellipsis = e.measure("…", &style).0;
        assert_eq!(e.fit_text("abcdef", &style, ellipsis - 0.5), "");
        assert_eq!(e.fit_text("abcdef", &style, ellipsis + 0.5), "…");
    }

    /// The point of the cap metrics: whatever the font, the *ink* ends up centred in the box.
    #[test]
    fn optical_top_centres_the_cap_height() {
        let (mut e, style) = engine();
        let m = e.cap_metrics(&style);
        assert!(m.cap > 0.0, "a cap height must be measurable: {m:?}");
        for box_h in [16, 30, 40] {
            let top = e.optical_top(&style, 0, box_h);
            let ink = top as f32 + m.top;
            let cap_centre = ink + m.cap / 2.0;
            assert!(
                (cap_centre - box_h as f32 / 2.0).abs() <= 0.5,
                "box {box_h}: cap centre {cap_centre} should be at {}",
                box_h as f32 / 2.0
            );
        }
    }
}


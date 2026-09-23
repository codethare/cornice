//! Software rasteriser over a wl_shm byte buffer: rounded rects and alpha blending.
//! Every write is src-over, because several rounded rects stack on one surface.
//! Corners and edges are not anti-aliased (hard edges); switch to 4x supersampling if softer edges are wanted later.

use crate::geom::{Color, Rect};

pub struct Canvas<'a> {
    data: &'a mut [u8],
    width: i32,
    height: i32,
    clip: Option<Rect>,
}

impl<'a> Canvas<'a> {
    pub fn new(data: &'a mut [u8], width: i32, height: i32) -> Self {
        Self { data, width, height, clip: None }
    }

    /// Transparent pixels the surface does not cover must be cleared explicitly, or shm keeps the previous frame's leftovers.
    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    /// Set the clip rect; `None` means no clipping.
    /// While a notification card enters, the rect is smaller than the laid-out text, so text pixels are clipped to the card rect (design §7).
    pub fn set_clip(&mut self, clip: Option<Rect>) {
        self.clip = clip;
    }

    fn blend_px(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        if let Some(c) = self.clip {
            if !c.contains(x, y) {
                return;
            }
        }
        let i = ((y * self.width + x) * 4) as usize;
        let src = color.to_shm_bytes();
        if src[3] == 0 {
            return;
        }
        // The buffer holds premultiplied values, so src-over is dst = src + dst * (1 - src_a).
        // The alpha channel uses the same formula, so the four channels need no branching.
        let inv = 255 - src[3] as u32;
        for k in 0..4 {
            let d = self.data[i + k] as u32;
            let s = src[k] as u32;
            self.data[i + k] = (s + d * inv / 255).min(255) as u8;
        }
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                self.blend_px(x, y, color);
            }
        }
    }

    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: i32, color: Color) {
        if rect.is_empty() {
            return;
        }
        // the radius must not exceed half the short side, or the shape self-intersects
        let r = radius.clamp(0, rect.w.min(rect.h) / 2);
        if r == 0 {
            return self.fill_rect(rect, color);
        }
        // Judged by the distance from the pixel centre to the nearest corner centre; coordinates are clamped into the inner rect,
        // Outside the corner regions dx/dy are naturally 0, so no extra branch is needed. The corner itself is a
        // continuous (squircle) corner — see `CORNER_EXPONENT` — not a circular arc.
        let rf = r as f32;
        let n = crate::geom::CORNER_EXPONENT;
        let inside = |x: i32, y: i32| -> bool {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let cx = px.clamp(rect.x as f32 + rf, rect.right() as f32 - rf);
            let cy = py.clamp(rect.y as f32 + rf, rect.bottom() as f32 - rf);
            let dx = ((px - cx) / rf).abs().powf(n);
            let dy = ((py - cy) / rf).abs().powf(n);
            dx + dy <= 1.0
        };
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                if inside(x, y) {
                    self.blend_px(x, y, color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Color, Rect};

    fn canvas(w: i32, h: i32) -> Vec<u8> { vec![0u8; (w * h * 4) as usize] }

    fn px(data: &[u8], w: i32, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    #[test]
    fn fill_rect_writes_and_clamps() {
        let mut buf = canvas(4, 4);
        // Each draw keeps its `&mut buf` borrow of `Canvas` inside one block,
        // otherwise the later `px(&buf, ..)` conflicts with the still-live canvas borrow (E0502).
        {
            let mut c = Canvas::new(&mut buf, 4, 4);
            c.fill_rect(Rect::new(1, 1, 2, 2), Color::rgba(255, 0, 0, 255));
        }
        assert_eq!(px(&buf, 4, 1, 1), [0, 0, 255, 255]);
        assert_eq!(px(&buf, 4, 0, 0), [0, 0, 0, 0]);
        {
            let mut c = Canvas::new(&mut buf, 4, 4);
            // out of bounds does not panic
            c.fill_rect(Rect::new(-5, -5, 100, 100), Color::rgba(0, 255, 0, 255));
        }
        assert_eq!(px(&buf, 4, 3, 3), [0, 255, 0, 255]);
    }

    #[test]
    fn rounded_rect_leaves_corners_transparent() {
        let mut buf = canvas(8, 8);
        let mut c = Canvas::new(&mut buf, 8, 8);
        c.fill_rounded_rect(Rect::new(0, 0, 8, 8), 4, Color::rgba(255, 255, 255, 255));
        assert_eq!(px(&buf, 8, 0, 0), [0, 0, 0, 0], "the top-left corner should be empty");
        assert_eq!(px(&buf, 8, 7, 7), [0, 0, 0, 0], "the bottom-right corner should be empty");
        assert_eq!(px(&buf, 8, 4, 4), [255, 255, 255, 255], "the centre should be solid");
        assert_eq!(px(&buf, 8, 0, 4), [255, 255, 255, 255], "the left edge midpoint should be solid");
    }

    /// The corner is a continuous (superellipse) curve, not a circular arc: it bulges past where a circle of the
    /// same radius would sit, which is the difference between "one soft object" and "a square with cut corners".
    #[test]
    fn corners_are_continuous_not_circular() {
        let mut buf = canvas(8, 8);
        {
            let mut c = Canvas::new(&mut buf, 8, 8);
            c.fill_rounded_rect(Rect::new(0, 0, 8, 8), 4, Color::rgba(255, 255, 255, 255));
        }
        // Circle: dx = 2.5, dy = 3.5 from the corner centre (4, 4) → 6.25 + 12.25 = 18.5 > 16, outside.
        // Superellipse with n = 4: 2.5⁴ + 3.5⁴ = 39 + 150 = 189 ≤ 256, inside.
        assert_eq!(px(&buf, 8, 1, 0), [255, 255, 255, 255], "the shoulder must follow the superellipse, not the arc");
        assert_eq!(px(&buf, 8, 0, 0), [0, 0, 0, 0], "the corner tip stays empty");
        assert_eq!(px(&buf, 8, 4, 0), [255, 255, 255, 255], "the edge keeps its full extent");
    }

    #[test]
    fn blend_accumulates_alpha() {
        let mut buf = canvas(2, 2);
        let mut c = Canvas::new(&mut buf, 2, 2);
        c.fill_rect(Rect::new(0, 0, 1, 1), Color::rgba(255, 255, 255, 255));
        c.fill_rect(Rect::new(0, 0, 1, 1), Color::rgba(0, 0, 0, 128));
        let [b, g, r, a] = px(&buf, 2, 0, 0);
        assert_eq!(a, 255);
        // premultiplied src-over:0 + 255 * (1 - 128/255) = 127
        for (name, c) in [("r", r), ("g", g), ("b", b)] {
            assert!((c as i32 - 127).abs() <= 1, "{name} should be about 127, got {c}");
        }
    }
}

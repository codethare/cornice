//! Geometry and colour primitives.

/// Pixel rect in surface-local coordinates, origin top-left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect { pub x: i32, pub y: i32, pub w: i32, pub h: i32 }

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self { Self { x, y, w, h } }
    pub const fn right(&self) -> i32 { self.x + self.w }
    pub const fn bottom(&self) -> i32 { self.y + self.h }
    pub const fn is_empty(&self) -> bool { self.w <= 0 || self.h <= 0 }
    /// Overlap of two rects; an empty rect when they do not touch.
    pub fn intersect(&self, o: Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        Rect::new(x, y, self.right().min(o.right()) - x, self.bottom().min(o.bottom()) - y)
    }
    pub const fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

/// Continuous ("squircle") corner exponent: Apple's default corner curve is the superellipse
/// `|x|ⁿ + |y|ⁿ = rⁿ` with n > 2 (`UICornerCurve.continuous`), whose curvature reaches zero where it meets the
/// straight edge. A circular arc meets it at a tangent point instead, which is why a normal rounded rectangle
/// reads as "a square with the corners cut off" rather than as one object. n = 4 is Figma's "corner smoothing
/// 0.6 ≈ the iOS shape"; the value belongs to the shape language, so it is a constant and not a knob.
pub const CORNER_EXPONENT: f32 = 4.0;

/// Straight (non-premultiplied) alpha RGBA.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color { pub r: u8, pub g: u8, pub b: u8, pub a: u8 }

/// Premultiply and round to 0..=255. A separate `const fn`, since closures cannot be called from a `const fn`.
const fn premul(c: u8, a: u8) -> u8 { ((c as u32 * a as u32 + 127) / 255) as u8 }

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self { Self { r, g, b, a } }

    /// Accepts `#rgb` / `#rrggbb` / `#rrggbbaa` (the hash may be omitted).
    pub fn from_hex(s: &str) -> Result<Color, String> {
        let h = s.strip_prefix('#').unwrap_or(s);
        let bad = || format!("bad colour format: {s:?} (expected #rgb / #rrggbb / #rrggbbaa)");
        // h.len() is a byte length: non-ASCII input (e.g. "€", three bytes) takes the 3 branch but slices at an invalid char boundary and panics.
        // non-ASCII must be rejected before slicing (final review Important #2).
        if !h.is_ascii() {
            return Err(bad());
        }
        let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| bad());
        match h.len() {
            3 => {
                let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).map(|v| v * 17).map_err(|_| bad());
                Ok(Color::rgba(d(0)?, d(1)?, d(2)?, 255))
            }
            6 => Ok(Color::rgba(byte(0)?, byte(2)?, byte(4)?, 255)),
            8 => Ok(Color::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => Err(bad()),
        }
    }

    /// wl_shm Argb8888 requires premultiplied alpha, stored as B,G,R,A.
    pub const fn to_shm_bytes(self) -> [u8; 4] {
        [premul(self.b, self.a), premul(self.g, self.a), premul(self.r, self.a), self.a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_and_is_empty() {
        let a = Rect::new(0, 0, 10, 10);
        assert!(a.contains(0, 0));
        assert!(a.contains(9, 9));
        assert!(!a.contains(10, 0));
        assert!(!a.contains(0, 10));
        assert!(Rect::new(0, 0, 0, 10).is_empty());
    }

    #[test]
    fn color_from_hex_forms() {
        assert_eq!(Color::from_hex("#1a1a1aee").unwrap(), Color::rgba(0x1a, 0x1a, 0x1a, 0xee));
        assert_eq!(Color::from_hex("#fff").unwrap(), Color::rgba(255, 255, 255, 255));
        assert_eq!(Color::from_hex("#88c0d0").unwrap(), Color::rgba(0x88, 0xc0, 0xd0, 255));
        assert!(Color::from_hex("#12345").is_err());
        assert!(Color::from_hex("red").is_err());
        assert!(Color::from_hex("#gggggg").is_err());
    }

    #[test]
    fn color_from_hex_rejects_non_ascii() {
        // non-ASCII input must not panic (final review Important #2); it must return a Result error.
        assert!(Color::from_hex("€").is_err());
        assert!(Color::from_hex("€").is_err());
        assert!(Color::from_hex("#€€").is_err());
    }

    #[test]
    fn premultiplied_byte_order() {
        // premultiplied byte order is B,G,R,A; premultiplication rounds (+127/255)
        assert_eq!(Color::rgba(0x11, 0x22, 0x33, 0xff).to_shm_bytes(), [0x33, 0x22, 0x11, 0xff]);
        assert_eq!(Color::rgba(0x40, 0x40, 0x40, 0x80).to_shm_bytes(), [0x20, 0x20, 0x20, 0x80]);
    }

    #[test]
    fn rect_intersect() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(a.intersect(Rect::new(5, 5, 10, 10)), Rect::new(5, 5, 5, 5));
        assert_eq!(a.intersect(Rect::new(2, 2, 1, 1)), Rect::new(2, 2, 1, 1));
        assert!(a.intersect(Rect::new(20, 0, 5, 5)).is_empty(), "disjoint rects give an empty rect");
        assert_eq!(a.intersect(a), a);
    }
}

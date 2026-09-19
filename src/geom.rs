//! Geometry and colour primitives.

/// Pixel rect in surface-local coordinates, origin top-left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect { pub x: i32, pub y: i32, pub w: i32, pub h: i32 }

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self { Self { x, y, w, h } }
    pub const fn right(&self) -> i32 { self.x + self.w }
    pub const fn bottom(&self) -> i32 { self.y + self.h }
    pub const fn is_empty(&self) -> bool { self.w <= 0 || self.h <= 0 }
    pub const fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    /// The smallest rect enclosing both; returns the other when one is empty.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() { return *other; }
        if other.is_empty() { return *self; }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }
    pub fn lerp(a: Rect, b: Rect, t: f32) -> Rect {
        let l = |x: i32, y: i32| x + ((y - x) as f32 * t).round() as i32;
        Rect::new(l(a.x, b.x), l(a.y, b.y), l(a.w, b.w), l(a.h, b.h))
    }
}

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

    /// src-over blend (straight alpha version).
    pub fn blend_over(self, dst: Color) -> Color {
        if self.a == 255 { return self; }
        if self.a == 0 { return dst; }
        let sa = self.a as u32;
        let da = dst.a as u32;
        let mix = |s: u8, d: u8| ((s as u32 * sa + d as u32 * da * (255 - sa) / 255) / 255) as u8;
        Color::rgba(mix(self.r, dst.r), mix(self.g, dst.g), mix(self.b, dst.b), (sa + da * (255 - sa) / 255) as u8)
    }

    pub fn lerp(a: Color, b: Color, t: f32) -> Color {
        let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
        Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
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
    fn rect_contains_and_union() {
        let a = Rect::new(0, 0, 10, 10);
        assert!(a.contains(0, 0));
        assert!(a.contains(9, 9));
        assert!(!a.contains(10, 0));
        assert!(!a.contains(0, 10));
        assert_eq!(a.union(&Rect::new(20, 20, 5, 5)), Rect::new(0, 0, 25, 25));
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
    fn blend_and_premultiply() {
        // 50% transparent white over pure black = half-bright grey
        let out = Color::rgba(255, 255, 255, 128).blend_over(Color::rgba(0, 0, 0, 255));
        assert!((out.r as i32 - 128).abs() <= 1, "{out:?}");
        assert_eq!(out.a, 255);
        // an opaque colour overwrites directly
        assert_eq!(Color::rgba(1, 2, 3, 255).blend_over(Color::rgba(9, 9, 9, 255)), Color::rgba(1, 2, 3, 255));
        // premultiplied byte order is B,G,R,A
        assert_eq!(Color::rgba(0x11, 0x22, 0x33, 0xff).to_shm_bytes(), [0x33, 0x22, 0x11, 0xff]);
        assert_eq!(Color::rgba(0x40, 0x40, 0x40, 0x80).to_shm_bytes(), [0x20, 0x20, 0x20, 0x80]);
    }

    #[test]
    fn lerp_endpoints_and_monotonic() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(100, 50, 200, 30);
        assert_eq!(Rect::lerp(a, b, 0.0), a);
        assert_eq!(Rect::lerp(a, b, 1.0), b);
        let mid = Rect::lerp(a, b, 0.5);
        assert_eq!(mid.x, 50);
        assert_eq!(Color::lerp(Color::rgba(0, 0, 0, 255), Color::rgba(255, 255, 255, 255), 1.0), Color::rgba(255, 255, 255, 255));
    }
}

//! Bar section layout: left hugs the left, right hugs the right, center is centred on the screen midline.

pub mod modules;

use crate::config::Config;
use crate::geom::Rect;
use crate::text::{truncate_to_width, TextEngine};
use crate::theme::Theme;
use crate::widget::{Event, Module, Span};

pub use modules::{Clock, Exec};

pub struct Sections {
    pub left: Vec<Box<dyn Module>>,
    pub center: Vec<Box<dyn Module>>,
    pub right: Vec<Box<dyn Module>>,
}

impl Sections {
    /// Returns (sections, the receiving end of the exec line channel); `State` attaches the receiver to calloop.
    pub fn from_config(cfg: &Config) -> (Self, calloop::channel::Channel<Event>) {
        let (exec_tx, exec_rx) = calloop::channel::channel::<Event>();
        let mut exec_id = 0usize;
        let left = build(&cfg.bar.left, &mut exec_id, &exec_tx);
        let center = build(&cfg.bar.center, &mut exec_id, &exec_tx);
        let right = build(&cfg.bar.right, &mut exec_id, &exec_tx);
        (Self { left, center, right }, exec_rx)
    }

    pub fn update(&mut self, ev: &Event) -> bool {
        let mut dirty = false;
        for m in self.left.iter_mut().chain(self.center.iter_mut()).chain(self.right.iter_mut()) {
            dirty |= m.update(ev);
        }
        dirty
    }

    fn widths_of(modules: &mut [Box<dyn Module>], text: &mut TextEngine, theme: &Theme) -> Vec<i32> {
        modules
            .iter_mut()
            .map(|m| {
                let total: f32 = m.spans().iter().map(|s| text.measure(&s.text, &theme.font).0).sum();
                total.ceil() as i32
            })
            .collect()
    }

    pub fn widths(&mut self, text: &mut TextEngine, theme: &Theme) -> SectionWidths {
        SectionWidths {
            left: Self::widths_of(&mut self.left, text, theme),
            center: Self::widths_of(&mut self.center, text, theme),
            right: Self::widths_of(&mut self.right, text, theme),
        }
    }
}

fn build(specs: &[crate::config::ModuleSpec], exec_id: &mut usize, exec_tx: &calloop::channel::Sender<Event>) -> Vec<Box<dyn Module>> {
    specs
        .iter()
        .map(|spec| match spec {
            crate::config::ModuleSpec::Clock { format } => Box::new(Clock::new(format.clone())) as Box<dyn Module>,
            crate::config::ModuleSpec::Exec { command, format } => {
                *exec_id += 1;
                Box::new(Exec::spawn(*exec_id, command.clone(), format.clone(), exec_tx.clone())) as Box<dyn Module>
            }
        })
        .collect()
}

pub struct SectionWidths { pub left: Vec<i32>, pub center: Vec<i32>, pub right: Vec<i32> }

#[derive(Clone, Debug, PartialEq)]
pub struct BarLayout {
    pub left: Vec<Rect>,
    pub center: Vec<Rect>,
    pub right: Vec<Rect>,
}

/// Optical inset for the bar's rounded ends.
///
/// A pill's end is not a straight edge: the boundary curves away from the text, so a gap measured from the
/// bounding box reads smaller than the measured one and the eye hangs the text in the corner. Content is pushed
/// in to the corner's 45° keyline — the same rule keyline grids use when a circle has to read the same size as a
/// square next to it. The corners are continuous (superellipse, see `geom::CORNER_EXPONENT`), so this is
/// `geom::corner_inset`, not `r(1-1/√2)`: a softer corner leaves less to correct. The correction belongs to the
/// component, so it is derived from `radius` and is not a knob.
pub fn end_inset(radius: i32) -> i32 {
    crate::geom::corner_inset(radius)
}

/// Each section lays its items out by `spacing`; center is allocated first among the three.
pub fn layout(widths: &SectionWidths, output_w: i32, theme: &Theme) -> BarLayout {
    let h = theme.height;
    let p = (theme.padding + end_inset(theme.radius)).max(0);
    let s = theme.spacing.max(0);

    let run = |ws: &[i32], start: i32| -> Vec<Rect> {
        let mut x = start;
        ws.iter()
            .map(|w| {
                let r = Rect::new(x, 0, *w, h);
                // zero-width modules take no gap
                // so the modules beside it do not shift when the neighbouring modules keep their relative positions.
                if *w > 0 {
                    x += w + s;
                }
                r
            })
            .collect()
    };
    let total = |ws: &[i32]| {
        let filled = ws.iter().filter(|w| **w > 0).count() as i32;
        if filled == 0 {
            0
        } else {
            ws.iter().sum::<i32>() + s * (filled - 1)
        }
    };

    let center_total = total(&widths.center);
    let center_left = output_w / 2 - center_total / 2;
    let center_right = center_left + center_total;

    // center has priority: it is carved out of the available width first
    let left_limit = if widths.center.is_empty() { output_w } else { center_left - s };
    let right_limit = if widths.center.is_empty() { output_w } else { center_right + s };

    let mut left = run(&widths.left, p);
    for r in left.iter_mut() {
        // Cut the width first, then pull the start back inside the limit: the right edge never crosses center and the width is never negative
        r.w = r.w.min((left_limit - r.x).max(0));
        if r.x > left_limit { r.x = left_limit; }
    }

    let right_total = total(&widths.right);
    let right_start = p.max(output_w - p - right_total);
    let mut right = run(&widths.right, right_start);
    for r in right.iter_mut() {
        r.w = r.w.min((output_w - p - r.x).max(0));
    }
    if !widths.center.is_empty() {
        for r in right.iter_mut() {
            if r.x < right_limit {
                r.x = right_limit;
                r.w = r.w.min((output_w - p - r.x).max(0));
            }
        }
    }

    let center = run(&widths.center, center_left);
    BarLayout { left, center, right }
}

/// Module text wider than the available space is truncated so it never crosses into a neighbouring section.
pub fn fit_text(spans: &[Span], avail: i32, text: &mut TextEngine, theme: &Theme) -> Vec<Span> {
    let mut out = Vec::new();
    let mut left = avail.max(0) as f32;
    for s in spans {
        if left <= 0.0 { break; }
        let (w, _) = text.measure(&s.text, &theme.font);
        if w <= left {
            left -= w;
            out.push(s.clone());
        } else {
            let mut s2 = s.clone();
            let mut measure = |t: &str| text.measure(t, &theme.font).0;
            s2.text = truncate_to_width(&s.text, left, &mut measure);
            left = 0.0;
            if !s2.text.is_empty() { out.push(s2); }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::theme::Theme;

    fn widths(l: &[i32], c: &[i32], r: &[i32]) -> SectionWidths {
        SectionWidths { left: l.to_vec(), center: c.to_vec(), right: r.to_vec() }
    }

    #[test]
    fn three_sections_are_placed_as_specified() {
        let t = Theme::defaults(30); // padding 8, spacing 6, radius 15
        let i = end_inset(t.radius); // 2: the continuous corner is fuller than a circular one
        assert_eq!(i, 2);
        let out = layout(&widths(&[20, 20], &[50], &[30, 30]), 1000, &t);
        assert_eq!(out.left[0], Rect::new(8 + i, 0, 20, 30));
        assert_eq!(out.left[1], Rect::new(8 + i + 20 + 6, 0, 20, 30));
        // center is centred on the screen midline, not on the space that is left over
        assert_eq!(out.center[0], Rect::new(500 - 25, 0, 50, 30));
        // right hugs the right edge; its internals still run left to right
        assert_eq!(out.right[0], Rect::new(1000 - 8 - i - 66, 0, 30, 30));
        assert_eq!(out.right[1], Rect::new(1000 - 8 - i - 30, 0, 30, 30));

    }

    /// Both ends keep the same optical inset, so the text does not look pinned to one corner of the pill.
    #[test]
    fn pill_ends_are_inset_by_the_corner_keyline() {
        let t = Theme::defaults(30);
        let i = end_inset(t.radius);
        for radius in [0, 2, 15, 30] {
            assert!(end_inset(radius) >= 0);
            assert!(end_inset(radius) <= radius / 3 + 1, "the inset stays a corner correction, not a margin");
        }
        assert_eq!(end_inset(0), 0, "a square bar needs no corner correction");
        let out = layout(&widths(&[10], &[], &[10]), 1000, &t);
        assert_eq!(out.left[0].x, t.padding + i);
        assert_eq!(out.right[0].right(), 1000 - t.padding - i);
    }

    #[test]
    fn notification_config_does_not_enter_bar_layout() {
        let cfg = crate::config::parse("[notification]\nposition = \"center\"\n[bar.right]\nmodules = [ { kind = \"clock\", format = \"%H\" } ]\n").unwrap();
        let (mut sections, _rx) = Sections::from_config(&cfg);
        let theme = Theme::defaults(30);
        let widths = sections.widths(&mut TextEngine::new(), &theme);
        assert_eq!(widths.right.len(), 1);
        assert!(widths.right[0] > 0);
    }

    #[test]
    fn empty_sections_stay_empty() {
        let t = Theme::defaults(30);
        let out = layout(&widths(&[], &[], &[]), 1000, &t);
        assert!(out.left.is_empty() && out.right.is_empty() && out.center.is_empty());
    }

    #[test]
    fn center_wins_overlap_and_sides_are_truncated() {
        let t = Theme::defaults(30);
        // a 200-wide left plus a centered center would overlap left of the midline
        let out = layout(&widths(&[480, 100], &[200], &[480]), 1000, &t);
        let center_left = out.center[0].x;
        for r in &out.left {
            assert!(r.right() <= center_left - t.spacing, "left overlaps center: {r:?} vs {center_left}");
            assert!(r.w >= 0);
        }
        for r in &out.right {
            assert!(r.x >= out.center[0].right() + t.spacing, "right overlaps center: {r:?}");
            assert!(r.x + r.w <= 1000 - t.padding);
        }
    }
}

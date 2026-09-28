//! Bar section layout: left hugs the left, right hugs the right, center is centred on the screen midline.

pub mod modules;

use crate::config::Config;
use crate::geom::Rect;
use crate::text::{truncate_to_width, TextEngine};
use crate::theme::Theme;
use crate::widget::{Event, Module, Span};

pub use modules::{Applications, Clock, Exec};

pub struct Sections {
    pub left: Vec<Box<dyn Module>>,
    pub center: Vec<Box<dyn Module>>,
    pub right: Vec<Box<dyn Module>>,
}

impl Sections {
    /// Returns sections, the receiver for module events and the sender the Wayland callbacks use for toplevel updates.
    pub fn from_config(cfg: &Config) -> (Self, calloop::channel::Channel<Event>, calloop::channel::Sender<Event>) {
        let (event_tx, event_rx) = calloop::channel::channel::<Event>();
        let mut exec_id = 0usize;
        let left = build(&cfg.bar.left, &mut exec_id, &event_tx);
        let center = build(&cfg.bar.center, &mut exec_id, &event_tx);
        let right = build(&cfg.bar.right, &mut exec_id, &event_tx);
        (Self { left, center, right }, event_rx, event_tx)
    }

    pub fn update(&mut self, ev: &Event) -> bool {
        let mut dirty = false;
        for m in self.left.iter_mut().chain(self.center.iter_mut()).chain(self.right.iter_mut()) {
            dirty |= m.update(ev);
        }
        dirty
    }

    fn widths_of(modules: &mut [Box<dyn Module>], text: &mut TextEngine, theme: &Theme) -> Vec<i32> {
        modules.iter_mut().map(|module| spans_width(&module.spans(), text, theme)).collect()
    }

    pub fn widths(&mut self, text: &mut TextEngine, theme: &Theme) -> SectionWidths {
        SectionWidths {
            left: Self::widths_of(&mut self.left, text, theme),
            center: Self::widths_of(&mut self.center, text, theme),
            right: Self::widths_of(&mut self.right, text, theme),
        }
    }
}

fn build(specs: &[crate::config::ModuleSpec], exec_id: &mut usize, event_tx: &calloop::channel::Sender<Event>) -> Vec<Box<dyn Module>> {
    specs
        .iter()
        .map(|spec| match spec {
            crate::config::ModuleSpec::Clock { format } => Box::new(Clock::new(format.clone())) as Box<dyn Module>,
            crate::config::ModuleSpec::Exec { command, format } => {
                *exec_id += 1;
                Box::new(Exec::spawn(*exec_id, command.clone(), format.clone(), event_tx.clone())) as Box<dyn Module>
            }
            crate::config::ModuleSpec::Applications => Box::new(Applications::new()) as Box<dyn Module>,
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

/// Each section lays its items out by `spacing`; center is allocated first among the three.
pub fn layout(widths: &SectionWidths, output_w: i32, theme: &Theme) -> BarLayout {
    let h = theme.height;
    let p = theme.padding.max(0);
    let s = theme.spacing.max(0);

    let run = |ws: &[i32], start: i32| -> Vec<Rect> {
        let mut x = start;
        ws.iter()
            .map(|w| {
                let r = Rect::new(x, 0, *w, h);
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

/// The width of a module's visual units; application chips are square and every pair takes one `spacing` gap.
pub fn spans_width(spans: &[Span], text: &mut TextEngine, theme: &Theme) -> i32 {
    if spans.is_empty() {
        return 0;
    }
    let width: i32 = spans
        .iter()
        .map(|span| if span.badge.is_some() { theme.app_icon } else { text.measure(&span.text, &theme.font).0.ceil() as i32 })
        .sum();
    width + theme.spacing.max(0) * (spans.len() as i32 - 1)
}

/// Module text wider than the available space is truncated so it never crosses into a neighbouring section.
/// An application chip is never truncated: a half chip is worse than a missing one.
pub fn fit_text(spans: &[Span], avail: i32, text: &mut TextEngine, theme: &Theme) -> Vec<Span> {
    let mut out = Vec::new();
    let mut left = avail.max(0) as f32;
    let gap = theme.spacing.max(0) as f32;
    for (index, span) in spans.iter().enumerate() {
        if index > 0 { left -= gap; }
        if left <= 0.0 { break; }
        if span.badge.is_some() {
            let width = theme.app_icon as f32;
            if width > left { break; }
            left -= width;
            out.push(span.clone());
        } else {
            let (width, _) = text.measure(&span.text, &theme.font);
            if width <= left {
                left -= width;
                out.push(span.clone());
            } else {
                let mut clipped = span.clone();
                let mut measure = |value: &str| text.measure(value, &theme.font).0;
                clipped.text = truncate_to_width(&span.text, left, &mut measure);
                if !clipped.text.is_empty() { out.push(clipped); }
                break;
            }
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
        let t = Theme::defaults(30);
        let out = layout(&widths(&[20, 20], &[50], &[30, 30]), 1000, &t);
        assert_eq!(out.left[0], Rect::new(8, 0, 20, 30));
        assert_eq!(out.left[1], Rect::new(8 + 20 + 6, 0, 20, 30));
        assert_eq!(out.center[0], Rect::new(500 - 25, 0, 50, 30));
        assert_eq!(out.right[0], Rect::new(1000 - 8 - 66, 0, 30, 30));
        assert_eq!(out.right[1], Rect::new(1000 - 8 - 30, 0, 30, 30));
    }

    #[test]
    fn square_bar_uses_only_configured_padding() {
        let t = Theme::defaults(30);
        let out = layout(&widths(&[10], &[], &[10]), 1000, &t);
        assert_eq!(out.left[0].x, t.padding);
        assert_eq!(out.right[0].right(), 1000 - t.padding);
    }

    #[test]
    fn notification_config_does_not_enter_bar_layout() {
        let cfg = crate::config::parse("[notification]\nposition = \"center\"\n[bar.right]\nmodules = [ { kind = \"clock\", format = \"%H\" } ]\n").unwrap();
        let (mut sections, _rx, _tx) = Sections::from_config(&cfg);
        let theme = Theme::defaults(30);
        let widths = sections.widths(&mut TextEngine::new(), &theme);
        assert_eq!(widths.right.len(), 1);
        assert!(widths.right[0] > 0);
    }

    #[test]
    fn application_chips_keep_their_width_and_take_a_gap() {
        let theme = Theme::defaults(30);
        let mut text = TextEngine::new();
        let chip = Span::application("F", 2);
        assert_eq!(spans_width(std::slice::from_ref(&chip), &mut text, &theme), theme.app_icon);
        let text_width = text.measure("x", &theme.font).0.ceil() as i32;
        assert_eq!(spans_width(&[chip, Span::text("x")], &mut text, &theme), theme.app_icon + theme.spacing + text_width);
    }

    #[test]
    fn fit_text_drops_a_chip_that_does_not_fit_instead_of_truncating_it() {
        let theme = Theme::defaults(30);
        let mut text = TextEngine::new();
        assert!(fit_text(&[Span::application("F", 2)], 2, &mut text, &theme).is_empty());
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

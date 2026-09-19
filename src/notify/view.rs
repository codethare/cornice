//! Notification card layout, drawing and hit testing; the island animation only supplies an interpolated rect.

use crate::anim::{Easing, Tween};
use crate::canvas::Canvas;
use crate::geom::{Color, Rect};
use crate::notify::queue::{Notification, Urgency};
use crate::text::TextEngine;
use crate::theme::Theme;
use crate::widget::Action;

/// Maximum card width; overflow is truncated by character rather than wrapped, keeping the height predictable.
pub const MAX_CARD_W: i32 = 420;

pub fn urgency_color(u: Urgency, theme: &Theme) -> Color {
    match u {
        Urgency::Low | Urgency::Normal => theme.foreground,
        Urgency::Critical => Color::rgba(0xff, 0x5f, 0x56, 0xff),
    }
}

pub fn card_size(n: &Notification, text: &mut TextEngine, theme: &Theme) -> (i32, i32) {
    let (sw, _) = text.measure(&n.summary, &theme.font);
    let (bw, _) = text.measure(&n.body, &theme.font);
    let line = (theme.font.size * 1.35).ceil() as i32;
    let w = (sw.max(bw).ceil() as i32 + theme.card_padding * 2).clamp(80, MAX_CARD_W);
    let mut h = theme.card_padding * 2 + line;
    if !n.body.is_empty() {
        h += line;
    }
    if !n.actions.is_empty() {
        h += line + theme.card_gap;
    }
    (w, h)
}

/// Stacked top to bottom, right-aligned; `top` is the top edge of the first card.
pub fn card_rects(count: usize, sizes: &[(i32, i32)], output_w: i32, theme: &Theme, top: i32) -> Vec<Rect> {
    let mut y = top;
    (0..count)
        .map(|i| {
            let (w, h) = sizes.get(i).copied().unwrap_or((200, 40));
            let r = Rect::new(output_w - theme.padding - w, y, w, h);
            y += h + theme.card_gap;
            r
        })
        .collect()
}

/// Draw one card and return the button hit rect. The caller appends the card body (close) hit rect after the button.
///
/// `alpha` applies to the text only: the island is "the solid capsule that is already there", the background stays opaque, and only the text follows
/// the card opens and floats out (design §7's "the capsule opens into a box, then the text floats out"). Scaling the background by `alpha` as well
/// would make the island fully transparent at t=0, losing the "growing out of the right cluster" origin.
pub fn render(canvas: &mut Canvas, rect: Rect, radius: i32, alpha: f32, n: &Notification, theme: &Theme, text: &mut TextEngine) -> Vec<(Rect, Action)> {
    canvas.fill_rounded_rect(rect, radius, theme.background);
    let fa = |c: Color| Color::rgba(c.r, c.g, c.b, (c.a as f32 * alpha.clamp(0.0, 1.0)) as u8);
    let line = (theme.font.size * 1.35).ceil() as i32;
    let x = rect.x + theme.card_padding;
    let mut y = rect.y + theme.card_padding;
    text.draw(canvas, &n.summary, x, y, &theme.font, fa(urgency_color(n.urgency, theme)));
    y += line;
    if !n.body.is_empty() {
        text.draw(canvas, &n.body, x, y, &theme.font, fa(theme.foreground));
        y += line;
    }
    let mut hits = Vec::new();
    if !n.actions.is_empty() {
        y += theme.card_gap / 2;
        let mut bx = x;
        for (key, label) in &n.actions {
            let (w, _) = text.measure(label, &theme.font);
            let br = Rect::new(bx, y, w.ceil() as i32 + theme.card_padding, line);
            canvas.fill_rounded_rect(br, line / 2, theme.accent);
            text.draw(canvas, label, bx + theme.card_padding / 2, y, &theme.font, fa(theme.background));
            hits.push((br, Action::NotificationAction { id: n.id, key: key.clone() }));
            bx = br.right() + theme.card_gap;
        }
    }
    hits
}

pub fn hit(hits: &[(Rect, Action)], x: i32, y: i32) -> Option<Action> {
    hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone())
}

/// the t=0 rect: it must be exactly the bar's right cluster.
pub fn island_start(bar: &crate::bar::BarLayout, theme: &Theme) -> Rect {
    let mut r = bar.right_cluster;
    r.h = theme.height;
    r
}

/// Enter: t=0 is the origin (island), t=1 the destination (card).
pub fn island_enter_rect(start: Rect, end: Rect, tw: &Tween, now: std::time::Instant) -> Rect {
    Rect::lerp(start, end, Easing::OutCubic.apply(tw.progress(now)))
}

/// Leave: t=0 is the origin (card), t=1 the destination (island).
pub fn island_exit_rect(start: Rect, end: Rect, tw: &Tween, now: std::time::Instant) -> Rect {
    Rect::lerp(start, end, Easing::InOutCubic.apply(tw.progress(now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::theme::Theme;

    #[test]
    fn island_start_equals_right_cluster_rect() {
        let t = Theme::defaults(30);
        let layout = crate::bar::layout(
            &crate::bar::SectionWidths { left: vec![20], center: vec![], right: vec![40, 30] },
            1000,
            &t,
        );
        let start = island_start(&layout, &t);
        assert_eq!(start, layout.right_cluster, "t=0 must coincide with the bar's right cluster");
        assert_eq!(start.h, t.height);
    }

    #[test]
    fn card_rects_stack_downward_from_below_the_bar() {
        let t = Theme::defaults(30);
        let sizes = vec![(300, 60), (300, 40), (300, 40)];
        let rects = card_rects(3, &sizes, 1000, &t, t.height + t.card_gap);
        assert_eq!(rects[0], Rect::new(1000 - t.padding - 300, 30 + t.card_gap, 300, 60));
        assert_eq!(rects[1].y, rects[0].bottom() + t.card_gap);
        assert_eq!(rects[2].y, rects[1].bottom() + t.card_gap);
        assert!(rects.iter().all(|r| r.right() == 1000 - t.padding), "all right-aligned");
    }

    #[test]
    fn hit_testing_matches_buttons_only() {
        let hits = vec![
            (Rect::new(10, 10, 50, 20), Action::NotificationAction { id: 1, key: "open".into() }),
            (Rect::new(70, 10, 50, 20), Action::NotificationClose(1)),
        ];
        assert_eq!(hit(&hits, 20, 15), Some(Action::NotificationAction { id: 1, key: "open".into() }));
        assert_eq!(hit(&hits, 80, 15), Some(Action::NotificationClose(1)));
        assert_eq!(hit(&hits, 200, 15), None);
    }

    #[test]
    fn card_width_is_capped_and_wraps_content() {
        let t = Theme::defaults(30);
        let mut text = crate::text::TextEngine::new();
        let n = crate::notify::queue::Notification {
            id: 1,
            app_name: "app".into(),
            summary: "s".into(),
            body: "b".into(),
            urgency: crate::notify::queue::Urgency::Normal,
            expire: None,
            actions: vec![],
            created: std::time::Instant::now(),
        };
        let (w, h) = card_size(&n, &mut text, &t);
        assert!(w <= 420 && w > 0, "the card has a maximum width, got {w}");
        assert!(h >= t.card_padding * 2 + t.font.size as i32, "at least padding plus one line of text");
    }
}

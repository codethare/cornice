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
    let cap = text.cap_metrics(&theme.font).cap.ceil() as i32;
    let w = (sw.max(bw).ceil() as i32 + theme.card_padding * 2).clamp(80, MAX_CARD_W);
    // body was already truncated to MAX_BODY_LINES by the queue, but a directly constructed Notification (tests) may have more;
    // the height is counted from the real line count, or a multi-line body spills onto the next card (final review Critical #2a).
    let body_lines = if n.body.is_empty() { 0 } else { n.body.lines().count().clamp(1, crate::notify::queue::MAX_BODY_LINES) };
    // The text block is measured from the cap of its first line to the baseline of its last, not from
    // `lines * line_height`: the leading above the cap and the descent below the baseline are empty,
    // so counting them would make the bottom padding look wider than the top one (text-box-trim).
    let mut h = theme.card_padding * 2 + cap + line * body_lines as i32;
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
/// `alpha` applies to the text only: the card is "the solid capsule that is already there", so its background stays
/// opaque while the box grows out of the bar and only the text follows — design §7: "the capsule opens into a box,
/// then the text floats out". Scaling the background by `alpha` as well would make the growing box fade in from
/// nothing, losing the "stretched out of the right cluster" origin.
pub fn render(canvas: &mut Canvas, rect: Rect, radius: i32, alpha: f32, n: &Notification, theme: &Theme, text: &mut TextEngine) -> Vec<(Rect, Action)> {
    canvas.fill_rounded_rect(rect, radius, theme.background);
    let fa = |c: Color| Color::rgba(c.r, c.g, c.b, (c.a as f32 * alpha.clamp(0.0, 1.0)) as u8);
    let line = (theme.font.size * 1.35).ceil() as i32;
    let x = rect.x + theme.card_padding;
    // inner width: all text is truncated to the card; summary is untrusted D-Bus input and must be truncated (final review Critical #2b).
    let inner_w = (rect.w - theme.card_padding * 2).max(0) as f32;
    // while entering, the rect is still smaller than the laid-out text: clip text pixels to the card rect (design §7, final review Critical #2c).
    canvas.set_clip(Some(rect));

    let mut y = text.cap_top(&theme.font, rect.y, theme.card_padding);
    let summary = crate::text::truncate_to_width(&n.summary, inner_w, |t: &str| text.measure(t, &theme.font).0);
    text.draw(canvas, &summary, x, y, &theme.font, fa(urgency_color(n.urgency, theme)));
    y += line;
    for body_line in n.body.lines() {
        let t = crate::text::truncate_to_width(body_line, inner_w, |t: &str| text.measure(t, &theme.font).0);
        text.draw(canvas, &t, x, y, &theme.font, fa(theme.foreground));
        y += line;
    }

    let mut hits = Vec::new();
    if !n.actions.is_empty() {
        y += theme.card_gap / 2;
        let mut bx = x;
        for (key, label) in &n.actions {
            let t = crate::text::truncate_to_width(label, inner_w, |t: &str| text.measure(t, &theme.font).0);
            let (w, _) = text.measure(&t, &theme.font);
            let br = Rect::new(bx, y, w.ceil() as i32 + theme.card_padding, line);
            canvas.fill_rounded_rect(br, line / 2, theme.accent);
            // The label is centred in the pill by its cap, not by its line box, like the bar's text.
            let ty = text.optical_top(&theme.font, br.y, br.h);
            text.draw(canvas, &t, bx + theme.card_padding / 2, ty, &theme.font, fa(theme.background));
            hits.push((br, Action::NotificationAction { id: n.id, key: key.clone() }));
            bx = br.right() + theme.card_gap;
        }
    }

    canvas.set_clip(None);
    hits
}

pub fn hit(hits: &[(Rect, Action)], x: i32, y: i32) -> Option<Action> {
    hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone())
}

/// The enter's origin: the bar's right cluster supplies the horizontal placement the blob starts from
/// (its width and its left edge), and its right edge must be the screen edge, so the blob emerges from
/// the pill's own end rather than from a few pixels inside it.
pub fn island_start(bar: &crate::bar::BarLayout, theme: &Theme) -> Rect {
    let mut r = bar.right_cluster;
    r.h = theme.height;
    r
}

/// Enter: the card grows *downwards out of the bar's bottom edge*, starting from the right cluster's
/// horizontal placement. Growing in place instead of travelling from the cluster to the card is what
/// reads as the bar stretching an arm: every frame the blob is attached to the same edge, its top
/// corners stay the card's rounded shoulders, and the join is tangent (a smooth "trumpet") from the
/// first frame to the last. A travelling rect would show a straight-edged tab sliding out of the bar.
pub fn island_enter_rect(start: Rect, end: Rect, tw: &Tween, now: std::time::Instant) -> Rect {
    let e = Easing::OutCubic.apply(tw.progress(now));
    let lerp = |a: i32, b: i32| a + ((b - a) as f32 * e).round() as i32;
    Rect::new(lerp(start.x, end.x), end.y, lerp(start.w, end.w), ((end.h as f32) * e).round() as i32)
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
        let rects = card_rects(3, &sizes, 1000, &t, t.height);
        assert_eq!(rects[0], Rect::new(1000 - t.padding - 300, t.height, 300, 60));
        assert_eq!(rects[1].y, rects[0].bottom() + t.card_gap);
        assert_eq!(rects[2].y, rects[1].bottom() + t.card_gap);
        assert!(rects.iter().all(|r| r.right() == 1000 - t.padding), "all right-aligned");
    }

    /// The card's nominal padding is also its *optical* padding: the ink gap at the top and the bottom
    /// must both be `card_padding`, or the block reads as sitting high/low in its box.
    #[test]
    fn card_text_keeps_equal_optical_padding() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = Notification {
            id: 1,
            summary: "Hi".into(),
            // no descender, so the ink bottom is the baseline: pad below the baseline is what we measure
            body: "no descenders".into(),
            urgency: Urgency::Normal,
            expire: None,
            actions: vec![],
            created: std::time::Instant::now(),
        };
        let (w, h) = card_size(&n, &mut text, &t);
        let w = w + 20;
        let h = h + 20;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let card = Rect::new(10, 10, w - 20, h - 20);
        {
            let mut c = Canvas::new(&mut buf, w, h);
            render(&mut c, card, t.radius, 1.0, &n, &t, &mut text);
        }
        let ink_rows: Vec<i32> = (0..h)
            .filter(|y| (0..w).any(|x| buf[((y * w + x) * 4) as usize] > 150))
            .collect();
        let (first, last) = (*ink_rows.first().unwrap(), *ink_rows.last().unwrap());
        assert_eq!(first, card.y + t.card_padding, "ink top must sit card_padding below the card's top edge");
        assert_eq!(last, card.bottom() - t.card_padding - 1, "ink bottom must sit card_padding above the card's bottom edge");
    }

    /// The enter rect stays anchored to the bar's bottom edge for the whole animation: it is the bar's
    /// material stretching, not a card flying in. t=0 is an invisible sliver, t=1 exactly the card.
    #[test]
    fn enter_grows_out_of_the_bar_edge() {
        let t = Theme::defaults(30);
        let start = Rect::new(1224, 0, 56, t.height); // the right cluster
        let end = Rect::new(1000, t.height, 280, 53); // the head card slot
        let now = std::time::Instant::now();
        let tw = Tween::new(now, 200);
        let at = |ms| island_enter_rect(start, end, &tw, now + std::time::Duration::from_millis(ms));
        let a = at(0);
        assert_eq!((a.y, a.h), (end.y, 0), "t=0 is a zero-height sliver on the bar's bottom edge: {a:?}");
        assert_eq!((a.x, a.w), (start.x, start.w), "...at the right cluster: {a:?}");
        assert_eq!(at(200), end, "t=1 is exactly the card slot");
        for ms in [10, 50, 100, 150, 190] {
            let r = at(ms);
            assert_eq!(r.y, end.y, "frame at {ms}ms must stay attached to the bar's edge: {r:?}");
            assert!(r.h >= 0 && r.h <= end.h, "height grows monotonically: {r:?}");
        }
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
            summary: "s".into(),
            body: "b".into(),
            urgency: crate::notify::queue::Urgency::Normal,
            expire: None,
            actions: vec![],
            created: std::time::Instant::now(),
        };
        let (w, h) = card_size(&n, &mut text, &t);
        assert!(w <= 420 && w > 0, "the card has a maximum width, got {w}");
        // one line of body + a summary line: padding + cap + one line advance (no leading above the cap)
        let line = (t.font.size * 1.35).ceil() as i32;
        let cap = text.cap_metrics(&t.font).cap.ceil() as i32;
        assert_eq!(h, t.card_padding * 2 + cap + line);
    }
}

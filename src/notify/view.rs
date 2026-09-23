//! Notification card layout, drawing and hit testing. The notification surface only supplies the stretch.

use std::time::Instant;

use crate::anim::{Easing, Tween};
use crate::canvas::Canvas;
use crate::geom::{Color, Rect};
use crate::notify::queue::{Notification, Urgency};
use crate::text::TextEngine;
use crate::theme::Theme;
use crate::widget::Action;

/// Maximum card width; overflow is truncated by character rather than wrapped, keeping the height predictable.
pub const MAX_CARD_W: i32 = 420;
/// Floor for a card holding one short word, so the shape never collapses into a sliver.
pub const MIN_CARD_W: i32 = 80;

pub fn urgency_color(u: Urgency, theme: &Theme) -> Color {
    match u {
        Urgency::Low | Urgency::Normal => theme.foreground,
        Urgency::Critical => Color::rgba(0xff, 0x5f, 0x56, 0xff),
    }
}

/// One line's advance; every row in the column steps by it.
fn line_height(theme: &Theme) -> i32 { (theme.font.size * 1.35).ceil() as i32 }

/// Body rows a card draws. The queue already caps the body, but a hand-built `Notification` may exceed it.
fn body_lines(n: &Notification) -> usize {
    if n.body.is_empty() { 0 } else { n.body.lines().count().min(crate::notify::queue::MAX_BODY_LINES) }
}

/// The width a card's text asks for; the column is as wide as the widest visible card.
pub fn card_width(n: &Notification, text: &mut TextEngine, theme: &Theme) -> i32 {
    let (sw, _) = text.measure(&n.summary, &theme.font);
    let (bw, _) = text.measure(&n.body, &theme.font);
    (sw.max(bw).ceil() as i32 + theme.card_padding * 2).clamp(MIN_CARD_W, MAX_CARD_W)
}

/// Width the bar reserves for the notification module: the widest visible card, 0 while nothing is visible.
pub fn slot_width(visible: &[Notification], text: &mut TextEngine, theme: &Theme) -> i32 {
    visible.iter().map(|n| card_width(n, text, theme)).max().unwrap_or(0)
}

/// The stretch attaches to the bar's bottom edge, so its span is clamped into `[r, output_w - r]`: that is
/// where the pill's bottom edge is straight. Under the pill's curve the column's square top corners would
/// leave a notch between the two silhouettes, i.e. the background *would* come apart at the join.
fn straight_band(x: i32, w: i32, output_w: i32, theme: &Theme) -> (i32, i32) {
    let r = theme.radius.clamp(0, theme.height / 2);
    let x = x.clamp(r, (output_w - r).max(r));
    (x, w.min(output_w - r - x).max(0))
}

/// One visible notification's rows inside the column.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// The card's own band: the click-to-close hit rect.
    pub band: Rect,
    /// Box top to hand to `draw` for the summary; the head card puts it on the bar's own text line.
    pub summary: i32,
    /// Box top of the first body line.
    pub body: i32,
    /// The action row's pill top; the pill is one line tall.
    pub actions: Option<i32>,
}

/// The stretched shape hanging off the bar. Its top is the bar's top, so the part that overlaps the bar is
/// invisible and the two read as one material.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub rect: Rect,
    pub cards: Vec<Card>,
}

/// Lay the visible cards out as one column. The head card's summary shares the bar's text line and only whatever
/// does not fit in the bar stretches below it; further cards follow at `card_gap` *inside* the same shape, so the
/// background never breaks between them either. Newest is at the top, right under the bar.
pub fn column(visible: &[Notification], slot_x: i32, slot_w: i32, output_w: i32, theme: &Theme, text: &mut TextEngine) -> Column {
    let (x, w) = straight_band(slot_x, slot_w, output_w, theme);
    let line = line_height(theme);
    let m = text.cap_metrics(&theme.font);
    let (cap, ink_off) = (m.cap.ceil() as i32, m.top.round() as i32);
    let mut cards: Vec<Card> = Vec::with_capacity(visible.len());
    let mut band_top = 0;
    for (i, n) in visible.iter().enumerate() {
        let rows = body_lines(n);
        // The head card shares the bar's line; every later card pads its summary from its own band top.
        let summary = if i == 0 { text.optical_top(&theme.font, 0, theme.height) } else { text.cap_top(&theme.font, band_top, theme.card_padding) };
        let mut next = summary; // box top of the row after the last one drawn
        let mut last_ink = summary + ink_off + cap; // ink bottom of the last row drawn
        for _ in 0..rows {
            next += line;
            last_ink = next + ink_off + cap;
        }
        let actions = if n.actions.is_empty() {
            None
        } else {
            next += theme.card_gap / 2;
            last_ink = next + line;
            Some(next)
        };
        // A head card that fits entirely inside the bar owns just the bar's row: nothing is stretched. Once a row
        // lands below the bar, the card keeps its bottom padding, so the shape ends `card_padding` below the baseline.
        let bottom = if i == 0 && last_ink <= theme.height { theme.height } else { last_ink + theme.card_padding };
        cards.push(Card { band: Rect::new(x, band_top, w, bottom - band_top), summary, body: summary + line, actions });
        band_top = bottom + theme.card_gap;
    }
    let h = match cards.last() {
        Some(c) => c.band.bottom(),
        // Nothing visible (an exit in flight): the shape is the bar's own row, and its width comes from `slot_w`.
        None => theme.height,
    };
    Column { rect: Rect::new(x, 0, w, h), cards }
}

/// Paint the column's background. Nothing above the bar's bottom edge is painted: the bar already painted that
/// area, and `theme.background` is translucent, so a second pass would darken the overlap and the stretch would
/// read as a separate layer glued under the bar instead of the bar's own material.
pub fn render_column(canvas: &mut Canvas, column: Rect, bar_bottom: i32, clip: Rect, theme: &Theme) {
    let below = clip.intersect(Rect::new(column.x, bar_bottom, column.w, column.bottom() - bar_bottom));
    if below.is_empty() {
        return;
    }
    canvas.set_clip(Some(below));
    canvas.fill_rounded_rect(column, theme.radius, theme.background);
    canvas.set_clip(Some(clip));
}

/// Draw one card's rows and return its button hit rects. The caller owns the clip: text is clipped to the
/// animating shape, which is still shorter than the laid-out rows while the column grows.
///
/// `alpha` applies to the text only: the shape is the solid capsule that is already there, so the background stays
/// opaque while it stretches and only the text follows — design §7: "the capsule opens into a box, then the text
/// floats out".
pub fn render(canvas: &mut Canvas, card: &Card, alpha: f32, n: &Notification, theme: &Theme, text: &mut TextEngine) -> Vec<(Rect, Action)> {
    let fa = |c: Color| Color::rgba(c.r, c.g, c.b, (c.a as f32 * alpha.clamp(0.0, 1.0)) as u8);
    let line = line_height(theme);
    let x = card.band.x + theme.card_padding;
    // Inner width: all text is truncated to the column; summary is untrusted D-Bus input and must be truncated.
    let inner_w = (card.band.w - theme.card_padding * 2).max(0) as f32;

    let summary = crate::text::truncate_to_width(&n.summary, inner_w, |t: &str| text.measure(t, &theme.font).0);
    text.draw(canvas, &summary, x, card.summary, &theme.font, fa(urgency_color(n.urgency, theme)));
    let mut y = card.body;
    for body_line in n.body.lines().take(crate::notify::queue::MAX_BODY_LINES) {
        let t = crate::text::truncate_to_width(body_line, inner_w, |t: &str| text.measure(t, &theme.font).0);
        text.draw(canvas, &t, x, y, &theme.font, fa(theme.foreground));
        y += line;
    }

    let mut hits = Vec::new();
    if let Some(ay) = card.actions {
        let mut bx = x;
        for (key, label) in &n.actions {
            let t = crate::text::truncate_to_width(label, inner_w, |t: &str| text.measure(t, &theme.font).0);
            let (w, _) = text.measure(&t, &theme.font);
            let br = Rect::new(bx, ay, (w.ceil() as i32 + theme.card_padding).min(card.band.w).max(0), line);
            canvas.fill_rounded_rect(br, line / 2, theme.accent);
            // The label is centred in the pill by its cap, not by its line box, like the bar's text.
            let ty = text.optical_top(&theme.font, br.y, br.h);
            text.draw(canvas, &t, bx + theme.card_padding / 2, ty, &theme.font, fa(theme.background));
            hits.push((br, Action::NotificationAction { id: n.id, key: key.clone() }));
            bx = br.right() + theme.card_gap;
        }
    }
    hits
}

pub fn hit(hits: &[(Rect, Action)], x: i32, y: i32) -> Option<Action> {
    hits.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone())
}

/// The stretching shape: y is always 0 and the top is the bar's top, so the overlap with the bar is invisible;
/// x/w come from `end` (the current layout) because the bar's material stretches, it does not slide sideways.
pub fn stretch(start: Rect, end: Rect, easing: Easing, tw: &Tween, now: Instant) -> Rect {
    let e = easing.apply(tw.progress(now));
    let h = start.h + ((end.h - start.h) as f32 * e).round() as i32;
    Rect::new(end.x, 0, end.w, h)
}

/// Upper bound of the column's tail below the bar, used to size the notification surface once: a resize per
/// animation frame would stutter, so the surface must already be tall enough for every frame.
pub fn max_tail(max_visible: usize, theme: &Theme, text: &mut TextEngine) -> i32 {
    let line = line_height(theme);
    let cap = text.cap_metrics(&theme.font).cap.ceil() as i32;
    let worst_card = theme.card_padding * 2 + cap + line * (crate::notify::queue::MAX_BODY_LINES as i32 + 1) + theme.card_gap / 2;
    (worst_card + theme.card_gap) * (max_visible as i32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32, summary: &str, body: &str) -> Notification {
        Notification {
            id,
            summary: summary.into(),
            body: body.into(),
            urgency: Urgency::Normal,
            expire: None,
            actions: vec![],
            created: Instant::now(),
        }
    }

    fn laid_out(visible: &[Notification], slot_x: i32, slot_w: i32, output_w: i32, theme: &Theme, text: &mut TextEngine) -> Column {
        column(visible, slot_x, slot_w, output_w, theme, text)
    }

    /// "The first card is shown in the bar": a card whose rows fit inside the bar stretches nowhere.
    #[test]
    fn a_short_card_stays_inside_the_bar() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Hi", "");
        let w = card_width(&n, &mut text, &t);
        let col = laid_out(&[n], 1000 - t.padding - w, w, 1000, &t, &mut text);
        assert_eq!(col.rect.h, t.height, "nothing is stretched below the bar: {col:?}");
        assert_eq!(col.cards.len(), 1);
        let head = &col.cards[0];
        assert_eq!(head.band, Rect::new(col.rect.x, 0, col.rect.w, t.height));
        // The summary sits on the bar's own text line, not on a card padding line.
        assert_eq!(head.summary, text.optical_top(&t.font, 0, t.height));
    }

    /// "If it is too long, it stretches downwards": rows that do not fit in the bar extend the same shape.
    #[test]
    fn a_long_card_stretches_below_the_bar() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let line = line_height(&t);
        let m = text.cap_metrics(&t.font);
        let (cap, ink_off) = (m.cap.ceil() as i32, m.top.round() as i32);
        let n = note(1, "Subject", "one\ntwo\nthree");
        let w = card_width(&n, &mut text, &t);
        let col = laid_out(&[n], 0, w, 1000, &t, &mut text);
        assert!(col.rect.h > t.height, "three body rows must not fit in a 30px bar: {col:?}");
        let head = &col.cards[0];
        assert_eq!(head.body, head.summary + line, "body rows keep the text rhythm from the bar's line");
        assert_eq!(head.summary + ink_off, t.height / 2 - cap / 2, "the head summary is cap-centred on the bar's line");
        // The last row is the third body row: its ink bottom plus one card_padding is the shape's bottom.
        assert_eq!(
            head.band.bottom(),
            head.body + 2 * line + ink_off + cap + t.card_padding,
            "the shape ends one card_padding below the last baseline"
        );
        assert_eq!(head.band.y, 0, "the card's band starts at the bar's top");
    }

    /// "A second notification keeps stretching downwards": one shape, the later card inside it.
    #[test]
    fn the_stack_is_one_piece() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let a = note(1, "First", "body");
        let b = note(2, "Second", "");
        let w = slot_width(&[a.clone(), b.clone()], &mut text, &t);
        let col = laid_out(&[a, b], 0, w, 1000, &t, &mut text);
        assert_eq!(col.cards.len(), 2);
        assert_eq!(col.cards[1].band.y, col.cards[0].band.bottom() + t.card_gap);
        assert_eq!(col.rect.h, col.cards[1].band.bottom(), "the column covers every card, so there is no break in it");
        assert_eq!(col.rect.y, 0, "the shape starts at the bar's top and its overlap with the bar is invisible");
    }

    /// The stretch may only attach where the bar's bottom edge is straight, or its square top corners would
    /// leave a notch under the pill's curve.
    #[test]
    fn the_column_attaches_to_the_straight_part_of_the_bar() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Wide", "");
        let w = card_width(&n, &mut text, &t);
        let r = t.radius;
        // Right section: the module's slot runs into the pill's rounded end.
        let col = laid_out(&[n.clone()], 1000 - t.padding - w, w, 1000, &t, &mut text);
        assert_eq!(col.rect.right(), 1000 - r, "the column stops where the pill's bottom edge starts to curve");
        // Left section, same mirror: padding alone would put the column inside the corner curve.
        let col = laid_out(&[n], t.padding, w, 1000, &t, &mut text);
        assert_eq!(col.rect.x, r);
        assert_eq!(col.rect.w, w.min(1000 - 2 * r));
    }

    /// Requirement: the stretched background must not separate from the bar. The colour is translucent, so
    /// painting it twice over the bar (once by the bar, once by the card) would darken the overlap; the card
    /// content must not paint any background inside the bar at all.
    #[test]
    fn the_column_paints_no_background_inside_the_bar() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Subject", "one\ntwo");
        let w = card_width(&n, &mut text, &t) + 20;
        let h = 90;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let bar = Rect::new(0, 0, w, t.height);
        let col = laid_out(&[n.clone()], 10, w - 20, w, &t, &mut text);
        {
            let mut c = Canvas::new(&mut buf, w, h);
            // The bar surface paints the bar first, exactly as the real one does; then the card is drawn on top.
            c.fill_rounded_rect(bar, t.radius, t.background);
            render_column(&mut c, col.rect, t.height, col.rect, &t);
            c.set_clip(Some(col.rect));
            render(&mut c, &col.cards[0], 1.0, &n, &t, &mut text);
        }
        let px = |x: i32, y: i32| buf[((y * w + x) * 4) as usize];
        let alpha = |x: i32, y: i32| buf[((y * w + x) * 4) as usize + 3];
        // Outside the column, inside the bar: one background layer.
        let reference = alpha(1, t.height / 2);
        assert_eq!(reference, t.background.a, "the bar's background is there to compare against");
        // Inside the column's span, inside the bar: still exactly one layer, not two.
        let inside = col.rect.right() - 4;
        assert_eq!(alpha(inside, t.height / 2), reference, "the card must not paint a second background layer over the bar");
        // Below the bar the column does paint, and the shape is solid in its middle.
        assert_eq!(alpha(inside, t.height + 4), t.background.a, "the stretched part is filled");
        assert_ne!(px(inside, t.height + 4), 0, "…with the background colour");
    }

    /// The join must be tight in the vertical direction too: no transparent row between the bar and the column.
    #[test]
    fn no_gap_between_the_bar_and_the_column() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Subject", "one\ntwo");
        let w = 300;
        let h = 90;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let col = laid_out(&[n], 10, w - 20, w, &t, &mut text);
        {
            let mut c = Canvas::new(&mut buf, w, h);
            c.fill_rounded_rect(Rect::new(0, 0, w, t.height), t.radius, t.background);
            render_column(&mut c, col.rect, t.height, col.rect, &t);
        }
        let mid = col.rect.x + col.rect.w / 2;
        for y in (t.height - 2)..(t.height + 4) {
            let a = buf[((y * w + mid) * 4) as usize + 3];
            assert_ne!(a, 0, "row {y} is transparent: the column came apart from the bar");
        }
    }

    /// The head card's ink sits one card_padding above the shape's bottom edge, so the text block does not read
    /// as sitting high in its box; the top of the block is the bar's own line.
    #[test]
    fn card_text_keeps_equal_optical_padding() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        // no descenders, so the ink bottom is the baseline: padding below the baseline is what gets measured
        let n = note(1, "Hi", "no descenders");
        let w = card_width(&n, &mut text, &t) + 20;
        let h = 90;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let col = laid_out(&[n.clone()], 10, w - 20, w, &t, &mut text);
        {
            let mut c = Canvas::new(&mut buf, w, h);
            c.set_clip(Some(col.rect));
            render(&mut c, &col.cards[0], 1.0, &n, &t, &mut text);
        }
        let ink_rows: Vec<i32> = (0..h)
            .filter(|y| (0..w).any(|x| buf[((y * w + x) * 4) as usize] > 150))
            .collect();
        let last = *ink_rows.last().unwrap();
        assert_eq!(last, col.rect.bottom() - t.card_padding - 1, "ink bottom must sit card_padding above the shape's bottom edge");
        let first = *ink_rows.first().unwrap();
        let m = text.cap_metrics(&t.font);
        assert_eq!(first, col.cards[0].summary + m.top.round() as i32, "the head summary sits on the bar's text line");
    }

    /// A card below the head one is padded from its own band top, at both ends.
    #[test]
    fn a_lower_card_is_padded_at_both_ends() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let head = note(1, "Head", "");
        let second = note(2, "Hi", "no descenders");
        let w = slot_width(&[head.clone(), second.clone()], &mut text, &t) + 20;
        let h = 140;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let col = laid_out(&[head, second.clone()], 10, w - 20, w, &t, &mut text);
        let card = &col.cards[1];
        {
            let mut c = Canvas::new(&mut buf, w, h);
            c.set_clip(Some(col.rect));
            render(&mut c, card, 1.0, &second, &t, &mut text);
        }
        let ink_rows: Vec<i32> = (0..h)
            .filter(|y| (0..w).any(|x| buf[((y * w + x) * 4) as usize] > 150))
            .collect();
        assert_eq!(*ink_rows.first().unwrap(), card.band.y + t.card_padding);
        assert_eq!(*ink_rows.last().unwrap(), card.band.bottom() - t.card_padding - 1);
    }

    /// The stretch never moves sideways and never leaves the bar's row: y = 0 at every step, height monotone.
    #[test]
    fn the_stretch_grows_downwards_only() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Subject", "one\ntwo\nthree");
        let w = card_width(&n, &mut text, &t);
        let col = laid_out(&[n], 0, w, 1000, &t, &mut text);
        let start = Rect::new(col.rect.x, 0, col.rect.w, t.height);
        let now = Instant::now();
        let tw = Tween::new(now, 200);
        let at = |ms: u64| stretch(start, col.rect, Easing::OutCubic, &tw, now + std::time::Duration::from_millis(ms));
        assert_eq!(at(0), start, "t=0 is the bar's own row: nothing has been stretched yet");
        assert_eq!(at(200), col.rect, "t=1 is exactly the laid-out column");
        for ms in [10, 50, 100, 150, 190] {
            let r = at(ms);
            assert_eq!((r.y, r.x, r.w), (0, col.rect.x, col.rect.w), "the shape stretches, it does not slide: {r:?}");
            assert!(r.h >= t.height && r.h <= col.rect.h, "height only grows: {r:?}");
        }
        // The exit is the same tween reversed and may not grow the shape.
        let back = stretch(col.rect, start, Easing::InOutCubic, &tw, now + std::time::Duration::from_millis(200));
        assert_eq!(back.h, start.h);
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
    fn card_width_is_capped_and_floored() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let w = card_width(&note(1, "s", "b"), &mut text, &t);
        assert!(w >= MIN_CARD_W && w <= MAX_CARD_W, "the card has a width range, got {w}");
        let wide = card_width(&note(1, &"x".repeat(600), ""), &mut text, &t);
        assert_eq!(wide, MAX_CARD_W);
        // The bar reserves the widest card of the stack and nothing while the queue is empty.
        let mut slot = TextEngine::new();
        assert_eq!(slot_width(&[], &mut slot, &t), 0);
        assert_eq!(slot_width(&[note(1, "s", "b"), note(2, &"x".repeat(600), "")], &mut slot, &t), MAX_CARD_W);
    }

    /// The surface has to be tall enough for every frame of the animation; a card is at most
    /// `MAX_BODY_LINES` body rows plus a summary and an action row.
    #[test]
    fn max_tail_bounds_every_card() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let body = (0..crate::notify::queue::MAX_BODY_LINES).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let mut n = note(1, "Subject", &body);
        n.actions = vec![("open".into(), "Open".into())];
        let max_visible = 4;
        let mut text2 = TextEngine::new();
        let col = column(
            &std::iter::repeat_n(n, max_visible).collect::<Vec<_>>(),
            0,
            MAX_CARD_W,
            1280,
            &t,
            &mut text,
        );
        assert!(col.rect.h - t.height <= max_tail(max_visible, &t, &mut text2), "column {} vs bound {}", col.rect.h - t.height, max_tail(max_visible, &t, &mut text2));
    }
}

//! Notification stack layout, drawing and hit testing. The notification surface only supplies the stretch.

use std::time::Instant;

use crate::anim::{Easing, Tween};
use crate::canvas::Canvas;
use crate::geom::{Color, Rect};
use crate::notify::queue::{Notification, Urgency};
use crate::text::TextEngine;
use crate::theme::Theme;
use crate::widget::Action;

pub fn urgency_color(u: Urgency, theme: &Theme) -> Color {
    match u {
        Urgency::Low | Urgency::Normal => theme.foreground,
        Urgency::Critical => Color::rgba(0xff, 0x5f, 0x56, 0xff),
    }
}

/// One line's advance; every row in a card steps by it.
fn line_height(theme: &Theme) -> i32 { (theme.font.size * 1.35).ceil() as i32 }

/// Body rows a card draws. The queue already caps the body, but a hand-built `Notification` may exceed it.
fn body_lines(n: &Notification) -> usize {
    if n.body.is_empty() { 0 } else { n.body.lines().count().min(crate::notify::queue::MAX_BODY_LINES) }
}

/// Width the bar reserves for the notification module: one card, 0 while nothing is visible. Every card is the
/// same size — Apple's expanded Live Activity is a fixed 371×84–160 pt rather than a shrink-wrap of its content —
/// so the bar's layout does not move again while the stack is up.
pub fn slot_width(visible: &[Notification], theme: &Theme) -> i32 {
    if visible.is_empty() { 0 } else { theme.card_w }
}

/// The stack's horizontal span is clamped into `[r, output_w - r]`: that is where the bar's bottom edge is
/// straight. Under the pill's curve the head card's square top corners would leave a notch between the two
/// silhouettes, i.e. the background *would* come apart at the join.
fn straight_band(x: i32, w: i32, output_w: i32, theme: &Theme) -> (i32, i32) {
    let r = theme.radius.clamp(0, theme.height / 2);
    let x = x.clamp(r, (output_w - r).max(r));
    (x, w.min(output_w - r - x).max(0))
}

/// One visible notification: its own card, newest at the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// The card's rect: background, hit rect. The head card's top edge is the bar's top.
    pub rect: Rect,
    /// Box top to hand to `draw` for the summary; the head card puts it on the bar's own text line.
    pub summary: i32,
    /// Box top of the first body line.
    pub body: i32,
    /// The action row's pill top; the pill is one line tall.
    pub actions: Option<i32>,
}

/// The stack: the head card hanging off the bar, every later notification its own card below it.
#[derive(Clone, Debug, PartialEq)]
pub struct Stack {
    /// The stretched extent. Its top is the bar's top, so the overlap with the bar is invisible.
    pub rect: Rect,
    pub cards: Vec<Card>,
}

/// Lay the visible notifications out. The head card's summary shares the bar's text line and only the rows that do
/// not fit in the bar stretch below it; macOS-style, each later notification is its own rounded card below that,
/// newest first, with `card_gap` between the cards.
pub fn stack(visible: &[Notification], slot_x: i32, output_w: i32, theme: &Theme, text: &mut TextEngine) -> Stack {
    let (x, w) = straight_band(slot_x, theme.card_w, output_w, theme);
    let line = line_height(theme);
    let m = text.cap_metrics(&theme.font);
    let mut cards: Vec<Card> = Vec::with_capacity(visible.len());
    let mut top = 0;
    for (i, n) in visible.iter().enumerate() {
        let body = body_lines(n);
        // The head card shares the bar's line; every later card pads its summary from its own top edge.
        let summary = if i == 0 { text.optical_top(&theme.font, 0, theme.height) } else { text.cap_top(&theme.font, top, theme.card_padding) };
        let mut next = summary; // box top of the row after the last one drawn
        let mut last_ink = summary + m.top.round() as i32 + m.cap.ceil() as i32; // ink bottom of the last row drawn
        for _ in 0..body {
            next += line;
            last_ink = next + m.top.round() as i32 + m.cap.ceil() as i32;
        }
        let actions = if n.actions.is_empty() {
            None
        } else {
            next += theme.card_gap / 2;
            last_ink = next + line;
            Some(next)
        };
        // A head card whose rows fit in the bar owns just the bar's row and stretches nowhere. Once a row lands
        // below the bar, the card keeps its bottom padding, so it ends `card_padding` below the last baseline.
        let bottom = if i == 0 && last_ink <= theme.height { theme.height } else { last_ink + theme.card_padding };
        cards.push(Card { rect: Rect::new(x, top, w, bottom - top), summary, body: summary + line, actions });
        top = bottom + theme.card_gap;
    }
    let h = cards.last().map_or(theme.height, |c| c.rect.bottom());
    Stack { rect: Rect::new(x, 0, w, h), cards }
}

/// Paint one card's background. Nothing above the bar's bottom edge is painted for the head card: the bar already
/// painted that area, and `theme.background` is translucent, so a second pass would darken the overlap and the
/// stretch would read as a separate layer glued under the bar. Its top corners stay square for the same reason its
/// span is clamped — a rounded corner at the junction leaves a notch.
pub fn render_card_background(canvas: &mut Canvas, card: &Card, head: bool, bar_bottom: i32, clip: Rect, theme: &Theme) {
    let top = if head { bar_bottom } else { card.rect.y };
    let area = clip.intersect(Rect::new(card.rect.x, top, card.rect.w, card.rect.bottom() - top));
    if area.is_empty() {
        return;
    }
    let r = theme.card_radius;
    // Shifting the shape up by `r` puts its top corners above the clip, so an attached card keeps straight sides
    // where it meets the bar while its bottom corners are as round as the shape language wants.
    let shape = if head { Rect::new(card.rect.x, card.rect.y - r, card.rect.w, card.rect.h + r) } else { card.rect };
    canvas.set_clip(Some(area));
    canvas.fill_rounded_rect(shape, r, theme.background);
    canvas.set_clip(Some(clip));
}

/// Draw one card's rows and return its button hit rects. The caller owns the clip: text is clipped to the
/// animating shape, which is still shorter than the laid-out rows while the stack opens.
///
/// `alpha` applies to the text only: the shape is the material that is already there, so the background stays
/// opaque while it stretches and only the text follows — design §7: "the capsule opens into a box, then the text
/// floats out".
pub fn render(canvas: &mut Canvas, card: &Card, alpha: f32, n: &Notification, theme: &Theme, text: &mut TextEngine) -> Vec<(Rect, Action)> {
    let fa = |c: Color| Color::rgba(c.r, c.g, c.b, (c.a as f32 * alpha.clamp(0.0, 1.0)) as u8);
    let line = line_height(theme);
    let x = card.rect.x + theme.card_padding;
    // Inner width: all text is truncated to the card; summary is untrusted D-Bus input and must be truncated.
    let inner_w = (card.rect.w - theme.card_padding * 2).max(0) as f32;

    // Apple's notification anatomy: the title in the primary label colour, the body in the secondary one.
    let summary = crate::text::truncate_to_width(&n.summary, inner_w, |t: &str| text.measure(t, &theme.font).0);
    text.draw(canvas, &summary, x, card.summary, &theme.font, fa(urgency_color(n.urgency, theme)));
    let mut y = card.body;
    for body_line in n.body.lines().take(crate::notify::queue::MAX_BODY_LINES) {
        let t = crate::text::truncate_to_width(body_line, inner_w, |t: &str| text.measure(t, &theme.font).0);
        text.draw(canvas, &t, x, y, &theme.font, fa(theme.secondary));
        y += line;
    }

    let mut hits = Vec::new();
    if let Some(ay) = card.actions {
        let mut bx = x;
        for (key, label) in &n.actions {
            let t = crate::text::truncate_to_width(label, inner_w, |t: &str| text.measure(t, &theme.font).0);
            let (w, _) = text.measure(&t, &theme.font);
            // The pill is one line tall, so Apple's concentric rule (inner radius = outer radius − margin) clamps
            // to a capsule here anyway — the shape Apple's own short buttons take.
            let br = Rect::new(bx, ay, (w.ceil() as i32 + theme.card_padding).min(card.rect.w).max(0), line);
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

/// Upper bound of the stack's extent below the bar, used to size the notification surface once: a resize per
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

    fn laid_out(visible: &[Notification], slot_x: i32, output_w: i32, theme: &Theme, text: &mut TextEngine) -> Stack {
        stack(visible, slot_x, output_w, theme, text)
    }

    /// Full width of the stack's slot, as the bar would lay it out for a right-section module.
    fn slot_x(output_w: i32, theme: &Theme) -> i32 { output_w - theme.padding - theme.card_w }

    /// "The first card is shown in the bar": a card whose rows fit inside the bar stretches nowhere.
    #[test]
    fn a_short_card_stays_inside_the_bar() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Hi", "");
        let x = slot_x(1000, &t);
        let s = laid_out(&[n], x, 1000, &t, &mut text);
        assert_eq!(s.rect.h, t.height, "nothing is stretched below the bar: {s:?}");
        assert_eq!(s.cards.len(), 1);
        let head = &s.cards[0];
        assert_eq!(head.rect, Rect::new(s.rect.x, 0, s.rect.w, t.height));
        assert_eq!(head.rect.w, s.rect.w, "every card is the same width");
        // The summary sits on the bar's own text line, not on a card padding line.
        assert_eq!(head.summary, text.optical_top(&t.font, 0, t.height));
    }

    /// "If it is too long, it stretches downwards": rows that do not fit in the bar extend the same card.
    #[test]
    fn a_long_card_stretches_below_the_bar() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let line = line_height(&t);
        let m = text.cap_metrics(&t.font);
        let (cap, ink_off) = (m.cap.ceil() as i32, m.top.round() as i32);
        let n = note(1, "Subject", "one\ntwo\nthree");
        let s = laid_out(&[n], 0, 1000, &t, &mut text);
        assert!(s.rect.h > t.height, "three body rows must not fit in a 30px bar: {s:?}");
        let head = &s.cards[0];
        assert_eq!(head.body, head.summary + line, "body rows keep the text rhythm from the bar's line");
        assert_eq!(head.summary + ink_off, t.height / 2 - cap / 2, "the head summary is cap-centred on the bar's line");
        // The last row is the third body row: its ink bottom plus one card_padding is the card's bottom.
        assert_eq!(
            head.rect.bottom(),
            head.body + 2 * line + ink_off + cap + t.card_padding,
            "the card ends one card_padding below the last baseline"
        );
        assert_eq!(s.rect.bottom(), head.rect.bottom());
    }

    /// "A second notification keeps stretching downwards": it is its own card, `card_gap` below the head one.
    #[test]
    fn the_stack_grows_downwards_card_by_card() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let a = note(1, "First", "body");
        let b = note(2, "Second", "");
        let s = laid_out(&[a, b], 0, 1000, &t, &mut text);
        assert_eq!(s.cards.len(), 2);
        assert_eq!(s.cards[1].rect.y, s.cards[0].rect.bottom() + t.card_gap);
        assert_eq!(s.rect.bottom(), s.cards[1].rect.bottom(), "the stretch covers every card");
        assert_eq!(s.rect.y, 0, "the shape starts at the bar's top and its overlap with the bar is invisible");
        // Apple's expanded Live Activity is one fixed size, so a one-word card and a three-line card are equally
        // wide and the stack is a column.
        assert!(s.cards.iter().all(|c| c.rect.w == t.card_w && c.rect.x == s.rect.x));
    }

    /// The stretch may only attach where the bar's bottom edge is straight, or its square top corners would
    /// leave a notch under the pill's curve.
    #[test]
    fn the_stack_attaches_to_the_straight_part_of_the_bar() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Wide", "");
        let r = t.radius;
        // Right section: the module's slot runs into the pill's rounded end.
        let s = laid_out(&[n.clone()], slot_x(1000, &t), 1000, &t, &mut text);
        assert_eq!(s.rect.right(), 1000 - r, "the stack stops where the pill's bottom edge starts to curve");
        // Left section, same mirror: padding alone would put the stack inside the corner curve.
        let s = laid_out(&[n], t.padding, 1000, &t, &mut text);
        assert_eq!(s.rect.x, r);
        assert_eq!(s.rect.w, t.card_w.min(1000 - 2 * r));
    }

    /// Requirement: the stretched background must not separate from the bar. The colour is translucent, so
    /// painting it twice over the bar (once by the bar, once by the card) would darken the overlap; the card
    /// content must not paint any background inside the bar at all.
    #[test]
    fn the_head_card_paints_no_background_inside_the_bar() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Subject", "one\ntwo");
        let w = t.card_w;
        let h = 120;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let bar = Rect::new(0, 0, w, t.height);
        let s = laid_out(&[n.clone()], 0, w, &t, &mut text);
        {
            let mut c = Canvas::new(&mut buf, w, h);
            // The bar surface paints the bar first, exactly as the real one does; then the card is drawn on top.
            c.fill_rounded_rect(bar, t.radius, t.background);
            render_card_background(&mut c, &s.cards[0], true, t.height, s.rect, &t);
            c.set_clip(Some(s.rect));
            render(&mut c, &s.cards[0], 1.0, &n, &t, &mut text);
        }
        let alpha = |x: i32, y: i32| buf[((y * w + x) * 4) as usize + 3];
        let px = |x: i32, y: i32| buf[((y * w + x) * 4) as usize];
        // Outside the stack, inside the bar: one background layer, to compare against.
        let reference = alpha(1, t.height / 2);
        assert_eq!(reference, t.background.a, "the bar's background is there to compare against");
        // Inside the card's span, inside the bar: still exactly one layer, not two.
        let inside = s.rect.right() - 4;
        assert_eq!(alpha(inside, t.height / 2), reference, "the card must not paint a second background layer over the bar");
        // Below the bar the card does paint, and the shape is solid in its middle.
        assert_eq!(alpha(inside, t.height + 4), t.background.a, "the stretched part is filled");
        assert_ne!(px(inside, t.height + 4), 0, "…with the background colour");
    }

    /// The join must be tight vertically too: no transparent row between the bar and the card.
    #[test]
    fn no_gap_between_the_bar_and_the_card() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Subject", "one\ntwo");
        let w = t.card_w;
        let h = 120;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let s = laid_out(&[n], 0, w, &t, &mut text);
        {
            let mut c = Canvas::new(&mut buf, w, h);
            c.fill_rounded_rect(Rect::new(0, 0, w, t.height), t.radius, t.background);
            render_card_background(&mut c, &s.cards[0], true, t.height, s.rect, &t);
        }
        let mid = s.rect.x + s.rect.w / 2;
        for y in (t.height - 2)..(t.height + 4) {
            let a = buf[((y * w + mid) * 4) as usize + 3];
            assert_ne!(a, 0, "row {y} is transparent: the card came apart from the bar");
        }
        // …and the corners too: the card's sides stay straight right up to the bar's bottom edge.
        for y in (t.height - 1)..(t.height + 2) {
            for x in [s.rect.x, s.rect.right() - 1] {
                let a = buf[((y * w + x) * 4) as usize + 3];
                assert_ne!(a, 0, "the corner at {x},{y} is transparent: a rounded top corner left a notch");
            }
        }
    }

    /// macOS stacks notifications as separate cards, so the gap between two of them is background — the cards do
    /// not merge into one shape.
    #[test]
    fn cards_below_the_head_are_their_own_card() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let head = note(1, "Head", "one");
        let second = note(2, "Second", "two");
        let w = t.card_w;
        let h = 200;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let s = laid_out(&[head, second], 0, w, &t, &mut text);
        {
            let mut c = Canvas::new(&mut buf, w, h);
            render_card_background(&mut c, &s.cards[0], true, t.height, s.rect, &t);
            render_card_background(&mut c, &s.cards[1], false, t.height, s.rect, &t);
        }
        let mid = s.rect.x + s.rect.w / 2;
        let gap_y = s.cards[0].rect.bottom() + t.card_gap / 2;
        let a = |y: i32| buf[((y * w + mid) * 4) as usize + 3];
        assert_eq!(a(gap_y), 0, "the gap between two cards must stay transparent, got {} at {gap_y}", a(gap_y));
        assert_ne!(a(s.cards[1].rect.y + t.card_padding), 0, "the second card is painted");
        assert_ne!(a(s.cards[0].rect.bottom() - 1), 0, "the head card is painted down to its own edge");
    }

    /// The head card's ink sits one card_padding above its bottom edge, so the text block does not read as
    /// sitting high in its box; the top of the block is the bar's own line.
    #[test]
    fn card_text_keeps_equal_optical_padding() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        // no descenders, so the ink bottom is the baseline: padding below the baseline is what gets measured
        let n = note(1, "Hi", "no descenders");
        let w = t.card_w;
        let h = 140;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let s = laid_out(&[n.clone()], 0, w, &t, &mut text);
        {
            let mut c = Canvas::new(&mut buf, w, h);
            c.set_clip(Some(s.rect));
            render(&mut c, &s.cards[0], 1.0, &n, &t, &mut text);
        }
        let ink_rows: Vec<i32> = (0..h)
            .filter(|y| (0..w).any(|x| buf[((y * w + x) * 4) as usize] > 60))
            .collect();
        let last = *ink_rows.last().unwrap();
        assert_eq!(last, s.rect.bottom() - t.card_padding - 1, "ink bottom must sit card_padding above the card's bottom edge");
        let first = *ink_rows.first().unwrap();
        let m = text.cap_metrics(&t.font);
        assert_eq!(first, s.cards[0].summary + m.top.round() as i32, "the head summary sits on the bar's text line");
    }

    /// A card below the head one is padded from its own top edge, at both ends.
    #[test]
    fn a_lower_card_is_padded_at_both_ends() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let head = note(1, "Head", "");
        let second = note(2, "Hi", "no descenders");
        let w = t.card_w;
        let h = 200;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let s = laid_out(&[head, second.clone()], 0, w, &t, &mut text);
        let card = &s.cards[1];
        {
            let mut c = Canvas::new(&mut buf, w, h);
            c.set_clip(Some(s.rect));
            render(&mut c, card, 1.0, &second, &t, &mut text);
        }
        let ink_rows: Vec<i32> = (0..h)
            .filter(|y| (0..w).any(|x| buf[((y * w + x) * 4) as usize] > 60))
            .collect();
        assert_eq!(*ink_rows.first().unwrap(), card.rect.y + t.card_padding);
        assert_eq!(*ink_rows.last().unwrap(), card.rect.bottom() - t.card_padding - 1);
    }

    /// The stretch never moves sideways and never leaves the bar's row: y = 0 at every step, and a spring may
    /// overshoot the target height slightly as it lands.
    #[test]
    fn the_stretch_grows_downwards_only() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let n = note(1, "Subject", "one\ntwo\nthree");
        let s = laid_out(&[n], 0, 1000, &t, &mut text);
        let start = Rect::new(s.rect.x, 0, s.rect.w, t.height);
        let now = Instant::now();
        let tw = Tween::new(now, 200);
        let at = |ms: u64| stretch(start, s.rect, Easing::Spring, &tw, now + std::time::Duration::from_millis(ms));
        assert_eq!(at(0), start, "t=0 is the bar's own row: nothing has been stretched yet");
        // A spring lands within a pixel of the target; the tween is dropped at t=1 and the laid-out stack is used.
        assert!((at(200).h - s.rect.h).abs() <= 1, "t=1 is the laid-out stack: {}", at(200).h);
        for ms in [10, 50, 100, 150, 190] {
            let r = at(ms);
            assert_eq!((r.y, r.x, r.w), (0, s.rect.x, s.rect.w), "the shape stretches, it does not slide: {r:?}");
            assert!(r.h >= t.height - 1, "the stretch never closes past the bar's row: {r:?}");
            assert!(r.h <= s.rect.h + (s.rect.h - t.height) / 10, "the overshoot stays small: {r:?}");
        }
        // The exit is the same tween reversed and must not grow the shape.
        let back = stretch(s.rect, start, Easing::Smooth, &tw, now + std::time::Duration::from_millis(200));
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
    fn the_card_is_one_derived_size() {
        let t = Theme::defaults(30);
        // The bar reserves one card while anything is visible, and nothing at all while the queue is empty.
        assert_eq!(slot_width(&[], &t), 0);
        assert_eq!(slot_width(&[note(1, "s", "b")], &t), t.card_w);
        assert_eq!(slot_width(&[note(1, "s", "b"), note(2, &"x".repeat(600), "")], &t), t.card_w);
        // A very long summary is truncated by the card's inner width, not by the card growing.
        let mut text = TextEngine::new();
        let s = laid_out(&[note(1, &"x".repeat(600), "")], 0, 1000, &t, &mut text);
        assert_eq!(s.rect.w, t.card_w);
        assert!(t.card_w >= crate::theme::MIN_CARD_W && t.card_w <= crate::theme::MAX_CARD_W);
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
        let s = stack(&std::iter::repeat_n(n, max_visible).collect::<Vec<_>>(), 0, 1280, &t, &mut text);
        let tail = s.rect.h - t.height;
        assert!(tail <= max_tail(max_visible, &t, &mut text2), "stack tail {tail} vs bound {}", max_tail(max_visible, &t, &mut text2));
    }
}

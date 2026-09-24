//! Notification stack layout, drawing and hit testing. The notification surface only supplies the stretch.

use std::time::Instant;

use crate::anim::{Easing, Tween};
use crate::canvas::Canvas;
use crate::geom::{Color, Rect};
use crate::notify::queue::{Notification, Urgency};
use crate::text::TextEngine;
use crate::theme::Theme;
use crate::widget::Action;

/// The stack is a screen-space budget, not a proportion of the bar. A 24" 1920×1080 at 60 cm viewing distance
/// spans 47.7° horizontally but only 28.0° vertically, so on a 16:9 output the vertical axis is the scarce one
/// and the notification stack may borrow at most this share of it (see `max_tail`).
const MAX_TAIL_PERCENT: i32 = 25;
const ACTION_FILL_PERCENT: u16 = 22;
const DIVIDER_ALPHA_PERCENT: u16 = 32;

pub fn urgency_color(u: Urgency, theme: &Theme) -> Color {
    match u {
        Urgency::Low | Urgency::Normal => theme.foreground,
        Urgency::Critical => Color::rgba(0xff, 0x5f, 0x56, 0xff),
    }
}

/// One line's advance; every row in a card steps by it.
fn line_height(theme: &Theme) -> i32 { (theme.font.size * 1.35).ceil() as i32 }

/// A separator thick enough to read at a glance while still following the card's spacing rhythm.
fn divider_height(theme: &Theme) -> i32 { (theme.card_gap / 3).max(1) }

/// Body rows a card draws. The queue already caps the body, but a hand-built `Notification` may exceed it.
fn body_lines(n: &Notification) -> usize {
    if n.body.is_empty() { 0 } else { n.body.lines().count().min(crate::notify::queue::MAX_BODY_LINES) }
}

/// A headline field or action is one visual row even when an untrusted client embeds a newline in it.
fn first_line(text: &str) -> &str { text.lines().next().unwrap_or("") }

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

/// One drawn notification: the head card, or the peek pill of the collapsed remainder. Newest at the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// The card's rect: background, hit rect. The head card's top edge is the bar's top.
    pub rect: Rect,
    /// Box top to hand to `draw` for the summary; the head card puts it on the bar's own text line.
    pub summary: i32,
    /// Box top of the first body line.
    pub body: i32,
    /// Separator between the headline and the first detail row.
    pub divider: Option<i32>,
    /// The action row's pill top; the pill is one line tall.
    pub actions: Option<i32>,
    /// The collapsed peek pill: one bar tall with the next summary and the number it represents.
    pub collapsed: Option<usize>,
}

/// The stack: the head card hanging off the bar, and behind it the collapsed peek pill.
#[derive(Clone, Debug, PartialEq)]
pub struct Stack {
    /// The stretched extent. Its top is the bar's top, so the overlap with the bar is invisible.
    pub rect: Rect,
    pub cards: Vec<Card>,
}

/// Lay the visible notifications out: the head card, plus one collapsed peek pill for everything behind it.
/// The head card's summary shares the bar's text line, so only the rows that do not fit in the bar stretch below
/// it; a separate full card per notification would grow the stack along the screen's scarce axis (see
/// `MAX_TAIL_PERCENT`), so everything past the first is one pill that previews the next notification and counts the remainder.
pub fn stack(visible: &[Notification], queued: usize, slot_x: i32, output_w: i32, theme: &Theme, text: &mut TextEngine) -> Stack {
    let (x, w) = straight_band(slot_x, theme.card_w, output_w, theme);
    let line = line_height(theme);
    let m = text.cap_metrics(&theme.font);
    let mut cards: Vec<Card> = Vec::with_capacity(2);
    let Some(head) = visible.first() else {
        return Stack { rect: Rect::new(x, 0, w, theme.height), cards };
    };
    // The head card shares the bar's own text line. A card whose rows fit in the bar owns just that row and
    // stretches nowhere; once a row lands below the bar the card keeps its bottom padding, so it ends
    // `card_padding` below the last baseline.
    let summary = text.optical_top(&theme.font, 0, theme.height);
    let mut body = summary + line;
    let mut next = body; // box top of the row after the last one drawn
    let mut last_ink = summary + m.top.round() as i32 + m.cap.ceil() as i32; // ink bottom of the last row drawn
    let mut divider = None;
    let body_rows = body_lines(head);
    if body_rows > 0 {
        next = next.max(theme.height);
        next += theme.card_gap / 2;
        divider = Some(next);
        next += divider_height(theme);
        body = next;
        for _ in 0..body_rows {
            last_ink = next + m.top.round() as i32 + m.cap.ceil() as i32;
            next += line;
        }
    }
    let actions = if head.actions.iter().any(|(_, label)| !first_line(label).trim().is_empty()) {
        next = next.max(theme.height);
        next += theme.card_gap / 2;
        if divider.is_none() {
            divider = Some(next);
            next += divider_height(theme);
        }
        last_ink = next + line;
        Some(next)
    } else {
        None
    };
    let bottom = if last_ink <= theme.height { theme.height } else { last_ink + theme.card_padding };
    cards.push(Card { rect: Rect::new(x, 0, w, bottom), summary, body, divider, actions, collapsed: None });
    // The collapsed remainder: the bar's own compact shape — one bar tall, `radius` corners — carrying the next
    // notification's summary and the total count. Entries past it stay in the queue and slide in as the ones ahead go.
    if visible.len() > 1 {
        let y = bottom + theme.card_gap;
        let top = text.optical_top(&theme.font, y, theme.height);
        cards.push(Card { rect: Rect::new(x, y, w, theme.height), summary: top, body: top, divider: None, actions: None, collapsed: Some(queued.saturating_sub(1)) });
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
    // The peek pill is the bar's shape, not a card: one bar tall, so `theme.radius` — a capsule — closes it.
    let r = if card.collapsed.is_some() { theme.radius } else { theme.card_radius };
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
    let right = card.rect.right() - theme.card_padding;
    let inner_w = (right - x).max(0) as f32;

    // The collapsed stack previews the next notification and keeps an explicit count, so a deep queue never
    // looks like a single undifferentiated card.
    if let Some(remaining) = card.collapsed {
        let count = format!("×{remaining}");
        let count = crate::text::truncate_to_width(&count, inner_w, |t: &str| text.measure(t, &theme.font).0);
        let count_w = text.measure(&count, &theme.font).0.ceil() as i32;
        let count_x = right - count_w;
        let summary_w = (count_x - theme.card_gap - x).max(0) as f32;
        let summary = crate::text::truncate_to_width(first_line(&n.summary), summary_w, |t: &str| text.measure(t, &theme.font).0);
        text.draw(canvas, &summary, x, card.summary, &theme.font, fa(theme.secondary));
        text.draw(canvas, &count, count_x, card.summary, &theme.font, fa(theme.accent));
        return Vec::new();
    }

    // The source label takes the place of the app icon cornice cannot render. It stays quiet and yields the
    // title most of the row, matching Apple's title-first hierarchy without competing with the headline.
    let source = first_line(n.app_name.trim());
    let source = crate::text::truncate_to_width(source, inner_w / 3.0, |t: &str| text.measure(t, &theme.font).0);
    let source_w = if source.is_empty() { 0 } else { text.measure(&source, &theme.font).0.ceil() as i32 };
    let source_x = right - source_w;
    let summary_w = if source.is_empty() { inner_w } else { (source_x - theme.card_gap - x).max(0) as f32 };
    let summary = crate::text::truncate_to_width(first_line(&n.summary), summary_w, |t: &str| text.measure(t, &theme.font).0);
    text.draw(canvas, &summary, x, card.summary, &theme.font, fa(urgency_color(n.urgency, theme)));
    if !source.is_empty() {
        text.draw(canvas, &source, source_x, card.summary, &theme.font, fa(theme.secondary));
    }
    if let Some(y) = card.divider {
        let divider = Color::rgba(theme.secondary.r, theme.secondary.g, theme.secondary.b, (theme.secondary.a as u16 * DIVIDER_ALPHA_PERCENT / 100) as u8);
        canvas.fill_rect(Rect::new(x, y, inner_w as i32, divider_height(theme)), fa(divider));
    }

    let mut y = card.body;
    for body_line in n.body.lines().take(crate::notify::queue::MAX_BODY_LINES) {
        let body = crate::text::truncate_to_width(body_line, inner_w, |t: &str| text.measure(t, &theme.font).0);
        text.draw(canvas, &body, x, y, &theme.font, fa(theme.secondary));
        y += line;
    }

    let mut hits = Vec::new();
    if let Some(ay) = card.actions {
        let action_pad = (theme.card_padding / 2).max(2);
        let fill = Color::rgba(theme.accent.r, theme.accent.g, theme.accent.b, (theme.accent.a as u16 * ACTION_FILL_PERCENT / 100) as u8);
        let mut bx = x;
        for (key, label) in &n.actions {
            let label = first_line(label).trim();
            let available = right - bx;
            if label.is_empty() {
                continue;
            }
            if available < line + action_pad * 2 {
                break;
            }
            let label = crate::text::truncate_to_width(label, (available - action_pad * 2) as f32, |t: &str| text.measure(t, &theme.font).0);
            let label_w = text.measure(&label, &theme.font).0.ceil() as i32;
            let br = Rect::new(bx, ay, (label_w + action_pad * 2).min(available), line);
            canvas.fill_rounded_rect(br, line / 2, fill);
            let ty = text.optical_top(&theme.font, br.y, br.h);
            text.draw(canvas, &label, bx + (br.w - label_w) / 2, ty, &theme.font, fa(theme.accent));
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
/// animation frame would stutter, so the surface must already be tall enough for every frame. It is capped at
/// `MAX_TAIL_PERCENT` of the output's height: `bar.height` is a config knob (1..=256) and on a short screen the
/// screen, not the bar, has to decide how much of it a notification may cover. Anything past the cap is clipped
/// by the surface; there is no other way for it to fail. `output_h` is `None` when the compositor advertises
/// neither a logical size nor a current mode, which leaves the derived bound uncapped.
pub fn max_tail(theme: &Theme, text: &mut TextEngine, output_h: Option<i32>) -> i32 {
    let line = line_height(theme);
    let cap = text.cap_metrics(&theme.font).cap.ceil() as i32;
    let summary = text.optical_top(&theme.font, 0, theme.height);
    let below_bar = (theme.height - summary - line).max(0);
    let worst_card = theme.card_padding * 2 + cap + line * (crate::notify::queue::MAX_BODY_LINES as i32 + 1) + below_bar + theme.card_gap + divider_height(theme);
    let tail = worst_card + theme.card_gap + theme.height; // head card, gap, peek pill
    match output_h {
        Some(h) if h > 0 => tail.min(h * MAX_TAIL_PERCENT / 100),
        _ => tail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32, summary: &str, body: &str) -> Notification {
        Notification {
            id,
            app_name: "Test".into(),
            summary: summary.into(),
            body: body.into(),
            urgency: Urgency::Normal,
            expire: None,
            actions: vec![],
            created: Instant::now(),
        }
    }

    fn laid_out(visible: &[Notification], slot_x: i32, output_w: i32, theme: &Theme, text: &mut TextEngine) -> Stack {
        stack(visible, visible.len(), slot_x, output_w, theme, text)
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
        assert_eq!(head.divider, None, "a compact bar-only card has no detail separator");
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
        let divider = head.divider.expect("a body must have a separator");
        assert_eq!(divider, (head.summary + line).max(t.height) + t.card_gap / 2, "the separator starts below the bar edge");
        assert!(divider >= t.height, "the separator must not cut the bar's own row");
        assert_eq!(head.body, divider + divider_height(&t), "body starts after the separator");
        assert_eq!(head.summary + ink_off, t.height / 2 - cap / 2, "the head summary is cap-centred on the bar's line");
        // The last row is the third body row: its ink bottom plus one card_padding is the card's bottom.
        assert_eq!(
            head.rect.bottom(),
            head.body + 2 * line + ink_off + cap + t.card_padding,
            "the card ends one card_padding below the last baseline"
        );
        assert_eq!(s.rect.bottom(), head.rect.bottom());
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

    /// "Everything behind the first is collapsed": six notifications draw the head card plus one peek pill.
    /// A full card each would carry the stack down the screen's scarce axis (see `MAX_TAIL_PERCENT`).
    #[test]
    fn everything_past_the_head_is_one_peek_pill() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let notes: Vec<Notification> = (0..6).map(|i| note(i + 1, &format!("Card {i}"), "body")).collect();
        let s = stack(&notes[..2], notes.len(), 0, 1000, &t, &mut text);
        assert_eq!(s.cards.len(), 2, "the head card and one pill, however long the queue");
        let (head, peek) = (&s.cards[0], &s.cards[1]);
        assert_eq!(head.collapsed, None);
        assert_eq!(peek.collapsed, Some(5), "the pill represents every notification behind the head");
        assert_eq!(peek.rect.y, head.rect.bottom() + t.card_gap, "the pill hangs one card_gap below the head card");
        assert_eq!(peek.rect.h, t.height, "the pill is the bar's own compact shape");
        assert_eq!(peek.rect.w, head.rect.w, "the pill is as wide as the card it hangs under");
        assert_eq!(s.rect.bottom(), peek.rect.bottom(), "the stretch covers the pill");
        assert_eq!(s.rect.y, 0, "the shape starts at the bar's top and its overlap with the bar is invisible");
        // Apple's expanded Live Activity is one fixed size, so a one-word card and a three-line card are equally
        // wide and the stack is a column.
        assert!(s.cards.iter().all(|c| c.rect.w == t.card_w && c.rect.x == s.rect.x));
    }

    /// The pill is its own shape: the gap between it and the head card is background, so the two do not merge.
    #[test]
    fn the_peek_pill_is_its_own_shape_with_a_gap() {
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
        let a = |y: i32| buf[((y * w + mid) * 4) as usize + 3];
        let gap_y = s.cards[0].rect.bottom() + t.card_gap / 2;
        assert_eq!(a(gap_y), 0, "the gap between head and pill must stay transparent, got {} at {gap_y}", a(gap_y));
        assert_ne!(a(s.cards[1].rect.y + 2), 0, "the pill is painted");
        assert_ne!(a(s.cards[0].rect.bottom() - 1), 0, "the head card is painted down to its own edge");
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

    /// macOS collapses the stack behind the head card, so the gap between the two shapes is background.
    #[test]
    fn the_peek_pill_shows_the_next_notification_and_count() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let head = note(1, "Head", "");
        let second = note(2, "Next up", "");
        let w = t.card_w;
        let h = 200;
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let s = laid_out(&[head, second.clone()], 0, w, &t, &mut text);
        let pill = s.cards[1].rect;
        assert_eq!(s.cards[1].collapsed, Some(1), "one notification is hidden behind the head");
        {
            let mut c = Canvas::new(&mut buf, w, h);
            c.set_clip(Some(s.rect));
            render(&mut c, &s.cards[1], 1.0, &second, &t, &mut text);
        }
        let ink_rows: Vec<i32> = (0..h).filter(|y| (0..w).any(|x| buf[((y * w + x) * 4) as usize] > 60)).collect();
        let m = text.cap_metrics(&t.font);
        assert_eq!(s.cards[1].summary, text.optical_top(&t.font, pill.y, t.height), "cap-centred in the pill, like the bar's own line");
        assert_eq!(*ink_rows.first().unwrap(), s.cards[1].summary + m.top.round() as i32);
        assert!(*ink_rows.last().unwrap() < pill.bottom(), "one line of ink, inside the pill");
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
    fn untrusted_action_labels_stay_inside_the_card() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let mut n = note(1, "Subject", "body");
        n.actions = (0..4).map(|i| (format!("action-{i}"), "x".repeat(200))).collect();
        let w = t.card_w;
        let mut buf = vec![0u8; (w * 160 * 4) as usize];
        let s = laid_out(&[n.clone()], 0, w, &t, &mut text);
        let hits = {
            let mut canvas = Canvas::new(&mut buf, w, 160);
            render(&mut canvas, &s.cards[0], 1.0, &n, &t, &mut text)
        };
        let card = s.cards[0].rect;
        assert!(!hits.is_empty(), "at least one short action label must still be offered");
        let action_pad = (t.card_padding / 2).max(2);
        assert!(hits.iter().all(|(r, _)| r.x >= card.x + t.card_padding && r.right() <= card.right() - t.card_padding));
        assert!(hits.iter().all(|(r, _)| r.w >= line_height(&t) + action_pad * 2), "a fitted button must still have room for its label");
    }

    #[test]
    fn headline_fields_cannot_add_visual_rows() {
        assert_eq!(first_line("title\nspoofed second row"), "title");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn action_only_card_starts_below_the_headline() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let mut n = note(1, "Subject", "");
        n.actions = vec![("open".into(), "Open".into())];
        let s = laid_out(&[n], 0, t.card_w, &t, &mut text);
        let card = &s.cards[0];
        let action = card.actions.expect("an action reserves a row");
        assert!(action >= t.height, "the action block starts below the bar edge");
        assert!(card.divider.is_some(), "the action block is separated from the headline");
    }

    #[test]
    fn empty_actions_do_not_reserve_a_row() {
        let t = Theme::defaults(30);
        let mut text = TextEngine::new();
        let mut n = note(1, "Subject", "");
        n.actions = vec![("empty".into(), "\n".into())];
        let s = laid_out(&[n], 0, t.card_w, &t, &mut text);
        assert_eq!(s.cards[0].actions, None);
        assert_eq!(s.rect.h, t.height);
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

    /// The surface has to be tall enough for every frame of the animation: a head card is at most
    /// `MAX_BODY_LINES` body rows plus a summary and an action row, and behind it there is one bar-tall pill.
    #[test]
    fn max_tail_bounds_the_stack() {
        let t = Theme::defaults(30);
        let body = (0..crate::notify::queue::MAX_BODY_LINES).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let mut n = note(1, "Subject", &body);
        n.actions = vec![("open".into(), "Open".into())];
        let visible: Vec<_> = std::iter::repeat_n(n, 6).collect();
        let mut text = TextEngine::new();
        let mut bound_engine = TextEngine::new();
        let s = stack(&visible, visible.len(), 0, 1280, &t, &mut text);
        let tail = s.rect.h - t.height;
        let bound = max_tail(&t, &mut bound_engine, None);
        assert!(tail <= bound, "stack tail {tail} vs bound {bound}");
    }

    /// A tall bar must not push the stack into the middle of a short screen: the derived bound is capped at
    /// `MAX_TAIL_PERCENT` of the output's height, and a shorter output gives a shorter surface.
    #[test]
    fn max_tail_is_capped_by_the_output_height() {
        let t = Theme::defaults(120);
        let mut text = TextEngine::new();
        let derived = max_tail(&t, &mut text, None);
        assert!(derived > 1080 * MAX_TAIL_PERCENT / 100, "the derived bound must exceed the cap, or this proves nothing: {derived}");
        assert_eq!(max_tail(&t, &mut text, Some(1080)), 1080 * MAX_TAIL_PERCENT / 100);
        assert!(max_tail(&t, &mut text, Some(720)) < max_tail(&t, &mut text, Some(2160)), "the cap follows the output");
        // 0 or an unknown output is no usable height: no cap, rather than a zero-height surface.
        assert_eq!(max_tail(&t, &mut text, Some(0)), derived);
    }
}

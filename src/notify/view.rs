//! Independent notification card layout, drawing, motion and hit testing.

use std::time::Instant;

use crate::anim::{Easing, Tween};
use crate::canvas::Canvas;
use crate::geom::{Color, Rect};
use crate::notify::queue::{Notification, Urgency};
use crate::text::TextEngine;
use crate::theme::Theme;
use crate::widget::Action;

const ACTION_FILL_PERCENT: u16 = 22;
const DIVIDER_ALPHA_PERCENT: u16 = 32;
const ENTER_START_SCALE: f32 = 0.94;
const EXIT_END_SCALE: f32 = 0.96;

pub fn urgency_color(u: Urgency, theme: &Theme) -> Color {
    match u {
        Urgency::Low | Urgency::Normal => theme.foreground,
        Urgency::Critical => Color::rgba(0xff, 0x5f, 0x56, 0xff),
    }
}

fn line_height(theme: &Theme) -> i32 { (theme.font.size * 1.35).ceil() as i32 }
fn divider_height(theme: &Theme) -> i32 { (theme.card_gap / 3).max(1) }
fn body_lines(n: &Notification) -> usize { if n.body.is_empty() { 0 } else { n.body.lines().count().min(crate::notify::queue::MAX_BODY_LINES) } }
fn first_line(text: &str) -> &str { text.lines().next().unwrap_or("") }
fn has_actions(n: &Notification) -> bool { n.actions.iter().any(|(_, label)| !first_line(label).trim().is_empty()) }

pub fn top_margin(theme: &Theme, bar_margin: i32) -> i32 { bar_margin + theme.height + theme.card_gap }
pub fn surface_top(theme: &Theme, bar_margin: i32, card_top: i32) -> i32 { top_margin(theme, bar_margin) + card_top }
pub fn side_margin(theme: &Theme) -> i32 { theme.card_gap * 2 }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Visual { pub rect: Rect, pub alpha: f32, pub scale: f32 }

#[derive(Clone, Copy, Debug)]
pub struct Motion {
    enter: Tween,
    move_to: Tween,
    from_top: i32,
    to_top: i32,
    exit: Option<Tween>,
    exit_top: i32,
}

impl Motion {
    pub fn new(now: Instant, top: i32, enter_ms: u64) -> Self {
        Self {
            enter: Tween::new(now, enter_ms),
            move_to: Tween::new(now, 0),
            from_top: top,
            to_top: top,
            exit: None,
            exit_top: top,
        }
    }

    pub fn retarget(&mut self, now: Instant, top: i32, duration_ms: u64) -> bool {
        if self.exit.is_some() || top == self.to_top { return false; }
        self.from_top = self.top(now);
        self.to_top = top;
        self.move_to = Tween::new(now, duration_ms);
        true
    }

    pub fn begin_exit(&mut self, now: Instant, duration_ms: u64) {
        if self.exit.is_some() { return; }
        let top = self.top(now);
        self.enter = Tween::new(now, 0);
        self.move_to = Tween::new(now, 0);
        self.from_top = top;
        self.to_top = top;
        self.exit_top = top;
        self.exit = Some(Tween::new(now, duration_ms));
    }

    pub fn top(&self, now: Instant) -> i32 {
        if self.exit.is_some() { return self.exit_top; }
        let progress = Easing::Spring.apply(self.move_to.progress(now));
        self.from_top + ((self.to_top - self.from_top) as f32 * progress).round() as i32
    }

    pub fn visual(&self, now: Instant, width: i32, height: i32) -> Visual {
        let (scale, alpha) = if let Some(exit) = self.exit {
            let eased = Easing::Smooth.apply(exit.progress(now));
            (1.0 + (EXIT_END_SCALE - 1.0) * eased, 1.0 - eased)
        } else {
            let eased = Easing::Spring.apply(self.enter.progress(now));
            (ENTER_START_SCALE + (1.0 - ENTER_START_SCALE) * eased, eased)
        };
        let w = (width as f32 * scale).round().max(1.0) as i32;
        let h = (height as f32 * scale).round().max(1.0) as i32;
        Visual { rect: Rect::new((width - w) / 2, (height - h) / 2, w.min(width), h.min(height)), alpha: alpha.clamp(0.0, 1.0), scale }
    }

    pub fn is_exiting(&self) -> bool { self.exit.is_some() }
    pub fn is_animating(&self, now: Instant) -> bool { self.exit.is_some_and(|exit| !exit.is_done(now)) || !self.enter.is_done(now) || !self.move_to.is_done(now) }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: u32,
    /// Local pixel geometry of the surface buffer.
    pub rect: Rect,
    /// Offset from the first card's top edge within the notification column.
    pub top: i32,
    pub summary: i32,
    pub body: Option<i32>,
    pub divider: Option<i32>,
    pub actions: Option<i32>,
}

pub fn cards(visible: &[Notification], theme: &Theme) -> Vec<Card> {
    let mut out = Vec::with_capacity(visible.len());
    let mut top = 0;
    for notification in visible {
        let line = line_height(theme);
        let rows = body_lines(notification);
        let actions = has_actions(notification);
        let mut cursor = line;
        let mut divider = None;
        let mut body = None;
        let mut action = None;
        if rows > 0 || actions {
            cursor += theme.card_gap / 2;
            divider = Some(cursor);
            cursor += divider_height(theme);
            if rows > 0 {
                body = Some(cursor);
                cursor += rows as i32 * line;
            }
            if actions {
                if rows > 0 { cursor += theme.card_gap / 2; }
                action = Some(cursor);
                cursor += line;
            }
        }
        let height = theme.card_min_h().max(cursor + theme.card_padding * 2);
        let shift = (height - cursor) / 2;
        out.push(Card {
            id: notification.id,
            rect: Rect::new(0, 0, theme.card_w, height),
            top,
            summary: shift,
            body: body.map(|y| y + shift),
            divider: divider.map(|y| y + shift),
            actions: action.map(|y| y + shift),
        });
        top += height + theme.card_gap;
    }
    out
}

pub fn render_background(canvas: &mut Canvas, visual: Visual, theme: &Theme) {
    let alpha = visual.alpha.clamp(0.0, 1.0);
    let color = Color::rgba(theme.background.r, theme.background.g, theme.background.b, (theme.background.a as f32 * alpha) as u8);
    let radius = ((theme.card_radius as f32 * visual.scale).round() as i32).clamp(0, theme.card_radius);
    canvas.fill_rounded_rect(visual.rect, radius, color);
}

pub fn render(canvas: &mut Canvas, card: &Card, alpha: f32, notification: &Notification, theme: &Theme, text: &mut TextEngine) -> Vec<(Rect, Action)> {
    let alpha = alpha.clamp(0.0, 1.0);
    let fa = |color: Color| Color::rgba(color.r, color.g, color.b, (color.a as f32 * alpha) as u8);
    let line = line_height(theme);
    let x = card.rect.x + theme.card_padding;
    let right = card.rect.right() - theme.card_padding;
    let inner_w = (right - x).max(0) as f32;

    let source = first_line(notification.app_name.trim());
    let source = crate::text::truncate_to_width(source, inner_w / 3.0, |value| text.measure(value, &theme.font).0);
    let source_w = if source.is_empty() { 0 } else { text.measure(&source, &theme.font).0.ceil() as i32 };
    let source_x = right - source_w;
    let summary_w = if source.is_empty() { inner_w } else { (source_x - theme.card_gap - x).max(0) as f32 };
    let summary = crate::text::truncate_to_width(first_line(&notification.summary), summary_w, |value| text.measure(value, &theme.font).0);
    let summary_y = text.optical_top(&theme.font, card.summary, line);
    text.draw(canvas, &summary, x, summary_y, &theme.font, fa(urgency_color(notification.urgency, theme)));
    if !source.is_empty() { text.draw(canvas, &source, source_x, summary_y, &theme.font, fa(theme.secondary)); }

    if let Some(y) = card.divider {
        let divider = Color::rgba(theme.secondary.r, theme.secondary.g, theme.secondary.b, (theme.secondary.a as u16 * DIVIDER_ALPHA_PERCENT / 100) as u8);
        canvas.fill_rect(Rect::new(x, y, inner_w as i32, divider_height(theme)), fa(divider));
    }
    if let Some(mut y) = card.body {
        for body_line in notification.body.lines().take(crate::notify::queue::MAX_BODY_LINES) {
            let body = crate::text::truncate_to_width(body_line, inner_w, |value| text.measure(value, &theme.font).0);
            text.draw(canvas, &body, x, y, &theme.font, fa(theme.secondary));
            y += line;
        }
    }

    let mut hits = Vec::new();
    if let Some(ay) = card.actions {
        let action_pad = (theme.card_padding / 2).max(2);
        let fill = Color::rgba(theme.accent.r, theme.accent.g, theme.accent.b, (theme.accent.a as u16 * ACTION_FILL_PERCENT / 100) as u8);
        let mut bx = x;
        for (key, label) in &notification.actions {
            let label = first_line(label).trim();
            let available = right - bx;
            if label.is_empty() { continue; }
            if available < line + action_pad * 2 { break; }
            let label = crate::text::truncate_to_width(label, (available - action_pad * 2) as f32, |value| text.measure(value, &theme.font).0);
            let label_w = text.measure(&label, &theme.font).0.ceil() as i32;
            let button = Rect::new(bx, ay, (label_w + action_pad * 2).min(available), line);
            canvas.fill_rounded_rect(button, line / 2, fill);
            let text_y = text.optical_top(&theme.font, button.y, button.h);
            text.draw(canvas, &label, bx + (button.w - label_w) / 2, text_y, &theme.font, fa(theme.accent));
            hits.push((button, Action::NotificationAction { id: notification.id, key: key.clone() }));
            bx = button.right() + theme.card_gap;
        }
    }
    hits
}

pub fn hit(hits: &[(Rect, Action)], x: i32, y: i32) -> Option<Action> {
    hits.iter().find(|(rect, _)| rect.contains(x, y)).map(|(_, action)| action.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

    #[test]
    fn every_visible_notification_gets_its_own_card() {
        let theme = Theme::defaults(30);
        let notes = [note(3, "New", ""), note(2, "Middle", "one\ntwo"), note(1, "Old", "")];
        let cards = cards(&notes, &theme);
        assert_eq!(cards.iter().map(|card| card.id).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert!(cards.windows(2).all(|pair| pair[1].top == pair[0].top + pair[0].rect.h + theme.card_gap));
        assert!(cards.iter().all(|card| card.rect.w == theme.card_w && card.top >= 0));
    }

    #[test]
    fn card_height_grows_with_content_but_has_a_derived_minimum() {
        let theme = Theme::defaults(30);
        let short = cards(&[note(1, "Short", "")], &theme);
        let long = cards(&[note(1, "Long", "one\ntwo\nthree\nfour\nfive")], &theme);
        assert_eq!(short[0].rect.h, theme.card_min_h());
        assert!(long[0].rect.h > short[0].rect.h);
    }

    #[test]
    fn detail_rows_follow_the_derived_separator_rhythm() {
        let theme = Theme::defaults(30);
        let with_body = cards(&[note(1, "Subject", "one\ntwo")], &theme);
        let divider = with_body[0].divider.unwrap();
        assert_eq!(divider, with_body[0].summary + line_height(&theme) + theme.card_gap / 2);
        assert_eq!(with_body[0].body, Some(divider + divider_height(&theme)));
    }

    #[test]
    fn action_only_card_starts_below_the_headline() {
        let theme = Theme::defaults(30);
        let mut notification = note(1, "Subject", "");
        notification.actions = vec![("open".into(), "Open".into())];
        let card = &cards(&[notification], &theme)[0];
        assert!(card.actions.unwrap() > card.summary + line_height(&theme));
        assert!(card.divider.is_some());
    }

    #[test]
    fn cards_start_below_the_bar_at_a_derived_edge_inset() {
        let theme = Theme::defaults(30);
        assert_eq!(top_margin(&theme, 4), 4 + theme.height + theme.card_gap);
        assert_eq!(surface_top(&theme, 4, 0), 4 + theme.height + theme.card_gap);
        assert_eq!(surface_top(&theme, 4, 81), 4 + theme.height + theme.card_gap + 81);
        assert_eq!(side_margin(&theme), theme.card_gap * 2);
    }

    #[test]
    fn motion_endpoints_and_cards_are_independent() {
        let now = Instant::now();
        let theme = Theme::defaults(30);
        let size = (theme.card_w, theme.card_min_h());
        let mut entering = Motion::new(now, 0, 200);
        let start = entering.visual(now, size.0, size.1);
        let end = entering.visual(now + Duration::from_millis(200), size.0, size.1);
        assert!(start.alpha < 0.01 && start.scale < 0.95);
        assert!((end.alpha - 1.0).abs() < 0.01 && (end.scale - 1.0).abs() < 0.01);

        let other = Motion::new(now, 100, 200);
        entering.begin_exit(now, 160);
        assert!(entering.is_exiting());
        assert!(!other.is_exiting(), "one card exiting must not put the whole column in the same state");
        let exit = entering.visual(now + Duration::from_millis(160), size.0, size.1);
        assert!(exit.alpha < 0.01 && exit.scale < 0.97);
        assert!(!entering.is_animating(now + Duration::from_millis(160)));
    }

    #[test]
    fn remaining_cards_spring_to_new_tops() {
        let now = Instant::now();
        let mut motion = Motion::new(now, 80, 0);
        assert_eq!(motion.top(now), 80);
        motion.retarget(now, 20, 200);
        assert_eq!(motion.top(now), 80);
        assert!((motion.top(now + Duration::from_millis(200)) - 20).abs() <= 1);
    }

    #[test]
    fn headline_fields_cannot_add_visual_rows() {
        assert_eq!(first_line("title\nspoofed second row"), "title");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn untrusted_action_labels_stay_inside_the_card() {
        let theme = Theme::defaults(30);
        let mut notification = note(1, "Subject", "body");
        notification.actions = (0..4).map(|index| (format!("action-{index}"), "x".repeat(200))).collect();
        let card = cards(&[notification.clone()], &theme).remove(0);
        let mut buffer = vec![0u8; (card.rect.w * card.rect.h * 4) as usize];
        let hits = {
            let mut canvas = Canvas::new(&mut buffer, card.rect.w, card.rect.h);
            canvas.set_clip(Some(card.rect));
            render(&mut canvas, &card, 1.0, &notification, &theme, &mut TextEngine::new())
        };
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|(rect, _)| rect.x >= card.rect.x + theme.card_padding && rect.right() <= card.rect.right() - theme.card_padding));
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
}

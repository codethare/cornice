//! Minimal widget primitives shared by the bar and notifications.

use crate::geom::Color;

#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    NotificationClose(u32),
    NotificationAction { id: u32, key: String },
}

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Span {
    pub text: String,
    pub color: Option<Color>,
    pub bg: Option<Color>,
    pub action: Option<Action>,
    /// Window count for the application monogram chip; `None` for ordinary text spans.
    pub badge: Option<u32>,
}

/// One mapped toplevel as reported by the compositor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toplevel {
    pub app_id: String,
}

impl Span {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }
    /// A square application chip: one monogram, plus the window count when it exceeds one.
    pub fn application(monogram: impl Into<String>, count: u32) -> Self {
        Self { text: monogram.into(), badge: Some(count), ..Default::default() }
    }
    // Design §5 says the Span primitive covers "multi-colour text runs, notification buttons and module text" at once.
    // The application chips reuse it through `badge`; notification is a separate surface, so the constructors below stay minimal.
    #[allow(dead_code)]
    pub fn with_color(mut self, c: Color) -> Self { self.color = Some(c); self }
    #[allow(dead_code)]
    pub fn with_action(mut self, a: Action) -> Self { self.action = Some(a); self }
}

#[derive(Clone, Debug)]
pub enum Event {
    /// Timer wakeup (clock tick, animation step)
    Wake,
    /// One line of output from the `id`-th exec module child
    Line { id: usize, text: String },
    /// The compositor's current toplevel list, already reduced to app identity.
    Toplevels(Vec<Toplevel>),
}

pub trait Module {
    /// Returns whether a redraw is needed.
    fn update(&mut self, ev: &Event) -> bool;
    fn spans(&self) -> Vec<Span>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::Sections;
    use crate::text::TextEngine;
    use crate::theme::Theme;

    #[test]
    fn span_helpers() {
        let s = Span::text("hi");
        assert_eq!(s.text, "hi");
        assert!(s.color.is_none() && s.action.is_none() && s.badge.is_none());
        let s = Span::text("hi").with_color(crate::geom::Color::rgba(1, 2, 3, 4));
        assert_eq!(s.color.unwrap().r, 1);
        let s = Span::application("F", 2);
        assert_eq!((s.text.as_str(), s.badge), ("F", Some(2)));
    }

    #[test]
    fn sections_from_config_builds_modules() {
        let cfg = crate::config::parse(
            "[bar.left]\nmodules = [ { kind = \"clock\", format = \"%H\" } ]\n[bar.right]\nmodules = [ { kind = \"exec\", command = \"echo 1\", format = \"{out}\" } ]\n",
        )
        .unwrap();
        let (mut s, _rx, _tx) = Sections::from_config(&cfg);
        assert_eq!(s.left.len(), 1);
        assert_eq!(s.right.len(), 1);
        // There must be content before the first tick: the clock's initial value is computed right away instead of waiting for the first Wake
        assert!(!s.left[0].spans().is_empty());
        let mut text = TextEngine::new();
        let theme = Theme::defaults(30);
        let w = s.widths(&mut text, &theme);
        assert!(w.left[0] > 0, "the clock width should be greater than 0, got {}", w.left[0]);
    }
}

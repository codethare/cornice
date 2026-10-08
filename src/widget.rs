//! Minimal widget primitives shared by the bar and notifications.

use crate::geom::Color;

#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    /// The card body, i.e. the area not covered by an action button: a left click invokes the `default`
    /// action when the notification has one, and any click on it closes the card.
    NotificationBody(u32),
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
    /// True for the chip whose `app_id` the compositor reports as focused.
    pub focused: bool,
}

/// What a click or a wheel notch on a bar module does. The commands are run with `sh -c`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModuleActions {
    pub on_click: Option<String>,
    pub on_scroll_up: Option<String>,
    pub on_scroll_down: Option<String>,
}

impl ModuleActions {
    /// A module with no command stays out of the bar's input region, so clicks fall through to whatever is below.
    pub fn any(&self) -> bool {
        self.on_click.is_some() || self.on_scroll_up.is_some() || self.on_scroll_down.is_some()
    }
}

/// Cross-thread control messages: the D-Bus thread posts these when the CLI calls `cornice bar ...`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlRequest {
    ToggleBar,
    SetBarVisible(bool),
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
    pub fn application(monogram: impl Into<String>, count: u32, focused: bool) -> Self {
        Self { text: monogram.into(), badge: Some(count), focused, ..Default::default() }
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
    /// `app_id` of the focused toplevel, or `None` when nothing is focused (also the value when the
    /// compositor does not implement the window list at all).
    FocusedApp(Option<String>),
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
    fn module_action_lists_line_up_with_their_modules() {
        let cfg = crate::config::parse(
            "[bar.left]\nmodules = [ { kind = \"clock\" }, { kind = \"applications\", on_click = \"x\" } ]\n",
        )
        .unwrap();
        let (s, _rx, _tx) = Sections::from_config(&cfg);
        assert_eq!(s.left_actions.len(), s.left.len());
        assert!(!s.left_actions[0].any());
        assert_eq!(s.left_actions[1].on_click.as_deref(), Some("x"));
        assert!(!s.left_actions[0].any());
        assert!(s.center_actions.is_empty() && s.right_actions.is_empty());
    }

    #[test]
    fn span_helpers() {
        let s = Span::text("hi");
        assert_eq!(s.text, "hi");
        assert!(s.color.is_none() && s.action.is_none() && s.badge.is_none());
        let s = Span::text("hi").with_color(crate::geom::Color::rgba(1, 2, 3, 4));
        assert_eq!(s.color.unwrap().r, 1);
        let s = Span::application("F", 2, true);
        assert_eq!((s.text.as_str(), s.badge, s.focused), ("F", Some(2), true));
    }

    #[test]
    fn module_actions_report_whether_they_catch_input() {
        assert!(!ModuleActions::default().any());
        assert!(ModuleActions { on_click: Some("echo hi".into()), ..Default::default() }.any());
        assert!(ModuleActions { on_scroll_up: Some("true".into()), ..Default::default() }.any());
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
        // The idle timer is only armed when something wants a tick.
        assert!(s.has_clock, "the clock module asks for the per-second wake");
        assert!(!Sections::from_config(&crate::config::parse("[bar.left]\nmodules = []\n").unwrap()).0.has_clock);
        // There must be content before the first tick: the clock's initial value is computed right away instead of waiting for the first Wake
        assert!(!s.left[0].spans().is_empty());
        let mut text = TextEngine::new();
        let theme = Theme::defaults(30);
        let w = s.widths(&mut text, &theme);
        assert!(w.left[0] > 0, "the clock width should be greater than 0, got {}", w.left[0]);
    }
}

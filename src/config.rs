//! TOML config: parsing, default derivation and validation errors that carry line numbers.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Spanned;

use crate::geom::Color;
use crate::theme::{parse_font, Theme};

#[derive(Clone)]
pub struct Config { pub bar: Bar, pub theme: Theme, pub notification: Option<Notification> }

/// Hand-written `Debug`: the embedded `Theme` holds a `TextStyle`, which does not derive `Debug`
/// (it belongs to Task 3's file range), the derivation comes along with it. The planned test wants `unwrap_err()`,
/// hence `Config` must implement `Debug`.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bar", &self.bar)
            .field("notification", &self.notification)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Bar {
    pub height: i32,
    pub margin: i32,
    pub left: Vec<ModuleSpec>,
    pub center: Vec<ModuleSpec>,
    pub right: Vec<ModuleSpec>,
}

#[derive(Clone, Deserialize, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ModuleSpec {
    Clock { #[serde(default = "default_clock_format")] format: String },
    Exec { command: String, #[serde(default)] format: String },
}

fn default_clock_format() -> String { "%H:%M".to_string() }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationPosition { Left, Center, Right }

#[derive(Clone, Debug)]
pub struct Notification { pub position: NotificationPosition, pub max_visible: usize, pub enter_ms: u64, pub exit_ms: u64 }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawConfig { bar: RawBar, theme: RawTheme, notification: Option<RawNotification> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawBar {
    /// `Spanned` exists so an out-of-range error points at the real line and column.
    height: Option<Spanned<i32>>,
    margin: Option<i32>, padding: Option<i32>, spacing: Option<i32>,
    left: RawSection, center: RawSection, right: RawSection,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawSection { modules: Vec<Spanned<ModuleSpec>> }

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct RawTheme {
    background: Option<Spanned<String>>,
    foreground: Option<Spanned<String>>,
    accent: Option<Spanned<String>>,
    font: Option<Spanned<String>>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawNotification { position: Option<Spanned<String>>, max_visible: usize, enter_ms: u64, exit_ms: u64 }

impl Default for RawNotification {
    fn default() -> Self { Self { position: None, max_visible: 4, enter_ms: 220, exit_ms: 160 } }
}

/// Byte offset → (line, col), 1-based.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let upto = &text[..offset.min(text.len())];
    let line = upto.matches('\n').count() + 1;
    let col = upto.rfind('\n').map_or(upto.len(), |i| upto.len() - i - 1) + 1;
    (line, col)
}

fn color_at(text: &str, key: &str, v: &Option<Spanned<String>>, fallback: Color) -> Result<Color, String> {
    let Some(v) = v else { return Ok(fallback) };
    Color::from_hex(v.get_ref()).map_err(|e| {
        let (l, c) = line_col(text, v.span().start);
        format!("{l}:{c}: {key}: {e}")
    })
}

const DEFAULT_HEIGHT: i32 = 30;
/// Valid bar height range in pixels. The upper bound is defensive: the value is also `layer.set_size`,
/// input to each output's `SlotPool::new(width * height * 4)` and to the derived notification ratios,
/// a mistyped number must not turn into a multi-terabyte allocation request.
const HEIGHT_RANGE: std::ops::RangeInclusive<i32> = 1..=HEIGHT_MAX;
const HEIGHT_MAX: i32 = 256;

fn height_at(text: &str, v: &Option<Spanned<i32>>) -> Result<i32, String> {
    let Some(v) = v else { return Ok(DEFAULT_HEIGHT) };
    let h = *v.get_ref();
    if !HEIGHT_RANGE.contains(&h) {
        let (l, c) = line_col(text, v.span().start);
        return Err(format!("{l}:{c}: bar.height: bar height must be within {HEIGHT_RANGE:?} pixels, currently {h}"));
    }
    Ok(h)
}

fn modules_of(spanned: &[Spanned<ModuleSpec>]) -> Vec<ModuleSpec> { spanned.iter().map(|m| m.get_ref().clone()).collect() }

fn notification_position_at(text: &str, value: &Option<Spanned<String>>) -> Result<NotificationPosition, String> {
    let Some(value) = value else { return Ok(NotificationPosition::Right) };
    match value.get_ref().as_str() {
        "left" => Ok(NotificationPosition::Left),
        "center" => Ok(NotificationPosition::Center),
        "right" => Ok(NotificationPosition::Right),
        _ => {
            let (line, column) = line_col(text, value.span().start);
            Err(format!("{line}:{column}: notification.position: expected left, center, or right"))
        }
    }
}

pub fn parse(text: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| format!("{e}"))?;

    let height = height_at(text, &raw.bar.height)?;
    let mut theme = Theme::defaults(height);
    theme.padding = raw.bar.padding.unwrap_or(theme.padding);
    theme.spacing = raw.bar.spacing.unwrap_or(theme.spacing);
    theme.background = color_at(text, "theme.background", &raw.theme.background, theme.background)?;
    theme.foreground = color_at(text, "theme.foreground", &raw.theme.foreground, theme.foreground)?;
    theme.accent = color_at(text, "theme.accent", &raw.theme.accent, theme.accent)?;
    if let Some(f) = &raw.theme.font {
        theme.font = parse_font(f.get_ref(), theme.font.size);
    }
    let notification = match raw.notification {
        Some(notification) => Some(Notification {
            position: notification_position_at(text, &notification.position)?,
            max_visible: notification.max_visible.max(1),
            enter_ms: notification.enter_ms,
            exit_ms: notification.exit_ms,
        }),
        None => None,
    };

    Ok(Config {
        bar: Bar {
            height,
            margin: raw.bar.margin.unwrap_or(0),
            left: modules_of(&raw.bar.left.modules),
            center: modules_of(&raw.bar.center.modules),
            right: modules_of(&raw.bar.right.modules),
        },
        theme,
        notification,
    })
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("cornice/config.toml")
}

/// A missing file means an all-default config (the first run should not error).
pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("cornice: {} does not exist, using the default config", path.display());
            parse("")
        }
        Err(e) => Err(format!("failed to read {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"
[bar]
height = 30
margin = 0
padding = 8
spacing = 6

[theme]
background = "#1a1a1aee"
foreground = "#dcdcdc"
font = "Inter 11"

[bar.left]
modules = [ { kind = "clock", format = "%H:%M" } ]

[bar.center]
modules = []

[bar.right]
modules = [ { kind = "exec", command = "echo hi", format = "{out}!" } ]

[notification]
position = "left"
max_visible = 3
"##;

    #[test]
    fn parses_sample_and_derives_defaults() {
        let c = parse(SAMPLE).unwrap();
        assert_eq!(c.bar.height, 30);
        assert_eq!(c.theme.padding, 8);
        assert_eq!(c.theme.spacing, 6);
        assert_eq!(c.bar.left.len(), 1);
        assert!(c.bar.center.is_empty());
        assert!(matches!(c.bar.right[0], ModuleSpec::Exec { .. }));
        assert_eq!(c.theme.background.a, 0xee);
        assert_eq!(c.theme.font.size, 11.0);
        assert_eq!(c.theme.font.family, "Inter");
        let notification = c.notification.as_ref().expect("[notification] enables cards");
        assert_eq!(notification.position, NotificationPosition::Left);
        assert_eq!(notification.max_visible, 3);
        assert_eq!(notification.enter_ms, 220, "the default when absent");
    }

    /// The shipped template is a document, not decoration: it must stay parseable.
    #[test]
    fn shipped_template_parses() {
        let c = parse(include_str!("../config.toml")).unwrap();
        assert_eq!(c.bar.height, 30);
        assert_eq!(c.bar.margin, 0);
        assert_eq!(c.bar.right.len(), 2);
        let notification = c.notification.as_ref().expect("the template enables notifications");
        assert_eq!(notification.position, NotificationPosition::Right);
        assert_eq!(notification.max_visible, 4);
    }

    #[test]
    fn notification_section_is_optional_and_defaults_to_right() {
        assert!(parse("").unwrap().notification.is_none());
        let c = parse("[notification]\n").unwrap();
        assert_eq!(c.notification.unwrap().position, NotificationPosition::Right);
    }

    #[test]
    fn notification_position_accepts_only_three_anchors() {
        for (position, expected) in [("left", NotificationPosition::Left), ("center", NotificationPosition::Center), ("right", NotificationPosition::Right)] {
            let c = parse(&format!("[notification]\nposition = {position:?}\n")).unwrap();
            assert_eq!(c.notification.unwrap().position, expected);
        }
        let error = parse("[notification]\nposition = \"middle\"\n").unwrap_err();
        assert!(error.starts_with("2:"), "the invalid position points at its line:column: {error}");
        assert!(error.contains("notification.position"), "{error}");
    }

    #[test]
    fn notification_is_not_a_bar_module() {
        let error = parse("[bar.right]\nmodules = [ { kind = \"notification\" } ]\n").unwrap_err();
        assert!(error.contains("notification") || error.contains("unknown variant"), "{error}");
    }

    #[test]
    fn bar_height_derives_notification_proportions() {
        let c = parse("[bar]\nheight = 24\n").unwrap();
        assert_eq!(c.theme.card_gap, 4);
        assert!(c.bar.left.is_empty() && c.bar.right.is_empty());
        assert_eq!(c.bar.height, 24);
    }

    #[test]
    fn bar_radius_is_rejected_instead_of_silently_ignored() {
        let error = parse("[theme]\nradius = 0\n").unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("radius"), "{error}");
    }

    #[test]
    fn bad_color_reports_line_number() {
        let e = parse("[theme]\nbackground = \"zzzzzz\"\n").unwrap_err();
        assert!(e.starts_with("2:"), "the error message should start with `line:col:`: {e}");
        assert!(e.contains("theme.background"), "{e}");
    }

    #[test]
    fn syntax_error_reports_line_number() {
        let e = parse("[bar]\nheight = \n").unwrap_err();
        assert!(e.contains("line"), "toml supplies the line number: {e}");
    }

    #[test]
    fn unknown_module_kind_is_rejected() {
        let e = parse("[bar.left]\nmodules = [ { kind = \"river.tags\" } ]\n").unwrap_err();
        assert!(e.contains("river.tags") || e.contains("unknown variant"), "{e}");
    }

    #[test]
    fn font_string_without_size_uses_default() {
        let c = parse("[theme]\nfont = \"monospace\"\n").unwrap();
        assert_eq!(c.theme.font.family, "monospace");
        assert_eq!(c.theme.font.size, 11.0);
    }

    /// Trust boundary: height flows into `layer.set_size` and the shm pool size, so it must be rejected at the parsing layer.
    #[test]
    fn out_of_range_height_is_rejected_with_line_and_column() {
        for (input, shown) in [
            ("[bar]\nheight = -1\n", "-1"),
            ("[bar]\nheight = 0\n", "0"),
            ("[bar]\nheight = 4096\n", "4096"),
        ] {
            let e = parse(input).unwrap_err();
            assert!(e.starts_with("2:"), "the error message should start with `line:col:`: {e}");
            assert!(e.contains("bar.height"), "{e}");
            assert!(e.contains(shown), "{e}");
        }
    }

    #[test]
    fn height_range_edges_are_accepted() {
        for h in [1, 24, 30, 40, 256] {
            let input = format!("[bar]\nheight = {h}\n");
            assert_eq!(parse(&input).unwrap().bar.height, h);
        }
        // still defaults to 30 when the key is absent
        assert_eq!(parse("[bar]\n").unwrap().bar.height, 30);
    }
}

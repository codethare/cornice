//! Built-in bar modules: `clock`, `exec` and `applications`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Local;

use crate::widget::{Event, Module, Span, Toplevel};

pub struct Clock { pub format: String, pub text: String }

impl Clock {
    pub fn new(format: String) -> Self {
        let mut c = Self { format, text: String::new() };
        c.text = Local::now().format(&c.format).to_string();
        c
    }
}

impl Module for Clock {
    fn update(&mut self, ev: &Event) -> bool {
        let Event::Wake = ev else { return false };
        let now = Local::now().format(&self.format).to_string();
        if now == self.text { return false; }
        self.text = now;
        true
    }
    fn spans(&self) -> Vec<Span> { vec![Span::text(&self.text)] }
}

pub struct Exec {
    pub id: usize,
    pub format: String,
    pub latest: String,
}

/// `{out}` placeholder substitution; with no placeholder the format string is emitted as-is.
pub fn apply_format(fmt: &str, value: &str) -> String {
    if fmt.is_empty() { return value.to_string(); }
    fmt.replace("{out}", value)
}

impl Exec {
    /// Spawn a long-running child; its stdout goes back to the main loop line by line through `tx`.
    /// With `interval` the child is one-shot and re-run every `interval` ms instead of staying resident.
    pub fn spawn(id: usize, command: String, format: String, interval: Option<u64>, tx: calloop::channel::Sender<Event>) -> Self {
        // command is moved straight into the resident thread; no copy is kept in the struct — nothing would read it.
        std::thread::spawn(move || loop {
            let started = Instant::now();
            let child = Command::new("sh").arg("-c").arg(&command).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
            match child {
                Ok(mut child) => {
                    if let Some(out) = child.stdout.take() {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            if tx.send(Event::Line { id, text: line }).is_err() { return; }
                        }
                    }
                    let _ = child.wait();
                }
                Err(e) => eprintln!("cornice: failed to start exec module ({command}): {e}"),
            }
            match interval {
                // Counted from the start of the run, so a slow command does not add its own duration to the period.
                Some(ms) => std::thread::sleep(Duration::from_millis(ms).saturating_sub(started.elapsed())),
                // ponytail: a resident child that exits is retried at a fixed 1s, no backoff; add backoff if it ever fails madly.
                None => std::thread::sleep(Duration::from_secs(1)),
            }
        });
        Self { id, format, latest: String::new() }
    }

    pub fn rendered(&self) -> String { apply_format(&self.format, &self.latest) }
}

impl Module for Exec {
    fn update(&mut self, ev: &Event) -> bool {
        let Event::Line { id, text } = ev else { return false };
        if *id != self.id || *text == self.latest { return false; }
        self.latest = text.clone();
        true
    }
    fn spans(&self) -> Vec<Span> { vec![Span::text(self.rendered())] }
}

/// Open applications grouped by `app_id`; one monogram chip each, with the window count when it exceeds one.
pub struct Applications {
    groups: Vec<(String, u32)>,
    /// `app_id` reported as focused by the compositor's window list, when it reports one.
    focused: Option<String>,
}

impl Applications {
    pub fn new() -> Self { Self { groups: Vec::new(), focused: None } }
}

impl Default for Applications { fn default() -> Self { Self::new() } }

/// Sorted by `app_id`, so the bar does not reshuffle as toplevels arrive in arbitrary order.
fn group(toplevels: &[Toplevel]) -> Vec<(String, u32)> {
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for toplevel in toplevels {
        if toplevel.app_id.is_empty() { continue; }
        *counts.entry(&toplevel.app_id).or_default() += 1;
    }
    counts.into_iter().map(|(app_id, count)| (app_id.to_string(), count)).collect()
}

/// The last reverse-DNS segment's first letter; `?` when the compositor gave no usable identity.
fn monogram(app_id: &str) -> String {
    let name = app_id.rsplit('.').next().unwrap_or(app_id);
    name.chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".to_string())
}

impl Module for Applications {
    fn update(&mut self, ev: &Event) -> bool {
        match ev {
            Event::Toplevels(toplevels) => {
                let groups = group(toplevels);
                if groups == self.groups { return false; }
                self.groups = groups;
                true
            }
            Event::FocusedApp(focused) => {
                if *focused == self.focused { return false; }
                self.focused = focused.clone();
                true
            }
            _ => false,
        }
    }

    fn spans(&self) -> Vec<Span> {
        self.groups
            .iter()
            .map(|(app_id, count)| Span::application(monogram(app_id), *count, self.focused.as_deref() == Some(app_id.as_str())))
            .collect()
    }
}

/// Run a module's click or wheel command. Nothing waits for it here, and the wait happens on a throwaway
/// thread so a click cannot leave a zombie behind; stdio is discarded so the child cannot hold the bar's streams.
pub fn spawn_detached(command: &str) {
    let spawned = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("cornice: failed to run module command ({command}): {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_substitution() {
        assert_eq!(apply_format("{out}%", "42"), "42%");
        assert_eq!(apply_format("L{out} / {out}", "1"), "L1 / 1");
        assert_eq!(apply_format("", "42"), "42");
        assert_eq!(apply_format("static", "42"), "static");
    }

    #[test]
    fn exec_only_reacts_to_its_own_id() {
        let (tx, _rx) = calloop::channel::channel::<Event>();
        let mut e = Exec::spawn(3, "true".into(), "[{out}]".into(), None, tx);
        assert!(!e.update(&Event::Line { id: 2, text: "x".into() }));
        assert!(e.update(&Event::Line { id: 3, text: "7".into() }));
        assert_eq!(e.spans()[0].text, "[7]");
        assert!(!e.update(&Event::Line { id: 3, text: "7".into() }), "an unchanged value is not dirty");
    }

    #[test]
    fn applications_group_by_app_id_and_mark_counts() {
        let mut applications = Applications::new();
        let toplevels = vec![
            Toplevel { app_id: "firefox".into() },
            Toplevel { app_id: "firefox".into() },
            Toplevel { app_id: "org.gnome.Nautilus".into() },
        ];
        assert!(applications.update(&Event::Toplevels(toplevels.clone())), "a changed list is dirty");
        let mut reordered = toplevels.clone();
        reordered.reverse();
        assert!(!applications.update(&Event::Toplevels(reordered)), "order alone is not dirty");
        assert!(!applications.update(&Event::Toplevels(toplevels)), "the same list is not dirty");
        let spans = applications.spans();
        assert_eq!(spans.iter().map(|span| span.text.as_str()).collect::<Vec<_>>(), vec!["F", "N"]);
        assert_eq!(spans.iter().map(|span| span.badge).collect::<Vec<_>>(), vec![Some(2), Some(1)]);
        assert!(spans.iter().all(|span| !span.focused), "nothing is focused until the compositor says so");
    }

    #[test]
    fn only_the_focused_app_id_is_marked() {
        let mut applications = Applications::new();
        applications.update(&Event::Toplevels(vec![
            Toplevel { app_id: "firefox".into() },
            Toplevel { app_id: "kitty".into() },
        ]));
        assert!(applications.update(&Event::FocusedApp(Some("kitty".into()))));
        assert!(
            applications.spans().iter().map(|span| (span.text.as_str(), span.focused)).collect::<Vec<_>>()
                == vec![("F", false), ("K", true)]
        );
        assert!(!applications.update(&Event::FocusedApp(Some("kitty".into()))), "the same focus is not dirty");
        assert!(applications.update(&Event::FocusedApp(None)));
        assert!(applications.spans().iter().all(|span| !span.focused), "losing focus clears the highlight");
        assert!(applications.update(&Event::FocusedApp(Some("gone".into()))), "an app with no window is still a change");
    }

    #[test]
    fn monogram_prefers_the_last_app_id_segment() {
        assert_eq!(monogram("org.mozilla.firefox"), "F");
        assert_eq!(monogram("kitty"), "K");
        assert_eq!(monogram(""), "?");
    }

    #[test]
    fn clock_ticks_on_wake() {
        let mut c = Clock::new("%H:%M:%S".into());
        let first = c.spans()[0].text.clone();
        assert!(!first.is_empty());
        let _ = c.update(&Event::Wake);
        assert!(!c.update(&Event::Line { id: 0, text: "x".into() }));
    }
}

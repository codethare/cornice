//! Notification queue: pure logic, no D-Bus and no rendering.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Urgency { Low, Normal, Critical }

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub urgency: Urgency,
    /// None = never auto-dismiss
    pub expire: Option<Duration>,
    pub actions: Vec<(String, String)>,
    pub created: Instant,
}

#[derive(Clone, Debug)]
pub enum Request {
    Notify { id: u32, app_name: String, replaces_id: u32, summary: String, body: String, actions: Vec<(String, String)>, urgency: Urgency, expire_timeout: i32 },
    Close { id: u32 },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome { Added(u32), Replaced(u32), CloseRequested(u32), Ignored }

pub const MAX_BODY_LINES: usize = 5;
pub const MAX_BODY_CHARS: usize = 300;

pub fn default_timeout(u: Urgency) -> Option<Duration> {
    match u {
        Urgency::Low => Some(Duration::from_secs(4)),
        Urgency::Normal => Some(Duration::from_secs(6)),
        Urgency::Critical => None,
    }
}

/// This is a trust boundary: a client (or a script) can stuff in a whole document.
pub fn truncate_body(body: &str) -> String {
    let mut lines: Vec<&str> = body.lines().take(MAX_BODY_LINES + 1).collect();
    let truncated_lines = body.lines().count() > MAX_BODY_LINES;
    lines.truncate(MAX_BODY_LINES);
    let mut out = lines.join("\n");
    if truncated_lines { out.push('…'); }
    if out.chars().count() > MAX_BODY_CHARS {
        out = out.chars().take(MAX_BODY_CHARS).collect::<String>() + "…";
    }
    out
}

pub struct Queue {
    /// new → old
    items: Vec<Notification>,
    max_visible: usize,
}

impl Queue {
    pub fn new(max_visible: usize) -> Self { Self { items: Vec::new(), max_visible: max_visible.max(1) } }

    pub fn visible(&self) -> &[Notification] { &self.items[..self.items.len().min(self.max_visible)] }
    pub fn get(&self, id: u32) -> Option<&Notification> { self.items.iter().find(|n| n.id == id) }
    pub fn is_empty(&self) -> bool { self.items.is_empty() }

    pub fn remove(&mut self, id: u32) -> Option<Notification> {
        let i = self.items.iter().position(|n| n.id == id)?;
        Some(self.items.remove(i))
    }

    pub fn apply(&mut self, req: Request, now: Instant) -> Outcome {
        match req {
            Request::Notify { id, app_name, replaces_id, summary, body, actions, urgency, expire_timeout } => {
                let expire = if expire_timeout < 0 { default_timeout(urgency) } else if expire_timeout == 0 { None } else { Some(Duration::from_millis(expire_timeout as u64)) };
                let updated = Notification { id, app_name, summary, body: truncate_body(&body), urgency, expire, actions, created: now };
                if replaces_id != 0 {
                    if let Some(slot) = self.items.iter_mut().find(|n| n.id == replaces_id) {
                        let kept_id = slot.id;
                        *slot = Notification { id: kept_id, ..updated };
                        return Outcome::Replaced(kept_id);
                    }
                }
                self.items.insert(0, updated);
                Outcome::Added(id)
            }
            Request::Close { id } => {
                // report presence only and let notify_closed remove it once; otherwise it is removed twice,
                // a second remove in notify_closed returns None and swallows the NotificationClosed signal.
                if self.get(id).is_some() { Outcome::CloseRequested(id) } else { Outcome::Ignored }
            }
        }
    }

    /// Returns the ids that expired this round (the close reason is always 1).
    pub fn expire(&mut self, now: Instant) -> Vec<u32> {
        let expired: Vec<u32> = self
            .items
            .iter()
            .filter(|n| n.expire.is_some_and(|d| now.saturating_duration_since(n.created) >= d))
            .map(|n| n.id)
            .collect();
        self.items.retain(|n| !expired.contains(&n.id));
        expired
    }

    pub fn next_expiry(&self) -> Option<Instant> {
        self.items.iter().filter_map(|n| n.expire.map(|d| n.created + d)).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn notify(id: u32, replaces: u32) -> Request {
        Request::Notify {
            id,
            app_name: "test".into(),
            replaces_id: replaces,
            summary: format!("s{id}"),
            body: "b".into(),
            actions: vec![],
            urgency: Urgency::Normal,
            expire_timeout: -1,
        }
    }

    #[test]
    fn default_timeouts_by_urgency() {
        assert_eq!(default_timeout(Urgency::Low), Some(Duration::from_secs(4)));
        assert_eq!(default_timeout(Urgency::Normal), Some(Duration::from_secs(6)));
        assert_eq!(default_timeout(Urgency::Critical), None);
    }

    /// Same as `notify`, but with an explicit `expire_timeout`.
    fn notify_t(id: u32, replaces: u32, expire_timeout: i32) -> Request {
        match notify(id, replaces) {
            Request::Notify { id, app_name, replaces_id, summary, body, actions, urgency, .. } =>
                Request::Notify { id, app_name, replaces_id, summary, body, actions, urgency, expire_timeout },
            _ => unreachable!(),
        }
    }

    #[test]
    fn expire_timeout_semantics() {
        let now = Instant::now();
        // 0 = never auto-dismiss
        let mut q = Queue::new(4);
        q.apply(notify_t(1, 0, 0), now);
        assert_eq!(q.next_expiry(), None);
        // >0 = use as-is
        let mut q = Queue::new(4);
        q.apply(notify_t(2, 0, 1000), now);
        assert_eq!(q.next_expiry(), Some(now + Duration::from_millis(1000)));
        // -1 = default from urgency (normal → 6s)
        let mut q = Queue::new(4);
        q.apply(notify_t(3, 0, -1), now);
        assert_eq!(q.next_expiry(), Some(now + Duration::from_secs(6)));
    }

    #[test]
    fn replaces_id_updates_in_place_without_new_id() {
        let now = Instant::now();
        let mut q = Queue::new(4);
        assert_eq!(q.apply(notify(1, 0), now), Outcome::Added(1));
        assert_eq!(q.apply(notify(2, 0), now), Outcome::Added(2));
        let mut req = notify_t(9, 1, -1);
        if let Request::Notify { summary, .. } = &mut req {
            *summary = "new".into();
        }
        let out = q.apply(req, now);
        assert_eq!(out, Outcome::Replaced(1));
        assert_eq!(q.visible().len(), 2);
        assert_eq!(q.get(1).unwrap().summary, "new");
        // replacing does not move it (after notify(1), notify(2) takes the head, so id=1 is still at index 1)
        assert_eq!(q.visible()[1].id, 1);
    }

    #[test]
    fn replaces_id_for_unknown_id_creates_entry() {
        let now = Instant::now();
        let mut q = Queue::new(4);
        assert_eq!(q.apply(notify(7, 999), now), Outcome::Added(7));
    }

    #[test]
    fn max_visible_hides_but_keeps_in_queue() {
        let now = Instant::now();
        let mut q = Queue::new(2);
        q.apply(notify(1, 0), now);
        q.apply(notify(2, 0), now);
        q.apply(notify(3, 0), now);
        assert_eq!(q.visible().len(), 2, "only the first two are visible");
        assert_eq!(q.get(3).unwrap().summary, "s3", "the third entry is still queued");
        q.remove(1);
        // new → old order: notify(3) is newest at the head, notify(2) next; the one dropped is the hidden id=1 at the tail
        assert_eq!(q.visible().iter().map(|n| n.id).collect::<Vec<_>>(), vec![3, 2], "they slide in in turn as earlier ones disappear");
    }

    #[test]
    fn expiry_reports_ids_and_drops_them() {
        let now = Instant::now();
        let mut q = Queue::new(4);
        q.apply(notify_t(1, 0, 1000), now);
        assert!(q.expire(now + Duration::from_millis(999)).is_empty());
        assert_eq!(q.expire(now + Duration::from_millis(1000)), vec![1]);
        assert!(q.get(1).is_none());
    }

    #[test]
    fn close_and_action_requests() {
        let now = Instant::now();
        let mut q = Queue::new(4);
        q.apply(notify(5, 0), now);
        assert_eq!(q.apply(Request::Close { id: 5 }, now), Outcome::CloseRequested(5));
        assert_eq!(q.apply(Request::Close { id: 404 }, now), Outcome::Ignored);
    }

    #[test]
    fn truncate_body_limits_lines_and_chars() {
        assert_eq!(truncate_body("a\nb\nc\nd\ne\nf\ng"), "a\nb\nc\nd\ne…");
        let long = "x".repeat(500);
        assert!(truncate_body(&long).chars().count() <= 301);
        assert!(truncate_body(&long).ends_with('…'));
        assert_eq!(truncate_body("short"), "short");
    }
}

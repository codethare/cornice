//! D-Bus side: a zbus blocking connection plus the interface object. Interface methods never block; they only post messages to the main loop.

use zbus::blocking::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::interface;

use super::queue::{Request, Urgency};
use crate::widget::ControlRequest;

pub const PATH: &str = "/org/freedesktop/Notifications";
pub const IFACE: &str = "org.freedesktop.Notifications";
pub const BUS_NAME: &str = "org.freedesktop.Notifications";
/// cornice's own control surface on the same object path, kept off the spec interface so no client sees a non-standard method there.
pub const CONTROL_IFACE: &str = "org.cornice.Control";
/// Bar control answers on its own well-known name: the spec name belongs to whichever notification daemon
/// won it, and `cornice bar ...` must keep working when that is not cornice.
pub const CONTROL_BUS_NAME: &str = "org.cornice.Control";

pub struct NotifyDaemon {
    tx: calloop::channel::Sender<Request>,
    next_id: std::sync::atomic::AtomicU32,
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotifyDaemon {
    fn get_capabilities(&self) -> Vec<String> {
        // Declare honestly: whatever is unsupported is not advertised, and clients degrade to plain text on their own
        vec!["body".into(), "actions".into(), "persistence".into()]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        // The id is the client-visible handle, so it must be the one the queue ends up using. A client only ever
        // replaces an id cornice handed out, and every id handed out is below the counter, so anything at or
        // above it is a stale or invented number: allocating fresh for those keeps the counter from ever
        // colliding with an id a client supplied (the queue keeps `replaces_id` for the ones below it).
        let id = if replaces_id != 0 && replaces_id < self.next_id.load(std::sync::atomic::Ordering::Relaxed) {
            replaces_id
        } else {
            self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        };
        let urgency = match hints.get("urgency").and_then(|v| u8::try_from(v.clone()).ok()) {
            Some(0) => Urgency::Low,
            Some(2) => Urgency::Critical,
            _ => Urgency::Normal,
        };
        let actions = actions.chunks(2).filter_map(|c| Some((c.first()?.clone(), c.get(1)?.clone()))).collect();
        let _ = self.tx.send(Request::Notify { id, replaces_id, app_name, summary, body, actions, urgency, expire_timeout });
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.tx.send(Request::Close { id });
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("cornice".into(), "cornice".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }
}

pub struct Control {
    tx: calloop::channel::Sender<Request>,
    control_tx: calloop::channel::Sender<ControlRequest>,
}

#[interface(name = "org.cornice.Control")]
impl Control {
    fn close_all(&self) {
        let _ = self.tx.send(Request::CloseAll);
    }

    fn toggle_bar(&self) {
        let _ = self.control_tx.send(ControlRequest::ToggleBar);
    }

    fn set_bar_visible(&self, visible: bool) {
        let _ = self.control_tx.send(ControlRequest::SetBarVisible(visible));
    }
}

/// Own the bus names and register the objects. A name held by another process is only a degradation — the bar
/// and its control surface keep running — so this returns Err only when the session bus itself is unusable.
pub fn spawn(tx: calloop::channel::Sender<Request>, control_tx: calloop::channel::Sender<ControlRequest>) -> Result<Connection, String> {
    let conn = Connection::session().map_err(|e| format!("failed to connect to the session bus: {e}"))?;
    conn.object_server()
        .at(PATH, NotifyDaemon { tx: tx.clone(), next_id: std::sync::atomic::AtomicU32::new(1) })
        .map_err(|e| format!("failed to register the D-Bus object: {e}"))?;
    conn.object_server()
        .at(PATH, Control { tx, control_tx })
        .map_err(|e| format!("failed to register the D-Bus control interface: {e}"))?;
    for (name, what) in [(BUS_NAME, "notifications"), (CONTROL_BUS_NAME, "bar control")] {
        // `DoNotQueue` matters: without it a name someone else holds is queued for silently and cornice would
        // look like a working notification daemon that never receives anything.
        match conn.request_name_with_flags(name, RequestNameFlags::DoNotQueue.into()) {
            Ok(RequestNameReply::PrimaryOwner) => {}
            Ok(_) => eprintln!("cornice: {name} is held by another process, {what} unavailable"),
            Err(e) => eprintln!("cornice: cannot request {name}, {what} unavailable: {e}"),
        }
    }
    Ok(conn)
}

pub fn emit_closed(conn: &Connection, id: u32, reason: u32) -> zbus::Result<()> {
    conn.emit_signal(None::<()>, PATH, IFACE, "NotificationClosed", &(id, reason))
}

pub fn emit_action(conn: &Connection, id: u32, key: &str) -> zbus::Result<()> {
    conn.emit_signal(None::<()>, PATH, IFACE, "ActionInvoked", &(id, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon() -> (NotifyDaemon, calloop::channel::Channel<Request>) {
        let (tx, rx) = calloop::channel::channel::<Request>();
        (NotifyDaemon { tx, next_id: std::sync::atomic::AtomicU32::new(1) }, rx)
    }

    fn notify(daemon: &NotifyDaemon, replaces_id: u32) -> u32 {
        daemon.notify("app".into(), replaces_id, String::new(), "s".into(), "b".into(), vec![], std::collections::HashMap::new(), -1)
    }

    #[test]
    fn ids_come_from_the_counter() {
        let (daemon, _rx) = daemon();
        assert_eq!(notify(&daemon, 0), 1);
        assert_eq!(notify(&daemon, 0), 2);
    }

    #[test]
    fn replacing_a_live_id_keeps_that_id() {
        let (daemon, _rx) = daemon();
        notify(&daemon, 0);
        notify(&daemon, 0);
        assert_eq!(notify(&daemon, 1), 1, "the client keeps the handle it already has");
        assert_eq!(notify(&daemon, 2), 2);
    }

    /// The reason for the counter comparison: an invented id must not capture an id the counter hands out
    /// later, or two entries would carry the same id and share one card surface.
    #[test]
    fn an_invented_id_cannot_collide_with_a_future_id() {
        let (daemon, _rx) = daemon();
        notify(&daemon, 0);
        notify(&daemon, 0);
        let invented = notify(&daemon, 3);
        assert_eq!(invented, 3, "the reply is always the id the queue uses");
        assert_eq!(notify(&daemon, 0), 4, "the next allocation must not repeat the invented id");
    }

    #[test]
    fn a_stale_id_from_a_previous_run_is_treated_as_new() {
        let (daemon, _rx) = daemon();
        assert_eq!(notify(&daemon, 77), 1);
    }
}

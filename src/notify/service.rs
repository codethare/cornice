//! D-Bus side: a zbus blocking connection plus the interface object. Interface methods never block; they only post messages to the main loop.

use zbus::blocking::Connection;
use zbus::interface;

use super::queue::{Request, Urgency};

pub const PATH: &str = "/org/freedesktop/Notifications";
const IFACE: &str = "org.freedesktop.Notifications";
const BUS_NAME: &str = "org.freedesktop.Notifications";

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
        // app_name is the fixed first argument of the freedesktop contract, but this implementation never shows the source app name: it accepts it and stores nothing.
        let _ = app_name;
        let id = if replaces_id != 0 { replaces_id } else { self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed) };
        let urgency = match hints.get("urgency").and_then(|v| u8::try_from(v.clone()).ok()) {
            Some(0) => Urgency::Low,
            Some(2) => Urgency::Critical,
            _ => Urgency::Normal,
        };
        let actions = actions.chunks(2).filter_map(|c| Some((c.first()?.clone(), c.get(1)?.clone()))).collect();
        let _ = self.tx.send(Request::Notify { id, replaces_id, summary, body, actions, urgency, expire_timeout });
        id
    }

    fn close_notification(&self, id: u32) {
        let _ = self.tx.send(Request::Close { id });
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        ("cornice".into(), "cornice".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }
}

/// Own the bus name and register the object; returns Err when the name is taken and the caller degrades.
pub fn spawn(tx: calloop::channel::Sender<Request>) -> Result<Connection, String> {
    let conn = Connection::session().map_err(|e| format!("failed to connect to the session bus: {e}"))?;
    conn.request_name(BUS_NAME).map_err(|e| format!("org.freedesktop.Notifications is already taken: {e}"))?;
    conn.object_server()
        .at(PATH, NotifyDaemon { tx, next_id: std::sync::atomic::AtomicU32::new(1) })
        .map_err(|e| format!("failed to register the D-Bus object: {e}"))?;
    Ok(conn)
}

pub fn emit_closed(conn: &Connection, id: u32, reason: u32) -> zbus::Result<()> {
    conn.emit_signal(None::<()>, PATH, IFACE, "NotificationClosed", &(id, reason))
}

pub fn emit_action(conn: &Connection, id: u32, key: &str) -> zbus::Result<()> {
    conn.emit_signal(None::<()>, PATH, IFACE, "ActionInvoked", &(id, key))
}

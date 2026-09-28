//! Client mode: the same binary invoked with a subcommand, talking to the running daemon over D-Bus.
//! It reads no config and opens no Wayland connection, so dismissing keeps working while the bar itself is broken.
//!
//! The verb follows `makoctl` / `fnottctl` (`dismiss`, with `-a` / `--all` for the whole queue), and the
//! `notification` group keeps it apart from whatever bar-side command comes later.

use crate::notify::service::{BUS_NAME, CONTROL_IFACE, IFACE, PATH};

/// Process exit code: 0 on success, 1 on a D-Bus failure, 2 on a usage error.
pub fn run(args: &[String]) -> i32 {
    match args {
        [group, command, rest @ ..] if group == "notification" && command == "dismiss" => dismiss(rest),
        _ => usage(),
    }
}

fn dismiss(args: &[String]) -> i32 {
    let call = match args {
        [flag] if flag == "-a" || flag == "--all" => call(CONTROL_IFACE, "CloseAll", &()),
        [id] => match id.parse::<u32>() {
            Ok(id) => call(IFACE, "CloseNotification", &(id,)),
            Err(_) => return usage(),
        },
        _ => return usage(),
    };
    match call {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("cornice: {error}");
            1
        }
    }
}

fn usage() -> i32 {
    eprintln!("usage: cornice notification dismiss <id> | -a | --all");
    2
}

fn call<B: serde::ser::Serialize + zbus::zvariant::DynamicType>(interface: &str, method: &str, body: &B) -> Result<(), String> {
    let connection = zbus::blocking::Connection::session().map_err(|error| format!("no session bus: {error}"))?;
    connection.call_method(Some(BUS_NAME), PATH, Some(interface), method, body).map(|_| ()).map_err(|error| format!("{method} failed: {error}"))
}

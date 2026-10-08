//! Client mode: the same binary invoked with a subcommand, talking to the running daemon over D-Bus.
//! It reads no config and opens no Wayland connection, so both groups keep working while the daemon's
//! rendering is broken.
//!
//! The verbs follow `makoctl` / `fnottctl` (`dismiss`, with `-a` / `--all` for the whole queue), and the
//! `notification` group keeps a bar-side command from colliding with it.

use crate::notify::service::{BUS_NAME, CONTROL_BUS_NAME, CONTROL_IFACE, IFACE, PATH};

/// Process exit code: 0 on success, 1 on a D-Bus failure, 2 on a usage error.
pub fn run(args: &[String]) -> i32 {
    match args {
        [group, command, rest @ ..] if group == "notification" && command == "dismiss" => dismiss(rest),
        [group, command] if group == "bar" => bar(command),
        _ => usage(),
    }
}

fn dismiss(args: &[String]) -> i32 {
    let call = match args {
        [flag] if flag == "-a" || flag == "--all" => call(BUS_NAME, CONTROL_IFACE, "CloseAll", &()),
        [id] => match id.parse::<u32>() {
            Ok(id) => call(BUS_NAME, IFACE, "CloseNotification", &(id,)),
            Err(_) => return usage(),
        },
        _ => return usage(),
    };
    report(call)
}

fn bar(command: &str) -> i32 {
    let call = match command {
        "toggle" => call(CONTROL_BUS_NAME, CONTROL_IFACE, "ToggleBar", &()),
        "show" => call(CONTROL_BUS_NAME, CONTROL_IFACE, "SetBarVisible", &(true,)),
        "hide" => call(CONTROL_BUS_NAME, CONTROL_IFACE, "SetBarVisible", &(false,)),
        _ => return usage(),
    };
    report(call)
}

fn report(call: Result<(), String>) -> i32 {
    match call {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("cornice: {error}");
            1
        }
    }
}

fn usage() -> i32 {
    eprintln!("usage: cornice notification dismiss <id> | -a | --all\n       cornice bar toggle | show | hide");
    2
}

fn call<B: serde::ser::Serialize + zbus::zvariant::DynamicType>(destination: &str, interface: &str, method: &str, body: &B) -> Result<(), String> {
    let connection = zbus::blocking::Connection::session().map_err(|error| format!("no session bus: {error}"))?;
    connection
        .call_method(Some(destination), PATH, Some(interface), method, body)
        .map(|_| ())
        .map_err(|error| format!("{method} failed: {error}"))
}

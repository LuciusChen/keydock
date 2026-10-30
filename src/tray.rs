//! Apps that close to the tray have no window left, and some (QQ) start a
//! second instance when launched again. Activating their tray icon
//! (StatusNotifierItem) brings the running instance back instead.

use std::path::Path;

use gio::glib::{Error, Variant, prelude::*};
use gio::{BusType, Cancellable, DBusCallFlags, DBusConnection};

use crate::config::app_id_matches;

/// Activates the tray icon of the app's running process, if it has one.
pub fn activate(app_id: &str) -> bool {
    try_activate(app_id).unwrap_or_else(|e| {
        eprintln!("keydock: tray: {e}");
        false
    })
}

fn try_activate(app_id: &str) -> Result<bool, Error> {
    let conn = gio::bus_get_sync(BusType::Session, Cancellable::NONE)?;
    let Ok(reply) = call(
        &conn,
        ("org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher"),
        ("org.freedesktop.DBus.Properties", "Get"),
        (
            "org.kde.StatusNotifierWatcher",
            "RegisteredStatusNotifierItems",
        )
            .to_variant(),
    ) else {
        return Ok(false); // no tray running
    };
    let items: Vec<String> = reply
        .child_value(0)
        .as_variant()
        .and_then(|v| v.get())
        .unwrap_or_default();
    for item in &items {
        let (service, path) = split_item(item);
        let Some((pid,)) = call(
            &conn,
            ("org.freedesktop.DBus", "/org/freedesktop/DBus"),
            ("org.freedesktop.DBus", "GetConnectionUnixProcessID"),
            (service,).to_variant(),
        )
        .ok()
        .and_then(|r| r.get::<(u32,)>()) else {
            continue;
        };
        let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
            continue;
        };
        if exe_matches(app_id, &exe) {
            call(
                &conn,
                (service, path),
                ("org.kde.StatusNotifierItem", "Activate"),
                (0i32, 0i32).to_variant(),
            )?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// A blocking method call; the timeout keeps an unresponsive tray app from
/// stalling keydock.
fn call(
    conn: &DBusConnection,
    (dest, path): (&str, &str),
    (iface, method): (&str, &str),
    args: Variant,
) -> Result<Variant, Error> {
    conn.call_sync(
        Some(dest),
        path,
        iface,
        method,
        Some(&args),
        None,
        DBusCallFlags::NONE,
        500,
        Cancellable::NONE,
    )
}

/// Splits a registered item, "service/object/path" or a bare service.
fn split_item(item: &str) -> (&str, &str) {
    match item.find('/') {
        Some(i) => (&item[..i], &item[i..]),
        None => (item, "/StatusNotifierItem"),
    }
}

/// Matches the app id against the executable name, e.g. "QQ" and /opt/QQ/qq.
fn exe_matches(app_id: &str, exe: &Path) -> bool {
    exe.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| app_id_matches(app_id, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_items() {
        assert_eq!(
            split_item(":1.37/StatusNotifierItem"),
            (":1.37", "/StatusNotifierItem")
        );
        assert_eq!(
            split_item(":1.35/org/ayatana/NotificationItem/udiskie"),
            (":1.35", "/org/ayatana/NotificationItem/udiskie")
        );
        assert_eq!(
            split_item("org.kde.StatusNotifierItem-5-1"),
            ("org.kde.StatusNotifierItem-5-1", "/StatusNotifierItem")
        );
    }

    #[test]
    fn matches_executable_name() {
        assert!(exe_matches("QQ", Path::new("/opt/QQ/qq")));
        assert!(exe_matches("discord", Path::new("/opt/discord/Discord")));
        assert!(!exe_matches(
            "org.telegram.desktop",
            Path::new("/usr/bin/Telegram")
        ));
    }
}

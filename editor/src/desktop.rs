//! Which desktop Shotput is running on. GNOME captures through the Shell extension; everywhere
//! else the app captures by itself (see `native`).

use gtk::{gio, glib};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Desktop {
    Gnome,
    Kde,
    Hyprland,
    Sway,
    Other,
}

pub fn current() -> Desktop {
    let names = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    let has = |n: &str| names.split(':').any(|d| d == n);
    if has("kde") {
        Desktop::Kde
    } else if has("gnome") || has("ubuntu") || has("unity") {
        Desktop::Gnome
    } else if has("hyprland") || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        Desktop::Hyprland
    } else if has("sway") || std::env::var_os("SWAYSOCK").is_some() {
        Desktop::Sway
    } else {
        Desktop::Other
    }
}

/// True when the Shotput GNOME Shell extension is running and answering.
pub fn extension_running() -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return false };
    bus.call_sync(
        Some(crate::output::SHELL_BUS),
        crate::output::SHELL_PATH,
        "org.freedesktop.DBus.Introspectable",
        "Introspect",
        None,
        Some(glib::VariantTy::new("(s)").unwrap()),
        gio::DBusCallFlags::NO_AUTO_START,
        1500,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|v| v.child_value(0).str().map(|s| s.contains(crate::output::SHELL_IFACE)))
    .unwrap_or(false)
}

/// Whether this app does captures itself: any desktop without the GNOME extension.
pub fn captures_natively() -> bool {
    current() != Desktop::Gnome && !extension_running()
}

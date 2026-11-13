//! System-wide shortcuts without the GNOME extension, through the GlobalShortcuts portal
//! (KDE Plasma, Hyprland and others). The desktop asks once which keys to give Shotput and
//! remembers them; they can be changed later in the desktop's own shortcut settings.
//! On desktops without the portal (Sway), bind `shotput area` and friends in the config.

use crate::settings::Settings;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// Shortcut id, settings key, description.
const ACTIONS: [(&str, &str, &str); 8] = [
    ("area", "shortcut-area", "Capture area"),
    ("annotate", "shortcut-area-annotate", "Capture area and annotate"),
    ("window", "shortcut-window", "Capture window"),
    ("screen", "shortcut-screen", "Capture screen"),
    ("text", "shortcut-text", "Copy text from area"),
    ("last", "shortcut-annotate-last", "Annotate last capture"),
    ("repeat", "shortcut-repeat-area", "Repeat last area"),
    ("clipboard", "shortcut-annotate-clipboard", "Annotate clipboard image"),
];

thread_local! {
    static SESSION: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// `<Control><Shift>Print` → `CTRL+SHIFT+Print`, the trigger syntax the portal takes.
pub fn trigger(accel: &str) -> String {
    let mut parts = Vec::new();
    let mut rest = accel;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else { break };
        let modifier = rest[start + 1..start + end].to_lowercase();
        rest = &rest[start + end + 1..];
        match modifier.as_str() {
            "control" | "ctrl" | "primary" => parts.push("CTRL"),
            "shift" => parts.push("SHIFT"),
            "alt" | "mod1" => parts.push("ALT"),
            "super" | "mod4" | "meta" => parts.push("LOGO"),
            _ => {}
        }
    }
    let key = rest.trim();
    let key = if key.len() == 1 { key.to_uppercase() } else { key.to_string() };
    parts.iter().map(|s| s.to_string()).chain(std::iter::once(key)).collect::<Vec<_>>().join("+")
}

fn token() -> String {
    format!("shotput{}", glib::random_int_range(0, i32::MAX))
}

/// Waits for the Response of a portal request.
async fn response(bus: &gio::DBusConnection, handle_token: &str) -> Option<glib::VariantDict> {
    let sender = bus.unique_name()?.trim_start_matches(':').replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{handle_token}");
    let (tx, rx) = futures_channel_oneshot();
    let tx = RefCell::new(Some(tx));
    #[allow(deprecated)]
    let id = bus.signal_subscribe(
        Some(PORTAL),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, params| {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(params.clone());
            }
        },
    );
    let result = rx.await.ok();
    #[allow(deprecated)]
    bus.signal_unsubscribe(id);
    let params = result?;
    let code = params.child_value(0).get::<u32>().unwrap_or(2);
    (code == 0).then(|| glib::VariantDict::new(Some(&params.child_value(1))))
}

/// A tiny one-shot channel over glib's main loop, to avoid another dependency.
fn futures_channel_oneshot() -> (OneshotSender, OneshotReceiver) {
    let shared = std::rc::Rc::new(RefCell::new((None::<glib::Variant>, None::<std::task::Waker>)));
    (OneshotSender(shared.clone()), OneshotReceiver(shared))
}
struct OneshotSender(std::rc::Rc<RefCell<(Option<glib::Variant>, Option<std::task::Waker>)>>);
struct OneshotReceiver(std::rc::Rc<RefCell<(Option<glib::Variant>, Option<std::task::Waker>)>>);
impl OneshotSender {
    fn send(self, v: glib::Variant) -> Result<(), ()> {
        let mut s = self.0.borrow_mut();
        s.0 = Some(v);
        if let Some(w) = s.1.take() {
            w.wake();
        }
        Ok(())
    }
}
impl std::future::Future for OneshotReceiver {
    type Output = Result<glib::Variant, ()>;
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        let mut s = self.0.borrow_mut();
        match s.0.take() {
            Some(v) => std::task::Poll::Ready(Ok(v)),
            None => {
                s.1 = Some(cx.waker().clone());
                std::task::Poll::Pending
            }
        }
    }
}

/// Registers the app id with the portal (needed outside Flatpak on newer portals), opens a
/// session, binds the shortcuts and runs actions when they fire.
pub fn start(app: &adw::Application) {
    let app = app.clone();
    glib::MainContext::default().spawn_local(async move {
        if let Err(err) = bind(&app).await {
            eprintln!("shotput: global shortcuts are not available: {err}");
        }
    });
}

async fn bind(app: &adw::Application) -> Result<(), String> {
    let bus = gio::bus_get_future(gio::BusType::Session).await.map_err(|e| e.to_string())?;
    // xdg-desktop-portal 1.19+ wants host apps to say who they are; older ones work it out.
    let _ = bus
        .call_future(
            Some(PORTAL),
            PORTAL_PATH,
            "org.freedesktop.host.portal.Registry",
            "Register",
            Some(&(crate::APP_ID, glib::VariantDict::new(None)).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            3000,
        )
        .await;

    let handle = token();
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", &handle);
    options.insert("session_handle_token", token());
    let waiting = response(&bus, &handle);
    bus.call_future(Some(PORTAL), PORTAL_PATH, IFACE, "CreateSession", Some(&(options,).to_variant()), None, gio::DBusCallFlags::NONE, 10000)
        .await
        .map_err(|e| e.to_string())?;
    let result = waiting.await.ok_or("the portal declined a session")?;
    let session = result.lookup::<String>("session_handle").ok().flatten().ok_or("no session handle")?;
    SESSION.with(|s| *s.borrow_mut() = Some(session.clone()));

    // Fire actions for this session's shortcuts.
    let weak = app.downgrade();
    #[allow(deprecated)]
    bus.signal_subscribe(
        Some(PORTAL),
        Some(IFACE),
        Some("Activated"),
        Some(PORTAL_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, params| {
            let id = params.child_value(1).str().map(str::to_string).unwrap_or_default();
            if let Some(app) = weak.upgrade() {
                crate::native::run(&app, &id);
            }
        },
    );

    let settings = Settings::new();
    let shortcuts: Vec<(String, glib::VariantDict)> = ACTIONS
        .iter()
        .map(|(id, key, description)| {
            let props = glib::VariantDict::new(None);
            props.insert("description", *description);
            if let Some(accel) = settings.strv(key).first().filter(|a| !a.is_empty()) {
                props.insert("preferred_trigger", trigger(accel));
            }
            (id.to_string(), props)
        })
        .collect();
    let handle = token();
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", &handle);
    let waiting = response(&bus, &handle);
    let session_path = glib::variant::ObjectPath::try_from(session).map_err(|e| e.to_string())?;
    bus.call_future(
        Some(PORTAL),
        PORTAL_PATH,
        IFACE,
        "BindShortcuts",
        Some(&(session_path, shortcuts, "", options).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        120_000,
    )
    .await
    .map_err(|e| e.to_string())?;
    waiting.await.ok_or("the shortcuts were not bound")?;
    Ok(())
}

/// Opens the desktop's own dialog for changing Shotput's shortcuts (portal version 2).
pub fn configure() {
    let Some(session) = SESSION.with(|s| s.borrow().clone()) else { return };
    glib::MainContext::default().spawn_local(async move {
        let Ok(bus) = gio::bus_get_future(gio::BusType::Session).await else { return };
        let Ok(path) = glib::variant::ObjectPath::try_from(session) else { return };
        let _ = bus
            .call_future(
                Some(PORTAL),
                PORTAL_PATH,
                IFACE,
                "ConfigureShortcuts",
                Some(&(path, "", glib::VariantDict::new(None)).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                10000,
            )
            .await;
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn triggers() {
        assert_eq!(super::trigger("Print"), "Print");
        assert_eq!(super::trigger("<Control>Print"), "CTRL+Print");
        assert_eq!(super::trigger("<Control><Shift>Print"), "CTRL+SHIFT+Print");
        assert_eq!(super::trigger("<Shift>F3"), "SHIFT+F3");
        assert_eq!(super::trigger("<Super>a"), "LOGO+A");
    }
}

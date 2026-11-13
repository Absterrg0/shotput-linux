//! The windows on screen, front to back, for snapping and window captures without GNOME:
//! a KWin script on KDE Plasma, hyprctl on Hyprland, swaymsg on Sway.

use crate::desktop::{self, Desktop};
use crate::model::R;
use crate::overlay::WinInfo;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

pub const WINDOWS_PATH: &str = "/io/github/shotput/Windows";
const WINDOWS_IFACE: &str = "io.github.shotput.Windows";
const WINDOWS_XML: &str = r#"<node><interface name="io.github.shotput.Windows">
  <method name="Report"><arg type="s" direction="in" name="json"/></method>
</interface></node>"#;

thread_local! {
    static PENDING: RefCell<Option<Box<dyn FnOnce(Vec<WinInfo>)>>> = const { RefCell::new(None) };
}

/// Lists windows, then calls back. Calls back with nothing rather than waiting long.
pub fn list(app: &adw::Application, done: impl FnOnce(Vec<WinInfo>) + 'static) {
    match desktop::current() {
        Desktop::Kde => kwin(app, done),
        Desktop::Hyprland => command(&["hyprctl", "-j", "clients"], parse_hyprland, done),
        Desktop::Sway => command(&["swaymsg", "-t", "get_tree", "-r"], parse_sway, done),
        _ => done(Vec::new()),
    }
}

fn command(argv: &[&str], parse: fn(&str) -> Vec<WinInfo>, done: impl FnOnce(Vec<WinInfo>) + 'static) {
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_ref()).collect();
    let Ok(process) = gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE) else {
        done(Vec::new());
        return;
    };
    process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| {
        let out = result.ok().and_then(|(o, _)| o).map(|s| s.to_string()).unwrap_or_default();
        done(parse(&out));
    });
}

fn pretty(class: &str) -> Option<String> {
    let name = class.rsplit('.').next().unwrap_or(class).trim();
    let mut chars = name.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

fn parse_hyprland(json: &str) -> Vec<WinInfo> {
    let Ok(serde_json::Value::Array(clients)) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let mut list: Vec<(i64, WinInfo)> = clients
        .iter()
        .filter(|c| c["mapped"].as_bool().unwrap_or(true) && !c["hidden"].as_bool().unwrap_or(false))
        .filter_map(|c| {
            let at = c["at"].as_array()?;
            let size = c["size"].as_array()?;
            let rect = R::new(at[0].as_f64()?, at[1].as_f64()?, size[0].as_f64()?, size[1].as_f64()?);
            let order = c["focusHistoryID"].as_i64().unwrap_or(999);
            Some((order, WinInfo { rect, name: c["class"].as_str().and_then(pretty) }))
        })
        .collect();
    // Most recently focused first approximates front to back.
    list.sort_by_key(|(order, _)| *order);
    list.into_iter().map(|(_, w)| w).collect()
}

fn parse_sway(json: &str) -> Vec<WinInfo> {
    let Ok(tree) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let mut tiled = Vec::new();
    let mut floating = Vec::new();
    fn walk(node: &serde_json::Value, floating_parent: bool, tiled: &mut Vec<WinInfo>, floating: &mut Vec<WinInfo>) {
        let is_window = node["pid"].is_number() && node["visible"].as_bool().unwrap_or(false);
        if is_window {
            let r = &node["rect"];
            if let (Some(x), Some(y), Some(w), Some(h)) = (r["x"].as_f64(), r["y"].as_f64(), r["width"].as_f64(), r["height"].as_f64()) {
                let name = node["app_id"].as_str().or(node["window_properties"]["class"].as_str()).and_then(pretty);
                let info = WinInfo { rect: R::new(x, y, w, h), name };
                if floating_parent { floating.push(info) } else { tiled.push(info) }
            }
        }
        for child in node["nodes"].as_array().into_iter().flatten() {
            walk(child, floating_parent, tiled, floating);
        }
        for child in node["floating_nodes"].as_array().into_iter().flatten() {
            walk(child, true, tiled, floating);
        }
    }
    walk(&tree, false, &mut tiled, &mut floating);
    floating.reverse();
    floating.into_iter().chain(tiled).collect()
}

// MARK: KDE Plasma

const KWIN_SCRIPT: &str = r#"
const out = [];
const list = workspace.stackingOrder || workspace.clientList();
for (let i = list.length - 1; i >= 0; i--) {
    const w = list[i];
    if (!(w.normalWindow || w.dialog || w.utility) || w.minimized || w.hidden || w.deleted) continue;
    const here = w.onAllDesktops || (w.desktops ? w.desktops.indexOf(workspace.currentDesktop) >= 0 : w.desktop === workspace.currentDesktop);
    if (!here) continue;
    const g = w.frameGeometry;
    out.push([g.x, g.y, g.width, g.height, w.resourceClass ? String(w.resourceClass) : ""]);
}
callDBus("io.github.shotput.Shotput", "/io/github/shotput/Windows", "io.github.shotput.Windows", "Report", JSON.stringify(out));
"#;

/// Exports the object the KWin script reports to. Called once at startup.
pub fn export(connection: &gio::DBusConnection) {
    let Ok(info) = gio::DBusNodeInfo::for_xml(WINDOWS_XML) else { return };
    let Some(iface) = info.lookup_interface(WINDOWS_IFACE) else { return };
    let _ = connection
        .register_object(WINDOWS_PATH, &iface)
        .method_call(|_, _, _, _, _, params, invocation| {
            let json = params.child_value(0).str().unwrap_or("[]").to_string();
            let list: Vec<WinInfo> = serde_json::from_str::<Vec<(f64, f64, f64, f64, String)>>(&json)
                .unwrap_or_default()
                .into_iter()
                .map(|(x, y, w, h, class)| WinInfo { rect: R::new(x, y, w, h), name: pretty(&class) })
                .collect();
            if let Some(done) = PENDING.with(|p| p.borrow_mut().take()) {
                done(list);
            }
            invocation.return_value(None);
        })
        .build();
}

fn kwin(app: &adw::Application, done: impl FnOnce(Vec<WinInfo>) + 'static) {
    let Some(bus) = app.dbus_connection() else {
        done(Vec::new());
        return;
    };
    let done: Rc<RefCell<Option<Box<dyn FnOnce(Vec<WinInfo>)>>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    {
        let done = done.clone();
        PENDING.with(|p| *p.borrow_mut() = Some(Box::new(move |list| {
            if let Some(f) = done.borrow_mut().take() {
                f(list)
            }
        })));
    }
    // Answer with nothing if KWin does not report in time.
    {
        let done = done.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
            PENDING.with(|p| p.borrow_mut().take());
            if let Some(f) = done.borrow_mut().take() {
                f(Vec::new())
            }
        });
    }
    let path = crate::output::runtime_dir().join("kwin-windows.js");
    if std::fs::write(&path, KWIN_SCRIPT).is_err() {
        return;
    }
    let plugin = "shotput-windows";
    glib::MainContext::default().spawn_local(async move {
        let call = |object: &str, iface: &str, method: &str, args: Option<glib::Variant>| {
            bus.call_future(Some("org.kde.KWin"), object, iface, method, args.as_ref(), None, gio::DBusCallFlags::NONE, 2000)
        };
        let _ = call("/Scripting", "org.kde.kwin.Scripting", "unloadScript", Some((plugin,).to_variant())).await;
        let Ok(reply) = call(
            "/Scripting",
            "org.kde.kwin.Scripting",
            "loadScript",
            Some((path.to_string_lossy().to_string(), plugin).to_variant()),
        )
        .await
        else {
            return;
        };
        let id = reply.child_value(0).get::<i32>().unwrap_or(-1);
        if id < 0 {
            return;
        }
        // Plasma 6 names the script object /Scripting/Script<id>; Plasma 5 used /<id>.
        if call(&format!("/Scripting/Script{id}"), "org.kde.kwin.Script", "run", None).await.is_err() {
            let _ = call(&format!("/{id}"), "org.kde.kwin.Script", "run", None).await;
        }
        glib::timeout_add_local_once(std::time::Duration::from_millis(800), move || {
            glib::MainContext::default().spawn_local(async move {
                let _ = bus
                    .call_future(
                        Some("org.kde.KWin"),
                        "/Scripting",
                        "org.kde.kwin.Scripting",
                        "unloadScript",
                        Some(&(plugin,).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        2000,
                    )
                    .await;
            });
        });
    });
}

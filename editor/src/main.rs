//! Shotput for Linux: the editor app and command line.
//!
//! Captures, the selection overlay, the preview, pins and hotkeys live in the GNOME Shell
//! extension, which runs inside the compositor. This binary is the annotation editor (kept
//! resident so it opens instantly), the `shotput` command and `shotput://` links, and a
//! fallback that captures through the screenshot portal when the extension is not loaded.

mod backdrop;
mod canvas;
mod editor;
mod model;
mod output;
mod render;
mod settings;

use adw::prelude::*;
use editor::{CaptureInfo, Editor};
use gtk::{gdk, gio, glib};
use std::path::PathBuf;

pub const APP_ID: &str = "io.github.shotput.Shotput";

const ACTIONS: [&str; 9] = ["area", "annotate", "window", "screen", "last", "repeat", "text", "clipboard", "settings"];

const USAGE: &str = "Usage: shotput [COMMAND]

Commands:
  area        Drag an area. It is on the clipboard when you let go.
  annotate    Same, then the editor opens where you captured.
  window      Click a window to get it alone, with clean corners.
  screen      The whole display under the pointer.
  last        Opens the most recent capture in the editor.
  repeat      Captures the same area as last time, with no selection.
  text        Drag over any text and it is copied as text.
  clipboard   Opens the image on the clipboard in the editor.
  settings    Opens Shotput's settings.
  edit FILE   Opens an image in the editor.
  shotput://ACTION   The same commands as links, for launchers.

Captures run in the Shotput GNOME Shell extension. Without it, area, annotate, window and
screen fall back to GNOME's own screenshot picker.";

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let first = args.get(1).map(String::as_str);

    // Fast paths that never start GTK: they hand the action to the extension over D-Bus.
    match first {
        Some("compose") => return compose_command(&args[2..]),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return glib::ExitCode::SUCCESS;
        }
        Some(cmd) => {
            if let Some(action) = action_for(cmd) {
                if action == "blocked" {
                    return glib::ExitCode::SUCCESS;
                }
                if output::call_shell("Run", &(action.to_string(),).to_variant()) {
                    return glib::ExitCode::SUCCESS;
                }
            }
        }
        None => {
            if output::call_shell("Run", &("settings".to_string(),).to_variant()) {
                return glib::ExitCode::SUCCESS;
            }
        }
    }

    let dev = first == Some("dev-editor");
    let app = adw::Application::builder()
        .application_id(if dev { "io.github.shotput.Dev" } else { APP_ID })
        .flags(if dev {
            gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::NON_UNIQUE
        } else {
            gio::ApplicationFlags::HANDLES_COMMAND_LINE
        })
        .build();
    app.connect_startup(startup);
    app.connect_command_line(|app, line| {
        let args: Vec<String> = line.arguments().iter().map(|a| a.to_string_lossy().to_string()).collect();
        let cwd = line.cwd().unwrap_or_else(|| PathBuf::from("."));
        handle_command_line(app, &args, &cwd);
        glib::ExitCode::SUCCESS
    });
    app.connect_activate(|_| {});
    app.run_with_args(&args)
}

/// Maps a command or a `shotput://` link to an extension action. Links can only start captures
/// that put a selection on screen; a silent full-screen capture is shortcut only.
fn action_for(arg: &str) -> Option<&'static str> {
    if let Some(rest) = arg.strip_prefix("shotput://") {
        let name = rest.trim_end_matches('/');
        let mapped = match name {
            "area" => "area",
            "annotate" => "annotate",
            "window" => "window",
            "last" => "last",
            "text" => "text",
            "clipboard" => "clipboard",
            "settings" => "settings",
            _ => return None,
        };
        if mapped != "settings" && !settings::Settings::new().boolean("allows-links", true) {
            return Some("blocked");
        }
        return Some(mapped);
    }
    ACTIONS.iter().copied().find(|a| *a == arg)
}

fn startup(app: &adw::Application) {
    if let Some(display) = gdk::Display::default() {
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        gio::resources_register(
            &gio::Resource::from_data(&glib::Bytes::from_static(include_bytes!(concat!(env!("OUT_DIR"), "/shotput.gresource"))))
                .expect("bundled resources"),
        );
        gtk::IconTheme::for_display(&display).add_resource_path("/io/github/shotput/icons");
    }
    gtk::Window::set_default_icon_name(APP_ID);

    // Stay resident, so the editor opens instantly and anything this process put on the
    // clipboard stays pasteable.
    std::mem::forget(app.hold());

    // The extension opens captures through this action, without starting a process.
    let open = gio::SimpleAction::new("open", Some(glib::VariantTy::STRING));
    let weak = app.downgrade();
    open.connect_activate(move |_, param| {
        let (Some(app), Some(json)) = (weak.upgrade(), param.and_then(|p| p.str().map(str::to_string))) else { return };
        open_request(&app, &json);
    });
    app.add_action(&open);

    app.set_accels_for_action("win.copy", &["<Control>c"]);
    app.set_accels_for_action("win.save", &["<Control>s"]);
    app.set_accels_for_action("win.undo", &["<Control>z"]);
    app.set_accels_for_action("win.redo", &["<Control><Shift>z", "<Control>y"]);
    app.set_accels_for_action("win.close", &["<Control>w"]);
    app.set_accels_for_action("win.zoom-in", &["<Control>plus", "<Control>equal", "<Control>KP_Add"]);
    app.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
    app.set_accels_for_action("win.zoom-actual", &["<Control>0", "<Control>KP_0"]);
    app.set_accels_for_action("win.zoom-fit", &["<Control>9"]);
}

#[derive(serde::Deserialize, Default)]
struct OpenRequest {
    path: String,
    id: Option<String>,
    scale: Option<f64>,
    monitor: Option<(f64, f64)>,
    app: Option<String>,
    /// The unframed capture, when it was framed on a backdrop as it was taken.
    plain: Option<String>,
    fill: Option<String>,
    padding: Option<f64>,
    /// Write the edited result back to `path` for the extension.
    writes_back: Option<bool>,
}

fn open_request(app: &adw::Application, json: &str) {
    let Ok(request) = serde_json::from_str::<OpenRequest>(json) else { return };
    let Ok(texture) = gdk::Texture::from_filename(&request.path) else { return };
    let framed = request.plain.as_ref().and_then(|plain| {
        let plain = gdk::Texture::from_filename(plain).ok()?;
        let fill = match request.fill.as_deref() {
            Some("wallpaper") => backdrop::Fill::Wallpaper,
            Some(hex) => backdrop::Fill::Color(model::Rgba::parse(hex)?),
            None => return None,
        };
        Some((plain, backdrop::Style { fill, padding: request.padding.unwrap_or(48.0) }))
    });
    let writes_back = request.writes_back.unwrap_or(request.id.is_some());
    let capture = CaptureInfo {
        id: request.id.clone(),
        path: writes_back.then(|| PathBuf::from(&request.path)),
        scale: request.scale.unwrap_or(1.0).max(0.25),
        monitor: request.monitor,
        app_name: request.app,
        date: glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("time")),
        framed,
    };
    Editor::open(app, texture, capture);
}

fn open_file(app: &adw::Application, path: &std::path::Path, scale: f64) {
    match gdk::Texture::from_filename(path) {
        Ok(texture) => {
            let capture = CaptureInfo {
                id: None,
                path: None,
                scale,
                monitor: None,
                app_name: None,
                date: glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("time")),
                framed: None,
            };
            Editor::open(app, texture, capture);
        }
        Err(err) => eprintln!("shotput: cannot open {}: {err}", path.display()),
    }
}

fn handle_command_line(app: &adw::Application, args: &[String], cwd: &std::path::Path) {
    let arg = args.get(1).map(String::as_str);
    match arg {
        Some("edit") | Some("open") => {
            let scale = args.iter().position(|a| a == "--scale").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(1.0);
            for file in args[2..].iter().filter(|a| !a.starts_with("--") && a.parse::<f64>().is_err()) {
                let path = if file.starts_with("file://") { gio::File::for_uri(file).path().unwrap_or_default() } else { cwd.join(file) };
                open_file(app, &path, scale);
            }
        }
        Some("dev-editor") => {
            // shotput dev-editor OUT_DIR [IMAGE]: renders the editor with sample marks, then quits.
            let out = PathBuf::from(args.get(2).cloned().unwrap_or_else(|| ".".into()));
            let texture = args.get(3).and_then(|p| gdk::Texture::from_filename(p).ok()).unwrap_or_else(editor::dev_sample_image);
            let capture = CaptureInfo {
                id: None,
                path: None,
                scale: 1.0,
                monitor: Some((1920.0, 1080.0)),
                app_name: None,
                date: glib::DateTime::now_local().expect("time"),
                framed: None,
            };
            let e = Editor::open(app, texture, capture);
            e.dev_populate();
            let app = app.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(900), move || {
                let ok = e.dev_render(&out.join("editor-window.png"), &out.join("editor-export.png"));
                println!("dev render {}", if ok { "ok" } else { "failed" });
                app.quit();
            });
        }
        Some("--open-json") => {
            if let Some(json) = args.get(2) {
                open_request(app, json);
            }
        }
        Some(cmd) => match action_for(cmd) {
            Some("blocked") => {}
            Some(action) => fallback(app, action),
            None => {
                let path = cwd.join(cmd);
                if path.is_file() {
                    open_file(app, &path, 1.0);
                } else {
                    eprintln!("{USAGE}");
                }
            }
        },
        None => fallback(app, "settings"),
    }
}

// MARK: Fallback without the extension

/// Captures through the screenshot portal, for when the Shell extension is not loaded yet
/// (it starts with the next login after installing).
fn fallback(app: &adw::Application, action: &str) {
    match action {
        "area" | "annotate" | "window" | "text" => {
            let annotate = action == "annotate";
            let app = app.clone();
            portal_screenshot(true, move |path| {
                let Some(path) = path else { return };
                if let Ok(texture) = gdk::Texture::from_filename(&path) {
                    output::copy_png(&texture.save_to_png_bytes(), &texture);
                    if annotate || settings::Settings::new().string("after-capture", "preview") == "editor" {
                        open_file(&app, &path, 1.0);
                    } else {
                        notify(&app, "Screenshot copied", "Shotput's extension starts after you log in again.");
                    }
                }
            });
        }
        "screen" => {
            let app = app.clone();
            portal_screenshot(false, move |path| {
                if let Some(texture) = path.and_then(|p| gdk::Texture::from_filename(p).ok()) {
                    output::copy_png(&texture.save_to_png_bytes(), &texture);
                    notify(&app, "Screenshot copied", "");
                }
            });
        }
        "clipboard" => {
            let Some(display) = gdk::Display::default() else { return };
            let app = app.clone();
            display.clipboard().read_texture_async(gio::Cancellable::NONE, move |result| {
                if let Ok(Some(texture)) = result {
                    let capture = CaptureInfo {
                        id: None,
                        path: None,
                        scale: 1.0,
                        monitor: None,
                        app_name: None,
                        date: glib::DateTime::now_local().expect("time"),
                        framed: None,
                    };
                    Editor::open(&app, texture, capture);
                }
            });
        }
        "settings" => {
            let launcher = gio::Subprocess::newv(
                &["gnome-extensions".as_ref(), "prefs".as_ref(), settings::EXTENSION_UUID.as_ref()],
                gio::SubprocessFlags::NONE,
            );
            if launcher.is_err() {
                eprintln!("{USAGE}");
            }
        }
        _ => {
            if editor::count() == 0 {
                notify(app, "Shotput", "This needs the Shotput extension. Log out and back in to start it.");
            }
        }
    }
}

fn notify(app: &adw::Application, title: &str, body: &str) {
    let n = gio::Notification::new(title);
    if !body.is_empty() {
        n.set_body(Some(body));
    }
    app.send_notification(Some("shotput"), &n);
}

fn portal_screenshot(interactive: bool, done: impl FnOnce(Option<PathBuf>) + 'static) {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        done(None);
        return;
    };
    let token = format!("shotput{}", glib::random_int());
    let sender = bus.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
    let handle = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let done = std::cell::RefCell::new(Some(done));
    let subscription = std::rc::Rc::new(std::cell::Cell::new(None));
    let sub2 = subscription.clone();
    #[allow(deprecated)]
    let id = bus.signal_subscribe(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&handle),
        None,
        gio::DBusSignalFlags::NONE,
        move |bus, _, _, _, _, params| {
            if let Some(id) = sub2.take() {
                #[allow(deprecated)]
                bus.signal_unsubscribe(id);
            }
            let path = params.get::<(u32, glib::VariantDict)>().and_then(|(response, results)| {
                (response == 0).then_some(())?;
                let uri = results.lookup::<String>("uri").ok().flatten()?;
                gio::File::for_uri(&uri).path()
            });
            if let Some(done) = done.borrow_mut().take() {
                done(path);
            }
        },
    );
    subscription.set(Some(id));
    let options = glib::VariantDict::new(None);
    options.insert("handle_token", &token);
    options.insert("interactive", interactive);
    options.insert("modal", true);
    bus.call(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Screenshot",
        "Screenshot",
        Some(&("", options.end()).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        |_| {},
    );
}

// MARK: compose

/// `shotput compose IN OUT --scale S --fill wallpaper|#rrggbb --padding P [--no-shadow]`.
/// Frames a window capture on its backdrop for the extension.
fn compose_command(args: &[String]) -> glib::ExitCode {
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let (Some(input), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: shotput compose IN OUT --scale S --fill wallpaper|#rrggbb --padding P [--no-shadow]");
        return glib::ExitCode::FAILURE;
    };
    let scale = value("--scale").and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let padding = value("--padding").and_then(|s| s.parse().ok()).unwrap_or(48.0);
    let fill = match value("--fill").as_deref() {
        Some("wallpaper") | None => backdrop::Fill::Wallpaper,
        Some(hex) => backdrop::Fill::Color(model::Rgba::parse(hex).unwrap_or(model::Rgba::hex(0xE9E6DF, 1.0))),
    };
    let Ok(image) = gdk::Texture::from_filename(input) else { return glib::ExitCode::FAILURE };
    let wallpaper = if fill == backdrop::Fill::Wallpaper { backdrop::wallpaper() } else { None };
    let style = backdrop::Style { fill, padding };
    let adds_shadow = !args.iter().any(|a| a == "--no-shadow");
    let Some(result) = backdrop::compose(&image, scale, style, wallpaper.as_ref(), adds_shadow) else {
        return glib::ExitCode::FAILURE;
    };
    match result.save_to_png(out) {
        Ok(()) => glib::ExitCode::SUCCESS,
        Err(_) => glib::ExitCode::FAILURE,
    }
}

const CSS: &str = r#"
.shotput-canvas { background-color: #FBFBF9; }
.shotput-canvas.dark { background-color: #191917; }
.shotput-bar, .shotput-toast {
  background-color: alpha(@view_bg_color, 0.94);
  color: @view_fg_color;
  border-radius: 999px;
  box-shadow: 0 0 0 1px alpha(black, 0.06), 0 2px 10px alpha(black, 0.16);
}
.shotput-bar { padding: 7px 12px; }
.shotput-toast { padding: 7px 14px; font-weight: 500; }
button.shotput-circle {
  min-width: 28px; min-height: 28px; padding: 0; margin: 0;
  border-radius: 999px; background: none; box-shadow: none; border: none;
  transition: background-color 150ms ease-out;
}
button.shotput-circle:hover { background-color: alpha(currentColor, 0.08); }
button.shotput-circle:active { background-color: alpha(currentColor, 0.14); }
button.shotput-circle:disabled { opacity: 0.35; }
button.shotput-circle.selected { background-color: alpha(currentColor, 0.12); }
button.shotput-circle.accent-on { color: @accent_color; background-color: alpha(@accent_color, 0.14); }
button.shotput-swatch.selected { background: none; box-shadow: inset 0 0 0 2px @view_fg_color; }
.shotput-swatch-dot {
  min-width: 22px; min-height: 22px; border-radius: 999px;
  box-shadow: inset 0 0 0 1px alpha(black, 0.12);
}
button.shotput-swatch.selected .shotput-swatch-dot { min-width: 20px; min-height: 20px; }
.ink-0 { background-color: #E5484D; }
.ink-1 { background-color: #F76B15; }
.ink-2 { background-color: #FFC53D; }
.ink-3 { background-color: #30A46C; }
.ink-4 { background-color: #1F6FE5; }
.ink-5 { background-color: #8E4EC6; }
.ink-6 { background-color: #2B2A26; }
.ink-7 { background-color: #FFFFFF; }
.shotput-weight-bar { min-width: 14px; border-radius: 999px; background-color: @view_fg_color; }
.weight-0 { min-height: 2px; }
.weight-1 { min-height: 4px; }
.weight-2 { min-height: 6px; }
.weight-3 { min-height: 8px; }
.shotput-sep { min-width: 1px; min-height: 18px; margin: 0 6px; background-color: alpha(@view_fg_color, 0.12); }
.shotput-tools button { min-width: 34px; }
.shotput-tools button:checked { background-color: alpha(currentColor, 0.12); }
textview.shotput-text { padding: 0; }
"#;

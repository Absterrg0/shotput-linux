//! Captures without the GNOME extension, on KDE Plasma, Hyprland, Sway and other Wayland
//! desktops: grab each monitor, select on a frozen overlay, then copy, save, preview or annotate,
//! the same steps the extension takes on GNOME.

use crate::editor::{CaptureInfo, Editor};
use crate::grab::{self, Still};
use crate::model::R;
use crate::output;
use crate::overlay::{Mode, Outcome, Overlay, WinInfo};
use crate::settings::Settings;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// The most recent capture, for "annotate last" and the preview.
#[derive(Clone)]
pub struct Capture {
    pub id: String,
    pub texture: gdk::Texture,
    pub path: PathBuf,
    pub scale: f64,
    pub app_name: Option<String>,
    pub date: glib::DateTime,
}

thread_local! {
    static LAST: RefCell<Option<Capture>> = const { RefCell::new(None) };
    static LAST_AREA: Cell<Option<R>> = const { Cell::new(None) };
    static ACTIVE: RefCell<Option<Rc<Overlay>>> = const { RefCell::new(None) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

pub fn last() -> Option<Capture> {
    LAST.with(|l| l.borrow().clone())
}

/// The overlay on screen, for tests to drive.
pub fn active_overlay() -> Option<Rc<Overlay>> {
    ACTIVE.with(|a| a.borrow().clone())
}

fn captures_dir() -> PathBuf {
    let dir = glib::user_cache_dir().join("shotput/captures");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn run(app: &adw::Application, action: &str) {
    crate::debug!("run {action}");
    // Pressing a capture shortcut again while selecting backs out.
    if let Some(overlay) = active_overlay() {
        overlay.finish(Outcome::Cancelled);
        return;
    }
    match action {
        "area" | "annotate" | "text" | "window" => {
            let mode = if action == "window" { Mode::Window } else { Mode::Area };
            select(app, mode, action.to_string());
        }
        "screen" => shoot_screen(app),
        "repeat" => match LAST_AREA.with(|a| a.get()) {
            Some(rect) => {
                let app = app.clone();
                grab_then(&app.clone(), move |stills| {
                    if let Some(c) = crop_area(&stills, rect) {
                        deliver(&app, c, rect_monitor_scale(&stills, rect), None, "area");
                    }
                });
            }
            None => select(app, Mode::Area, "area".into()),
        },
        "last" => {
            if let Some(c) = last() {
                open_editor(app, &c);
            }
        }
        _ => {}
    }
}

fn grab_then(app: &adw::Application, then: impl FnOnce(Vec<Still>) + 'static) {
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    let app = app.clone();
    grab::grab(move |result| {
        BUSY.with(|b| b.set(false));
        crate::debug!("grab: {:?}", result.as_ref().map(|s| s.iter().map(|s| (s.texture.width(), s.texture.height())).collect::<Vec<_>>()));
        match result {
            Ok(stills) if !stills.is_empty() => then(stills),
            Ok(_) => {}
            Err(err) => crate::notify(&app, "Shotput could not capture the screen", &err),
        }
    });
}

fn select(app: &adw::Application, mode: Mode, action: String) {
    // Ask for the window list while the screen is being grabbed.
    let windows: Rc<RefCell<Option<Vec<WinInfo>>>> = Rc::new(RefCell::new(None));
    let stills_slot: Rc<RefCell<Option<Vec<Still>>>> = Rc::new(RefCell::new(None));
    let open = {
        let app = app.clone();
        let windows = windows.clone();
        let stills_slot = stills_slot.clone();
        Rc::new(move || {
            if windows.borrow().is_none() || stills_slot.borrow().is_none() {
                return;
            }
            let stills = stills_slot.borrow_mut().take().unwrap();
            let list = windows.borrow_mut().take().unwrap();
            crate::debug!("overlay: {} monitors, windows {:?}, layer shell {}", stills.len(), list.iter().map(|w| (w.rect, w.name.clone())).collect::<Vec<_>>(), crate::layer::available());
            let app2 = app.clone();
            let action = action.clone();
            let stills_for_result: Rc<Vec<(gdk::Rectangle, gdk::Texture)>> =
                Rc::new(stills.iter().map(|s| (s.geometry, s.texture.clone())).collect());
            let overlay = Overlay::open(&app, stills, list, mode, move |outcome| {
                ACTIVE.with(|a| a.borrow_mut().take());
                match outcome {
                    Outcome::Cancelled => {}
                    Outcome::Area(rect) => {
                        LAST_AREA.with(|a| a.set(Some(rect)));
                        if let Some((texture, scale)) = crop_from(&stills_for_result, rect) {
                            deliver(&app2, texture, scale, None, &action);
                        }
                    }
                    Outcome::Window(win) => {
                        if let Some((texture, scale)) = crop_from(&stills_for_result, win.rect) {
                            deliver(&app2, texture, scale, win.name, &action);
                        }
                    }
                }
            });
            ACTIVE.with(|a| *a.borrow_mut() = Some(overlay));
        })
    };
    {
        let open = open.clone();
        let windows = windows.clone();
        crate::windows::list(app, move |list| {
            *windows.borrow_mut() = Some(list);
            open();
        });
    }
    grab_then(app, move |stills| {
        *stills_slot.borrow_mut() = Some(stills);
        open();
    });
}

fn rect_of(g: &gdk::Rectangle) -> R {
    R::new(g.x() as f64, g.y() as f64, g.width() as f64, g.height() as f64)
}

/// Cuts a layout rectangle out of the still of the monitor it is mostly on.
fn crop_from(stills: &[(gdk::Rectangle, gdk::Texture)], rect: R) -> Option<(gdk::Texture, f64)> {
    let (geometry, texture) = stills
        .iter()
        .max_by(|a, b| {
            let area = |g: &gdk::Rectangle| rect_of(g).intersection(&rect).map(|r| r.w * r.h).unwrap_or(0.0);
            area(&a.0).total_cmp(&area(&b.0))
        })?;
    let m = rect_of(geometry);
    let local = rect.intersection(&m)?.offset(crate::model::p(-m.x, -m.y));
    let scale = texture.width() as f64 / m.w.max(1.0);
    let px = R::new(local.x * scale, local.y * scale, local.w * scale, local.h * scale);
    grab::crop(texture, px).map(|t| (t, scale))
}

fn crop_area(stills: &[Still], rect: R) -> Option<gdk::Texture> {
    let list: Vec<(gdk::Rectangle, gdk::Texture)> = stills.iter().map(|s| (s.geometry, s.texture.clone())).collect();
    crop_from(&list, rect).map(|(t, _)| t)
}

fn rect_monitor_scale(stills: &[Still], rect: R) -> f64 {
    stills.iter().find(|s| rect_of(&s.geometry).contains(rect.center())).map(|s| s.scale()).unwrap_or(1.0)
}

/// The whole of the first monitor (on Wayland an app cannot ask where the pointer is).
fn shoot_screen(app: &adw::Application) {
    let app = app.clone();
    grab_then(&app.clone(), move |stills| {
        if let Some(s) = stills.into_iter().next() {
            let scale = s.scale();
            deliver(&app, s.texture, scale, None, "screen");
        }
    });
}

// MARK: Delivery

fn play_sound(settings: &Settings) {
    if !settings.boolean("plays-sound", false) {
        return;
    }
    if glib::find_program_in_path("canberra-gtk-play").is_some() {
        let _ = gio::Subprocess::newv(
            &["canberra-gtk-play".as_ref(), "-i".as_ref(), "screen-capture".as_ref()],
            gio::SubprocessFlags::STDERR_SILENCE,
        );
    }
}

/// What happens after a capture, in the order the extension does it: sound, clipboard, save,
/// then the preview or the editor.
pub fn deliver(app: &adw::Application, texture: gdk::Texture, scale: f64, app_name: Option<String>, action: &str) {
    crate::debug!("deliver {action} {}x{} at {scale}", texture.width(), texture.height());
    let settings = Settings::new();
    play_sound(&settings);
    let id = glib::uuid_string_random().to_string();
    let path = captures_dir().join(format!("{id}.png"));
    let png = texture.save_to_png_bytes();
    let _ = std::fs::write(&path, &png);
    let capture = Capture {
        id,
        texture: texture.clone(),
        path,
        scale,
        app_name,
        date: glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("time")),
    };
    LAST.with(|l| *l.borrow_mut() = Some(capture.clone()));

    if action == "text" {
        copy_text_from(app, &texture);
        return;
    }
    copy_capture(&capture, &settings);
    if settings.boolean("saves-every-capture", false) {
        save_capture(&capture, &settings);
    }
    let after = settings.string("after-capture", "preview");
    if action == "annotate" || after == "editor" {
        open_editor(app, &capture);
    } else if after == "preview" {
        crate::npreview::show(app, &capture);
    }
}

/// The clipboard, at standard resolution when Settings asks for it.
pub fn copy_capture(capture: &Capture, settings: &Settings) {
    let texture = if settings.copy_at_point_size() && capture.scale > 1.0 {
        let w = (capture.texture.width() as f64 / capture.scale).round() as i32;
        let h = (capture.texture.height() as f64 / capture.scale).round() as i32;
        scaled(&capture.texture, w, h).unwrap_or_else(|| capture.texture.clone())
    } else {
        capture.texture.clone()
    };
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_texture(&texture);
    }
}

fn scaled(texture: &gdk::Texture, w: i32, h: i32) -> Option<gdk::Texture> {
    let source = crate::backdrop::texture_to_surface(texture)?;
    let target = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w.max(1), h.max(1)).ok()?;
    {
        let cr = gtk::cairo::Context::new(&target).ok()?;
        cr.scale(w as f64 / texture.width() as f64, h as f64 / texture.height() as f64);
        cr.set_source_surface(&source, 0.0, 0.0).ok()?;
        cr.source().set_filter(gtk::cairo::Filter::Good);
        cr.paint().ok()?;
    }
    crate::backdrop::surface_to_texture(target)
}

/// Saves to the save folder, or Pictures/Screenshots. Returns where it went.
pub fn save_capture(capture: &Capture, settings: &Settings) -> Option<PathBuf> {
    let format = if settings.string("save-format", "png") == "jpeg" { "jpeg" } else { "png" };
    let data = output::encode(&capture.texture, format, settings.double("jpeg-quality", 0.85))?;
    let folder = output::save_folder(settings).unwrap_or_else(|| {
        let pictures = glib::user_special_dir(glib::UserDirectory::Pictures).unwrap_or_else(|| glib::home_dir().join("Pictures"));
        pictures.join("Screenshots")
    });
    std::fs::create_dir_all(&folder).ok()?;
    let template = settings.string("filename-template", "Shotput %Y-%m-%d at %H.%M.%S");
    let name = format!("{}.{}", output::file_name(&template, &capture.date, capture.app_name.as_deref()), output::extension_for(format));
    let path = output::unique_path(&folder, &name);
    std::fs::write(&path, &data).ok()?;
    Some(path)
}

pub fn open_editor(app: &adw::Application, capture: &Capture) {
    let info = CaptureInfo {
        id: Some(capture.id.clone()),
        path: Some(capture.path.clone()),
        scale: capture.scale.max(0.25),
        monitor: None,
        app_name: capture.app_name.clone(),
        date: capture.date.clone(),
        framed: None,
    };
    Editor::open(app, capture.texture.clone(), info);
}

/// The editor wrote an annotated capture back to its file; keep "annotate last" current.
pub fn capture_edited(path: &std::path::Path) {
    LAST.with(|l| {
        let mut l = l.borrow_mut();
        if let Some(c) = l.as_mut().filter(|c| c.path == path) {
            if let Ok(t) = gdk::Texture::from_filename(path) {
                c.texture = t;
            }
        }
    });
}

fn copy_text_from(app: &adw::Application, texture: &gdk::Texture) {
    let path = output::runtime_dir().join(format!("ocr-{}.png", glib::uuid_string_random()));
    if output::write_ocr_input(texture, &path).is_none() {
        return;
    }
    if output::tesseract().is_none() {
        crate::notify(app, "Copy text needs Tesseract", "Install the tesseract package, then try again.");
        return;
    }
    let settings = Settings::new();
    let app = app.clone();
    output::recognize_text(
        &path.clone(),
        settings.boolean("ocr-keeps-line-breaks", true),
        &settings.string("ocr-language", "eng"),
        move |result| {
            let _ = std::fs::remove_file(&path);
            match result {
                Some((text, lines)) => {
                    output::copy_text(&text);
                    let body = if lines == 1 { "1 line".to_string() } else { format!("{lines} lines") };
                    crate::notify(&app, "Text copied", &body);
                }
                None => crate::notify(&app, "No text found", "Try a larger area."),
            }
        },
    );
}

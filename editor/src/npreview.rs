//! The floating preview after a capture, on desktops without the GNOME extension. A layer surface
//! in a screen corner that never takes the keyboard: click it to annotate, hover for Copy again,
//! Save and Dismiss, or drag it straight into another app. Without the layer shell (some
//! desktops), a notification says the capture was copied instead.

use crate::native::{self, Capture};
use crate::layer;
use crate::settings::Settings;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    static CURRENT: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
}

fn dismiss() {
    if let Some(w) = CURRENT.with(|c| c.borrow_mut().take()) {
        w.destroy();
    }
}

pub fn show(app: &adw::Application, capture: &Capture) {
    dismiss();
    let settings = Settings::new();
    // Not resizable would size the window to the full-size image; a default size keeps it small.
    let window = gtk::Window::builder().application(app).decorated(false).title("Shotput Preview").build();
    window.add_css_class("shotput-npreview-window");
    let Some(surface) = layer::Surface::init(&window, "shotput-preview") else {
        window.destroy();
        crate::notify(app, "Screenshot copied", "Paste it anywhere.");
        return;
    };
    let corner = settings.string("preview-corner", "automatic");
    let (left, top) = match corner.as_str() {
        "bottom-left" => (true, false),
        "top-left" => (true, true),
        "top-right" => (false, true),
        _ => (false, false),
    };
    surface
        .layer(layer::Layer::Overlay)
        .anchor(if left { layer::Edge::Left } else { layer::Edge::Right }, true)
        .anchor(if top { layer::Edge::Top } else { layer::Edge::Bottom }, true)
        .margin(if left { layer::Edge::Left } else { layer::Edge::Right }, 20)
        .margin(if top { layer::Edge::Top } else { layer::Edge::Bottom }, 20)
        .keyboard(layer::Keyboard::None);

    // Fit like the GNOME preview: at most 232 × 148, at least 208 wide for the buttons.
    let (tw, th) = (
        capture.texture.width() as f64 / capture.scale.max(1.0),
        capture.texture.height() as f64 / capture.scale.max(1.0),
    );
    let fit = (232.0 / tw.max(1.0)).min(148.0 / th.max(1.0)).min(1.0);
    let (w, h) = ((tw * fit).max(208.0).round() as i32, (th * fit).max(84.0).round() as i32);

    window.set_default_size(w, h);
    // A canvas of exactly the thumbnail's size: a Picture would ask for the image's full size.
    let picture = crate::canvas::Canvas::new();
    picture.set_size_request(w, h);
    {
        let texture = capture.texture.clone();
        picture.set_draw(move |snapshot, cw, ch| {
            let (tw, th) = (texture.width() as f64, texture.height() as f64);
            let k = (cw / tw).max(ch / th);
            let (dw, dh) = (tw * k, th * k);
            let rect = gtk::graphene::Rect::new(((cw - dw) / 2.0) as f32, ((ch - dh) / 2.0) as f32, dw as f32, dh as f32);
            snapshot.append_scaled_texture(&texture, gtk::gsk::ScalingFilter::Trilinear, &rect);
        });
    }

    let scrim = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    scrim.add_css_class("shotput-npreview-scrim");
    scrim.set_halign(gtk::Align::Fill);
    scrim.set_valign(gtk::Align::Fill);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::Center);
    buttons.set_valign(gtk::Align::Center);
    buttons.set_hexpand(true);
    scrim.append(&buttons);
    scrim.set_opacity(0.0);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&scrim);
    overlay.add_css_class("shotput-npreview");
    overlay.set_overflow(gtk::Overflow::Hidden);
    window.set_child(Some(&overlay));

    let make = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.set_tooltip_text(Some(tip));
        b.add_css_class("shotput-glass");
        b.set_focus_on_click(false);
        buttons.append(&b);
        b
    };
    let annotate = make("shotput-annotate-symbolic", "Annotate");
    let copy = make("edit-copy-symbolic", "Copy again");
    let save = make("document-save-symbolic", "Save");
    let close = make("window-close-symbolic", "Dismiss");

    let hovered = Rc::new(Cell::new(false));
    let generation = Rc::new(Cell::new(0u64));
    let schedule = {
        let generation = generation.clone();
        let hovered = hovered.clone();
        let window = window.downgrade();
        let seconds = settings.double("preview-duration", 6.0);
        Rc::new(move || {
            if seconds <= 0.0 {
                return;
            }
            let next = generation.get() + 1;
            generation.set(next);
            let generation = generation.clone();
            let hovered = hovered.clone();
            let window = window.clone();
            glib::timeout_add_local_once(std::time::Duration::from_secs_f64(seconds), move || {
                if generation.get() == next && !hovered.get() {
                    if let Some(w) = window.upgrade() {
                        if CURRENT.with(|c| c.borrow().as_ref() == Some(&w)) {
                            dismiss();
                        }
                    }
                }
            });
        })
    };

    let motion = gtk::EventControllerMotion::new();
    {
        let (hovered, scrim) = (hovered.clone(), scrim.clone());
        motion.connect_enter(move |_, _, _| {
            hovered.set(true);
            scrim.set_opacity(1.0);
        });
    }
    {
        let (hovered, scrim, schedule) = (hovered.clone(), scrim.clone(), schedule.clone());
        motion.connect_leave(move |_| {
            hovered.set(false);
            scrim.set_opacity(0.0);
            schedule();
        });
    }
    overlay.add_controller(motion);

    // Click to annotate; drag into any app.
    let click = gtk::GestureClick::new();
    {
        let app = app.clone();
        let capture = capture.clone();
        click.connect_released(move |g, _, x, y| {
            let Some(widget) = g.widget() else { return };
            if widget.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|w| w.ancestor(gtk::Button::static_type()).is_some()) {
                return;
            }
            dismiss();
            native::open_editor(&app, &capture);
        });
    }
    overlay.add_controller(click);

    let name = {
        let template = settings.string("filename-template", "Shotput %Y-%m-%d at %H.%M.%S");
        format!("{}.png", crate::output::file_name(&template, &capture.date, capture.app_name.as_deref()))
    };
    if let Ok(png) = std::fs::read(&capture.path) {
        if let Some(path) = crate::drag::stage_file(&png, &name) {
            let source = gtk::DragSource::new();
            source.set_actions(gdk::DragAction::COPY);
            source.set_content(Some(&crate::drag::content(&path, &capture.texture)));
            {
                let texture = capture.texture.clone();
                source.connect_drag_begin(move |_, drag| crate::drag::set_icon(drag, &texture));
            }
            source.connect_drag_end(|_, _, _| dismiss());
            overlay.add_controller(source);
        }
    }

    {
        let app = app.clone();
        let capture = capture.clone();
        annotate.connect_clicked(move |_| {
            dismiss();
            native::open_editor(&app, &capture);
        });
    }
    {
        let capture = capture.clone();
        copy.connect_clicked(move |b| {
            native::copy_capture(&capture, &Settings::new());
            b.set_icon_name("object-select-symbolic");
            glib::timeout_add_local_once(std::time::Duration::from_millis(600), dismiss);
        });
    }
    {
        let app = app.clone();
        let capture = capture.clone();
        save.connect_clicked(move |_| {
            dismiss();
            match native::save_capture(&capture, &Settings::new()) {
                Some(path) => {
                    let folder = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    crate::notify(&app, "Screenshot saved", &format!("Saved to {folder}"));
                }
                None => crate::notify(&app, "Could not save the screenshot", ""),
            }
        });
    }
    close.connect_clicked(|_| dismiss());

    window.present();
    {
        let window = window.downgrade();
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            if let Some(w) = window.upgrade() {
                crate::debug!("preview {}x{} (wanted {w_}x{h_})", w.width(), w.height(), w_ = w.default_width(), h_ = w.default_height());
            }
        });
    }
    CURRENT.with(|c| *c.borrow_mut() = Some(window));
    schedule();
}

pub const CSS: &str = r#"
window.shotput-npreview-window { background: none; }
.shotput-npreview { border-radius: 14px; background-color: #2b2a27; }
.shotput-npreview-scrim { background-color: alpha(#191917, 0.55); transition: opacity 180ms ease-out; }
button.shotput-glass {
  min-width: 30px; min-height: 30px; padding: 0; border-radius: 999px; color: white;
  background-color: alpha(white, 0.16); box-shadow: inset 0 0 0 1px alpha(white, 0.22);
}
button.shotput-glass:hover { background-color: alpha(white, 0.28); }
window.shotput-overlay { background: none; }
"#;

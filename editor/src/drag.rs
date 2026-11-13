//! Dragging a screenshot into another app, as a file and as an image.
//!
//! On Wayland a drag has to start from a press on the app's own window, and the preview lives in
//! GNOME Shell, so the preview hands the capture to a small window here that you drag from. The
//! editor's drag handle uses the same content.

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Writes the PNG under a readable name, so the app it lands in shows a sensible file name.
pub fn stage_file(png: &[u8], name: &str) -> Option<PathBuf> {
    let dir = crate::output::runtime_dir().join("drag");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// Offers the file first (uploads, file managers, chats) and the pixels second (image editors,
/// documents).
pub fn content(path: &Path, texture: &gdk::Texture) -> gdk::ContentProvider {
    let files = gdk::FileList::from_array(&[gio::File::for_path(path)]);
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&files.to_value()),
        gdk::ContentProvider::for_value(&texture.to_value()),
    ])
}

/// A thumbnail under the pointer while dragging.
pub fn set_icon(drag: &gdk::Drag, texture: &gdk::Texture) {
    let icon = thumbnail(texture, 180.0, 120.0);
    let (w, h) = (icon.width(), icon.height());
    let picture = gtk::Picture::for_paintable(&icon);
    picture.add_css_class("shotput-drag-icon");
    picture.set_overflow(gtk::Overflow::Hidden);
    let drag_icon = gtk::DragIcon::for_drag(drag);
    drag_icon.set_child(Some(&picture));
    drag.set_hotspot(w / 2, h / 2);
}

/// A scaled-down copy, since a drag icon shows its picture at full size.
fn thumbnail(texture: &gdk::Texture, max_w: f64, max_h: f64) -> gdk::Texture {
    let (w, h) = (texture.width().max(1), texture.height().max(1));
    let fit = (max_w / w as f64).min(max_h / h as f64).min(1.0);
    if fit >= 1.0 {
        return texture.clone();
    }
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(&bytes, gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, w, h, stride as i32);
    let (tw, th) = (((w as f64) * fit).round().max(1.0) as i32, ((h as f64) * fit).round().max(1.0) as i32);
    match pixbuf.scale_simple(tw, th, gtk::gdk_pixbuf::InterpType::Bilinear) {
        Some(small) => gdk::MemoryTexture::new(
            tw,
            th,
            gdk::MemoryFormat::R8g8b8a8,
            &small.read_pixel_bytes(),
            small.rowstride() as usize,
        )
        .upcast(),
        None => texture.clone(),
    }
}

#[derive(serde::Deserialize)]
pub struct DragRequest {
    path: String,
    name: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
}

/// A small window holding the capture, for dragging it into another app. It closes after a drop,
/// on Escape, or when you click elsewhere.
pub fn open_window(app: &adw::Application, json: &str) {
    let Ok(request) = serde_json::from_str::<DragRequest>(json) else { return };
    let Ok(png) = std::fs::read(&request.path) else { return };
    let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(png.clone())) else { return };
    let name = request.name.unwrap_or_else(|| "Shotput.png".into());
    let Some(path) = stage_file(&png, &name) else { return };

    let (tw, th) = (texture.width().max(1) as f64, texture.height().max(1) as f64);
    let (w, h) = match (request.width, request.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
        _ => {
            let fit = (232.0 / tw).min(148.0 / th).min(1.0);
            ((tw * fit).round().max(140.0) as i32, (th * fit).round().max(84.0) as i32)
        }
    };

    let window = gtk::Window::builder()
        .application(app)
        .title("Shotput Drag")
        .decorated(false)
        .resizable(false)
        .default_width(w)
        .default_height(h)
        .build();
    window.add_css_class("shotput-drag-window");

    let picture = gtk::Picture::for_paintable(&texture);
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_can_shrink(true);
    picture.set_size_request(w, h);
    let hint = gtk::Label::new(Some("Drag into any app"));
    hint.add_css_class("shotput-drag-hint");
    hint.set_valign(gtk::Align::End);
    hint.set_halign(gtk::Align::Center);
    hint.set_margin_bottom(8);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&hint);
    overlay.add_css_class("shotput-drag-card");
    overlay.set_overflow(gtk::Overflow::Hidden);
    overlay.set_cursor_from_name(Some("grab"));
    window.set_child(Some(&overlay));

    let dragging = Rc::new(Cell::new(false));
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::COPY);
    source.set_content(Some(&content(&path, &texture)));
    {
        let texture = texture.clone();
        let dragging = dragging.clone();
        let window = window.downgrade();
        source.connect_drag_begin(move |_, drag| {
            dragging.set(true);
            set_icon(drag, &texture);
            if let Some(w) = window.upgrade() {
                w.set_opacity(0.4);
            }
        });
    }
    {
        let dragging = dragging.clone();
        let window = window.downgrade();
        source.connect_drag_end(move |_, _, _| {
            dragging.set(false);
            if let Some(w) = window.upgrade() {
                w.close();
            }
        });
    }
    overlay.add_controller(source);

    let keys = gtk::EventControllerKey::new();
    {
        let window = window.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(w) = window.upgrade() {
                    w.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(keys);

    // Clicking elsewhere puts it away, but not while a drag is carrying it.
    let was_active = Rc::new(Cell::new(false));
    window.connect_is_active_notify(move |w| {
        if w.is_active() {
            was_active.set(true);
        } else if was_active.get() && !dragging.get() {
            w.close();
        }
    });
    window.present();
}

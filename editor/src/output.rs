//! Clipboard, files, and text recognition. The clipboard goes through the Shotput Shell
//! extension when it is running, so gnome-shell owns the image and it survives the editor.

use crate::settings::Settings;
use gtk::prelude::*;
use gtk::{cairo, gdk, gdk_pixbuf, gio, glib};
use std::path::{Path, PathBuf};

pub const SHELL_BUS: &str = "org.gnome.Shell";
pub const SHELL_PATH: &str = "/io/github/shotput/Shell";
pub const SHELL_IFACE: &str = "io.github.shotput.Shell";

/// Calls a method on the extension. Returns false when it is not there.
pub fn call_shell(method: &str, args: &glib::Variant) -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return false };
    bus.call_sync(
        Some(SHELL_BUS),
        SHELL_PATH,
        SHELL_IFACE,
        method,
        Some(args),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        3000,
        gio::Cancellable::NONE,
    )
    .is_ok()
}

pub fn runtime_dir() -> PathBuf {
    let dir = glib::user_runtime_dir().join("shotput");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Puts a PNG on the clipboard. Through the shell when possible; otherwise this process
/// serves it, and stays running so it remains pasteable.
pub fn copy_png(png: &glib::Bytes, texture: &gdk::Texture) {
    let path = runtime_dir().join("clipboard.png");
    if std::fs::write(&path, png).is_ok() && call_shell("SetClipboardImage", &(path.to_string_lossy().to_string(),).to_variant()) {
        return;
    }
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_texture(texture);
    }
}

pub fn copy_text(text: &str) {
    if call_shell("SetClipboardText", &(text.to_string(),).to_variant()) {
        return;
    }
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(text);
    }
}

// MARK: Encoding

pub fn encode(texture: &gdk::Texture, format: &str, quality: f64) -> Option<glib::Bytes> {
    match format {
        "jpeg" => encode_jpeg(texture, quality),
        _ => Some(texture.save_to_png_bytes()),
    }
}

/// JPEG has no alpha. Flatten onto white so transparent window corners do not turn black.
fn encode_jpeg(texture: &gdk::Texture, quality: f64) -> Option<glib::Bytes> {
    let source = crate::backdrop::texture_to_surface(texture)?;
    let (w, h) = (texture.width(), texture.height());
    let mut flat = cairo::ImageSurface::create(cairo::Format::Rgb24, w, h).ok()?;
    {
        let cr = cairo::Context::new(&flat).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        cr.set_source_surface(&source, 0.0, 0.0).ok()?;
        cr.paint().ok()?;
    }
    flat.flush();
    let stride = flat.stride() as usize;
    let data = flat.data().ok()?;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = y * stride + x * 4;
            if cfg!(target_endian = "little") {
                rgb.extend_from_slice(&[data[i + 2], data[i + 1], data[i]]);
            } else {
                rgb.extend_from_slice(&[data[i + 1], data[i + 2], data[i + 3]]);
            }
        }
    }
    let pixbuf = gdk_pixbuf::Pixbuf::from_bytes(&glib::Bytes::from_owned(rgb), gdk_pixbuf::Colorspace::Rgb, false, 8, w, h, w * 3);
    let q = ((quality * 100.0).round() as i32).clamp(10, 100).to_string();
    pixbuf.save_to_bufferv("jpeg", &[("quality", q.as_str())]).ok().map(glib::Bytes::from_owned)
}

// MARK: File names

/// Expands a template such as `Shotput %Y-%m-%d` into a file name without an extension.
pub fn file_name(template: &str, date: &glib::DateTime, app_name: Option<&str>) -> String {
    let fmt = |pattern: &str| date.format(pattern).map(|s| s.to_string()).unwrap_or_default();
    let mut result = String::new();
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        let Some(token) = chars.next() else {
            result.push('%');
            break;
        };
        match token {
            'Y' | 'y' | 'm' | 'b' | 'B' | 'd' | 'j' | 'A' | 'a' | 'H' | 'I' | 'p' | 'M' | 'S' => {
                result += &fmt(&format!("%{token}"))
            }
            'n' => result += app_name.unwrap_or("Screen"),
            'r' => {
                const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
                for _ in 0..4 {
                    result.push(ALPHABET[glib::random_int_range(0, ALPHABET.len() as i32) as usize] as char);
                }
            }
            '%' => result.push('%'),
            other => {
                result.push('%');
                result.push(other);
            }
        }
    }
    let cleaned = result.replace('/', "-").replace(':', ".").trim().to_string();
    if cleaned.is_empty() || cleaned.starts_with('.') {
        return "Shotput".into();
    }
    cleaned
}

pub fn extension_for(format: &str) -> &'static str {
    if format == "jpeg" { "jpg" } else { "png" }
}

pub fn unique_path(folder: &Path, name: &str) -> PathBuf {
    let path = folder.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    (2..).map(|n| folder.join(format!("{stem} {n}{ext}"))).find(|p| !p.exists()).unwrap_or(path)
}

pub fn save_folder(settings: &Settings) -> Option<PathBuf> {
    let folder = settings.string("save-folder", "");
    let folder = folder.trim();
    if folder.is_empty() {
        return None;
    }
    let path = PathBuf::from(folder.replace('~', &glib::home_dir().to_string_lossy()));
    path.is_dir().then_some(path)
}

// MARK: Text recognition

/// Screen text is small for Tesseract; doubling it reads far more reliably.
pub fn write_ocr_input(texture: &gdk::Texture, path: &Path) -> Option<()> {
    let (w, h) = (texture.width(), texture.height());
    if (w as i64) * (h as i64) >= 8_000_000 {
        return std::fs::write(path, texture.save_to_png_bytes()).ok();
    }
    let source = crate::backdrop::texture_to_surface(texture)?;
    let scaled = cairo::ImageSurface::create(cairo::Format::ARgb32, w * 2, h * 2).ok()?;
    {
        let cr = cairo::Context::new(&scaled).ok()?;
        cr.scale(2.0, 2.0);
        cr.set_source_surface(&source, 0.0, 0.0).ok()?;
        cr.source().set_filter(cairo::Filter::Bilinear);
        cr.paint().ok()?;
    }
    crate::backdrop::surface_to_texture(scaled)?.save_to_png(path).ok()
}

pub fn tesseract() -> Option<PathBuf> {
    let bundled = glib::user_data_dir().join("shotput/bin/tesseract");
    if bundled.exists() {
        return Some(bundled);
    }
    glib::find_program_in_path("tesseract")
}

/// Reads the text in a PNG on this machine with Tesseract. Calls back with the text and its
/// line count, or None when there is none.
pub fn recognize_text(png: &Path, keeps_line_breaks: bool, language: &str, done: impl FnOnce(Option<(String, usize)>) + 'static) {
    let Some(bin) = tesseract() else {
        done(None);
        return;
    };
    let args = [bin.as_os_str(), png.as_os_str(), "stdout".as_ref(), "-l".as_ref(), language.as_ref()];
    let Ok(process) = gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE) else {
        done(None);
        return;
    };
    process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| {
        let text = result.ok().and_then(|(out, _)| out).map(|s| s.to_string()).unwrap_or_default();
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if lines.is_empty() {
            done(None);
        } else {
            done(Some((lines.join(if keeps_line_breaks { "\n" } else { " " }), lines.len())));
        }
    });
}

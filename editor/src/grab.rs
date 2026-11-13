//! A still of every monitor, for the native overlay: KWin's screenshot service on KDE Plasma,
//! grim on wlroots desktops (Hyprland, Sway), and the screenshot portal anywhere else.

use crate::desktop::{self, Desktop};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::io::Read;

pub struct Still {
    pub monitor: gdk::Monitor,
    /// Where the monitor sits, in logical (layout) pixels.
    pub geometry: gdk::Rectangle,
    /// The monitor's pixels at full resolution.
    pub texture: gdk::Texture,
}

impl Still {
    /// Pixels per logical pixel, horizontally.
    pub fn scale(&self) -> f64 {
        self.texture.width() as f64 / self.geometry.width().max(1) as f64
    }
}

pub fn monitors() -> Vec<gdk::Monitor> {
    let Some(display) = gdk::Display::default() else { return Vec::new() };
    let list = display.monitors();
    (0..list.n_items()).filter_map(|i| list.item(i).and_downcast::<gdk::Monitor>()).collect()
}

/// Grabs every monitor, then calls back on the main thread.
pub fn grab(done: impl FnOnce(Result<Vec<Still>, String>) + 'static) {
    let monitors = monitors();
    if monitors.is_empty() {
        done(Err("No monitors".into()));
        return;
    }
    match desktop::current() {
        Desktop::Kde => {
            glib::MainContext::default().spawn_local(async move {
            let mut stills = Vec::new();
            for monitor in &monitors {
                match kwin_screen(monitor).await {
                    Ok(texture) => stills.push(Still { monitor: monitor.clone(), geometry: monitor.geometry(), texture }),
                    Err(err) => {
                        // KWin refuses apps it has not been told about; the portal still works.
                        eprintln!("shotput: KWin screenshot failed ({err}), using the portal");
                        portal(monitors, done);
                        return;
                    }
                }
            }
            done(Ok(stills));
            });
        }
        Desktop::Hyprland | Desktop::Sway if glib::find_program_in_path("grim").is_some() => {
            glib::MainContext::default().spawn_local(async move {
                let mut stills = Vec::new();
                for monitor in &monitors {
                    match grim(monitor).await {
                        Ok(texture) => stills.push(Still { monitor: monitor.clone(), geometry: monitor.geometry(), texture }),
                        Err(err) => {
                            done(Err(err));
                            return;
                        }
                    }
                }
                done(Ok(stills));
            });
        }
        _ => portal(monitors, done),
    }
}

// MARK: KDE Plasma

/// QImage formats KWin sends, as GDK memory formats (little-endian).
fn qimage_format(format: u32) -> Option<gdk::MemoryFormat> {
    Some(match format {
        4 => gdk::MemoryFormat::B8g8r8x8,
        5 => gdk::MemoryFormat::B8g8r8a8,
        6 => gdk::MemoryFormat::B8g8r8a8Premultiplied,
        16 => gdk::MemoryFormat::R8g8b8x8,
        17 => gdk::MemoryFormat::R8g8b8a8,
        18 => gdk::MemoryFormat::R8g8b8a8Premultiplied,
        13 => gdk::MemoryFormat::R8g8b8,
        _ => return None,
    })
}

/// org.kde.KWin.ScreenShot2.CaptureScreen. KWin lets apps whose desktop file lists the interface
/// in X-KDE-DBUS-Restricted-Interfaces call it; the installer's desktop file does.
async fn kwin_screen(monitor: &gdk::Monitor) -> Result<gdk::Texture, String> {
    let name = monitor.connector().ok_or("monitor has no connector")?.to_string();
    let bus = gio::bus_get_future(gio::BusType::Session).await.map_err(|e| e.to_string())?;
    let (reader, writer) = std::io::pipe().map_err(|e| e.to_string())?;
    let fds = gio::UnixFDList::new();
    fds.append(&writer).map_err(|e| e.to_string())?;
    drop(writer);

    let options = glib::VariantDict::new(None);
    options.insert("native-resolution", true);
    options.insert("include-cursor", false);
    let args = (name, options, glib::variant::Handle(0)).to_variant();

    let read = gio::spawn_blocking(move || {
        let mut reader = reader;
        let mut data = Vec::new();
        reader.read_to_end(&mut data).map(|_| data)
    });
    let (reply, _) = bus
        .call_with_unix_fd_list_future(
            Some("org.kde.KWin"),
            "/org/kde/KWin/ScreenShot2",
            "org.kde.KWin.ScreenShot2",
            "CaptureScreen",
            Some(&args),
            Some(glib::VariantTy::new("(a{sv})").unwrap()),
            gio::DBusCallFlags::NONE,
            5000,
            Some(&fds),
        )
        .await
        .map_err(|e| e.to_string())?;
    drop(fds);
    let data = read.await.map_err(|_| "reading failed")?.map_err(|e| e.to_string())?;

    let meta = glib::VariantDict::new(Some(&reply.child_value(0)));
    let get = |k: &str| meta.lookup::<u32>(k).ok().flatten();
    let (w, h, stride, format) = (
        get("width").ok_or("no width")?,
        get("height").ok_or("no height")?,
        get("stride").ok_or("no stride")?,
        get("format").ok_or("no format")?,
    );
    let format = qimage_format(format).ok_or(format!("unknown image format {format}"))?;
    if data.len() < (stride * h) as usize {
        return Err("short image".into());
    }
    Ok(gdk::MemoryTexture::new(w as i32, h as i32, format, &glib::Bytes::from_owned(data), stride as usize).upcast())
}

// MARK: wlroots

async fn grim(monitor: &gdk::Monitor) -> Result<gdk::Texture, String> {
    let name = monitor.connector().ok_or("monitor has no connector")?.to_string();
    let process = gio::Subprocess::newv(
        &["grim".as_ref(), "-o".as_ref(), name.as_str().as_ref(), "-t".as_ref(), "ppm".as_ref(), "-".as_ref()],
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    )
    .map_err(|e| e.to_string())?;
    let (out, _) = process.communicate_future(None).await.map_err(|e| e.to_string())?;
    let out = out.ok_or("grim gave nothing")?;
    ppm_texture(&out).ok_or_else(|| "could not read grim's image".into())
}

/// A binary PPM (P6), which grim writes fastest.
fn ppm_texture(data: &[u8]) -> Option<gdk::Texture> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 && i < data.len() {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        if data.get(i) == Some(&b'#') {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&data[start..i]).ok()?.to_string());
    }
    if fields.first()? != "P6" || fields.get(3)? != "255" {
        return None;
    }
    let (w, h): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let pixels = data.get(i + 1..i + 1 + w * h * 3)?;
    Some(gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8, &glib::Bytes::from(pixels), w * 3).upcast())
}

// MARK: Portal

/// A non-interactive full-screen portal screenshot, cut up per monitor.
fn portal(monitors: Vec<gdk::Monitor>, done: impl FnOnce(Result<Vec<Still>, String>) + 'static) {
    crate::portal_screenshot(false, move |path| {
        let Some(path) = path else {
            done(Err("The screenshot was not allowed".into()));
            return;
        };
        let texture = match gdk::Texture::from_filename(&path) {
            Ok(t) => t,
            Err(e) => {
                done(Err(e.to_string()));
                return;
            }
        };
        let _ = std::fs::remove_file(&path);
        // The portal's image spans the whole layout; find its scale from the layout's width.
        let right = monitors.iter().map(|m| m.geometry().x() + m.geometry().width()).max().unwrap_or(1);
        let left = monitors.iter().map(|m| m.geometry().x()).min().unwrap_or(0);
        let top = monitors.iter().map(|m| m.geometry().y()).min().unwrap_or(0);
        let scale = texture.width() as f64 / (right - left).max(1) as f64;
        let stills = monitors
            .into_iter()
            .filter_map(|monitor| {
                let g = monitor.geometry();
                let rect = crate::model::R::new(
                    ((g.x() - left) as f64 * scale).round(),
                    ((g.y() - top) as f64 * scale).round(),
                    (g.width() as f64 * scale).round(),
                    (g.height() as f64 * scale).round(),
                );
                let texture = crop(&texture, rect)?;
                Some(Still { monitor, geometry: g, texture })
            })
            .collect();
        done(Ok(stills));
    });
}

/// Cuts a rectangle (in texture pixels) out of a texture.
pub fn crop(texture: &gdk::Texture, rect: crate::model::R) -> Option<gdk::Texture> {
    let rect = rect.intersection(&crate::model::R::new(0.0, 0.0, texture.width() as f64, texture.height() as f64))?;
    let (x, y, w, h) = (rect.x.round() as usize, rect.y.round() as usize, rect.w.round() as usize, rect.h.round() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8Premultiplied);
    let (bytes, stride) = downloader.download_bytes();
    let mut out = Vec::with_capacity(w * h * 4);
    for row in y..(y + h).min(texture.height() as usize) {
        let start = row * stride + x * 4;
        out.extend_from_slice(bytes.get(start..start + w * 4)?);
    }
    let h = out.len() / (w * 4);
    Some(gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8a8Premultiplied, &glib::Bytes::from_owned(out), w * 4).upcast())
}

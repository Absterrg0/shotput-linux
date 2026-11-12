//! Puts a screenshot on a backdrop: the desktop wallpaper or a colour, with padding and a soft
//! shadow. Ported from Shotput's Backdrop.swift. Runs on the CPU with cairo and a separable blur,
//! so the editor and the `compose` command (used for window captures) share it without a display.

use crate::model::Rgba;
use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fill {
    Wallpaper,
    Color(Rgba),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub fill: Fill,
    /// Points of backdrop around the image.
    pub padding: f64,
}

/// Downloads a texture into a cairo image surface (premultiplied, native byte order).
pub fn texture_to_surface(texture: &gdk::Texture) -> Option<cairo::ImageSurface> {
    texture_to_surface_with_corner(texture).map(|(s, _)| s)
}

/// Also returns the alpha of the top-left pixel.
fn texture_to_surface_with_corner(texture: &gdk::Texture) -> Option<(cairo::ImageSurface, u8)> {
    let (w, h) = (texture.width(), texture.height());
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
    let stride = surface.stride() as usize;
    let corner;
    {
        let mut data = surface.data().ok()?;
        let mut downloader = gdk::TextureDownloader::new(texture);
        // Cairo's ARGB32 is B, G, R, A in memory on little-endian machines.
        downloader.set_format(if cfg!(target_endian = "little") {
            gdk::MemoryFormat::B8g8r8a8Premultiplied
        } else {
            gdk::MemoryFormat::A8r8g8b8Premultiplied
        });
        let (bytes, src_stride) = downloader.download_bytes();
        let row = (w as usize) * 4;
        for y in 0..h as usize {
            data[y * stride..y * stride + row].copy_from_slice(&bytes[y * src_stride..y * src_stride + row]);
        }
        corner = if cfg!(target_endian = "little") { bytes[3] } else { bytes[0] };
    }
    Some((surface, corner))
}

pub fn surface_to_texture(mut surface: cairo::ImageSurface) -> Option<gdk::Texture> {
    surface.flush();
    let (w, h, stride) = (surface.width(), surface.height(), surface.stride() as usize);
    let bytes = glib::Bytes::from(&*surface.data().ok()?);
    let format = if cfg!(target_endian = "little") {
        gdk::MemoryFormat::B8g8r8a8Premultiplied
    } else {
        gdk::MemoryFormat::A8r8g8b8Premultiplied
    };
    Some(gdk::MemoryTexture::new(w, h, format, &bytes, stride).upcast())
}

/// - `wallpaper`: Required for `Fill::Wallpaper`; without it the fill falls back to a neutral colour.
/// - `adds_shadow`: False when the image already carries the window's own shadow.
pub fn compose(
    image: &gdk::Texture,
    scale: f64,
    style: Style,
    wallpaper: Option<&gdk::Texture>,
    adds_shadow: bool,
) -> Option<gdk::Texture> {
    let (source, corner_alpha) = texture_to_surface_with_corner(image)?;
    let padding = (style.padding * scale).round() as i32;
    let (iw, ih) = (image.width(), image.height());
    let (width, height) = (iw + padding * 2, ih + padding * 2);
    let canvas = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    let cr = cairo::Context::new(&canvas).ok()?;

    match (style.fill, wallpaper) {
        (Fill::Wallpaper, Some(wallpaper)) => {
            if let Some(wp) = texture_to_surface(wallpaper) {
                let (ww, wh) = (wp.width() as f64, wp.height() as f64);
                let fill = (width as f64 / ww).max(height as f64 / wh);
                cr.save().ok();
                cr.translate((width as f64 - ww * fill) / 2.0, (height as f64 - wh * fill) / 2.0);
                cr.scale(fill, fill);
                cr.set_source_surface(&wp, 0.0, 0.0).ok();
                cr.source().set_filter(cairo::Filter::Good);
                cr.paint().ok();
                cr.restore().ok();
            }
        }
        (Fill::Wallpaper, None) => {
            let c = Rgba::hex(0xE9E6DF, 1.0);
            cr.set_source_rgb(c.0, c.1, c.2);
            cr.paint().ok();
        }
        (Fill::Color(c), _) => {
            cr.set_source_rgb(c.0, c.1, c.2);
            cr.paint().ok();
        }
    }

    // Area captures are square-cornered and opaque; round them so they sit on the backdrop like a
    // window does. Window captures already have their own transparent corners.
    let rounds = corner_alpha > 200;
    let radius = 10.0 * scale;
    let (fx, fy) = (padding as f64, padding as f64);
    let shape = |cr: &cairo::Context| {
        if rounds {
            crate::render::rounded_rect(cr, crate::model::R::new(fx, fy, iw as f64, ih as f64), radius);
        } else {
            cr.rectangle(fx, fy, iw as f64, ih as f64);
        }
    };

    if adds_shadow && padding > 0 {
        // The shadow's shape: the rounded frame, or the image's own alpha for windows. It is
        // soft, so it is blurred at a fraction of the size and scaled back up, which keeps a
        // padding slider fluid on large captures.
        let sigma = 36.0 * scale / 2.0;
        let factor = (sigma / 4.0).clamp(1.0, 8.0);
        let (mw, mh) = (((width as f64) / factor).ceil() as i32, ((height as f64) / factor).ceil() as i32);
        let mut mask = cairo::ImageSurface::create(cairo::Format::A8, mw, mh).ok()?;
        {
            let mc = cairo::Context::new(&mask).ok()?;
            mc.scale(1.0 / factor, 1.0 / factor);
            if rounds {
                shape(&mc);
                mc.fill().ok();
            } else {
                mc.set_source_surface(&source, fx, fy).ok();
                mc.paint().ok();
            }
        }
        mask.flush();
        {
            let stride = mask.stride() as usize;
            let mut data = mask.data().ok()?;
            gaussian_blur_a8(&mut data, mw as usize, mh as usize, stride, sigma / factor);
        }
        mask.mark_dirty();
        cr.save().ok();
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.32);
        cr.translate(0.0, 10.0 * scale);
        cr.scale(factor, factor);
        let pattern = cairo::SurfacePattern::create(&mask);
        pattern.set_filter(cairo::Filter::Bilinear);
        cr.mask(&pattern).ok();
        cr.restore().ok();
    }

    cr.save().ok();
    if rounds {
        shape(&cr);
        cr.clip();
    }
    cr.set_source_surface(&source, fx, fy).ok();
    cr.paint().ok();
    cr.restore().ok();
    drop(cr);
    surface_to_texture(canvas)
}

/// Three box blurs approximate a gaussian with the given standard deviation.
pub fn gaussian_blur_a8(data: &mut [u8], width: usize, height: usize, stride: usize, sigma: f64) {
    if sigma < 0.5 || width == 0 || height == 0 {
        return;
    }
    let boxes = box_sizes(sigma, 3);
    let mut buf = vec![0u8; width.max(height)];
    let mut line = vec![0u8; width.max(height)];
    for radius in boxes {
        // Horizontal pass.
        for y in 0..height {
            let row = &mut data[y * stride..y * stride + width];
            line[..width].copy_from_slice(row);
            box_line(&line[..width], &mut buf[..width], radius);
            row.copy_from_slice(&buf[..width]);
        }
        // Vertical pass.
        for x in 0..width {
            for y in 0..height {
                line[y] = data[y * stride + x];
            }
            box_line(&line[..height], &mut buf[..height], radius);
            for y in 0..height {
                data[y * stride + x] = buf[y];
            }
        }
    }
}

fn box_sizes(sigma: f64, n: usize) -> Vec<usize> {
    let ideal = ((12.0 * sigma * sigma / n as f64) + 1.0).sqrt();
    let mut wl = ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma - (n as f64) * (wl * wl) as f64 - 4.0 * n as f64 * wl as f64 - 3.0 * n as f64)
        / (-4.0 * wl as f64 - 4.0);
    let m = m_ideal.round() as i64;
    (0..n as i64).map(|i| (((if i < m { wl } else { wu }) - 1) / 2).max(0) as usize).collect()
}

fn box_line(src: &[u8], dst: &mut [u8], r: usize) {
    let n = src.len();
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let div = (2 * r + 1) as u32;
    let mut acc: u32 = 0;
    // Edges count as transparent.
    for i in 0..=r.min(n - 1) {
        acc += src[i] as u32;
    }
    for i in 0..n {
        dst[i] = (acc / div) as u8;
        let add = i + r + 1;
        if add < n {
            acc += src[add] as u32;
        }
        if i >= r {
            acc -= src[i - r] as u32;
        }
    }
}

// MARK: Wallpaper

/// The desktop wallpaper for the current light or dark style, as a texture.
pub fn wallpaper() -> Option<gdk::Texture> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup("org.gnome.desktop.background", true)?;
    let background = gio::Settings::new("org.gnome.desktop.background");
    let dark = source.lookup("org.gnome.desktop.interface", true).is_some()
        && gio::Settings::new("org.gnome.desktop.interface").string("color-scheme") == "prefer-dark";
    let uri = if dark { background.string("picture-uri-dark") } else { background.string("picture-uri") };
    let uri = if uri.is_empty() { background.string("picture-uri") } else { uri };
    let mut path = gio::File::for_uri(&uri).path()?;
    if path.extension().is_some_and(|e| e == "xml") {
        // Time-of-day wallpapers: use the first picture they list.
        let xml = std::fs::read_to_string(&path).ok()?;
        let start = xml.find("<file>")? + 6;
        let end = start + xml[start..].find("</file>")?;
        path = xml[start..end].trim().into();
    }
    gdk::Texture::from_filename(&path).ok()
}

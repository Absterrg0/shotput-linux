//! Draws annotations into a GTK snapshot, ported from Shotput's AnnotationRenderer.swift.
//! The editor canvas and the exporter share this, so what you see is what you copy: the canvas
//! renders it on screen, and export renders the same nodes on the GPU at full resolution.

use crate::model::*;
use gtk::pango::prelude::FontMapExt;
use gtk::prelude::*;
use gtk::{cairo, gdk, glib, graphene, gsk};
use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::rc::Rc;

/// The pixels being annotated, with a stable identity for caches.
#[derive(Clone)]
pub struct BaseImage {
    pub id: u64,
    pub texture: gdk::Texture,
    pixels: Rc<RefCell<Option<Rc<(glib::Bytes, usize)>>>>,
}

impl BaseImage {
    pub fn new(texture: gdk::Texture) -> BaseImage {
        BaseImage { id: new_id(), texture, pixels: Rc::default() }
    }
    pub fn width(&self) -> i32 {
        self.texture.width()
    }
    pub fn height(&self) -> i32 {
        self.texture.height()
    }
    /// Premultiplied RGBA bytes and stride, downloaded once.
    pub fn pixels(&self) -> Rc<(glib::Bytes, usize)> {
        if let Some(p) = self.pixels.borrow().as_ref() {
            return p.clone();
        }
        let mut downloader = gdk::TextureDownloader::new(&self.texture);
        downloader.set_format(gdk::MemoryFormat::R8g8b8a8Premultiplied);
        let downloaded = Rc::new(downloader.download_bytes());
        *self.pixels.borrow_mut() = Some(downloaded.clone());
        downloaded
    }
}

/// Maps image points onto the snapshot: `origin` is where point (0, 0) lands and `zoom` is
/// snapshot units per point.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub ox: f64,
    pub oy: f64,
    pub zoom: f64,
}

impl View {
    pub fn to_view(&self, q: P) -> P {
        p(self.ox + q.x * self.zoom, self.oy + q.y * self.zoom)
    }
    pub fn to_image(&self, q: P) -> P {
        p((q.x - self.ox) / self.zoom, (q.y - self.oy) / self.zoom)
    }
    pub fn rect(&self, r: R) -> R {
        R::new(self.ox + r.x * self.zoom, self.oy + r.y * self.zoom, r.w * self.zoom, r.h * self.zoom)
    }
}

pub fn grect(r: R) -> graphene::Rect {
    graphene::Rect::new(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
}

const HALO_WIDTH: f64 = 1.25;

#[derive(Default)]
pub struct Renderer {
    mosaics: RefCell<HashMap<u64, (R, u64, gdk::Texture)>>,
}

impl Renderer {
    /// Appends the marks. `scale` is base image pixels per point. `visible` limits the surfaces
    /// built for each mark to what can be seen, in snapshot units.
    pub fn draw(
        &self,
        snapshot: &gtk::Snapshot,
        annotations: &[Annotation],
        base: &BaseImage,
        scale: f64,
        view: View,
        visible: R,
    ) {
        for a in annotations {
            self.draw_one(snapshot, a, base, scale, view, visible);
        }
    }

    fn draw_one(&self, snapshot: &gtk::Snapshot, a: &Annotation, base: &BaseImage, scale: f64, view: View, visible: R) {
        let w = a.weight.stroke();
        let margin = match a.kind {
            Kind::Arrow(..) => (w * 4.4).max(13.0) * 0.6 + w + 8.0,
            Kind::Text(..) | Kind::Counter(..) => 8.0,
            _ => w + 8.0,
        };
        // Room for the soft shadow below each mark.
        let bounds_pts = a.frame().inset(-margin, -margin);
        let Some(bounds) = view.rect(bounds_pts).inset(-6.0 * view.zoom, -6.0 * view.zoom).intersection(&visible) else {
            return;
        };
        let zoom = view.zoom;

        match &a.kind {
            Kind::Highlight(rect) => {
                let cr = cairo_in(snapshot, bounds, view);
                let radius = 3f64.min(rect.w.min(rect.h) / 2.0);
                rounded_rect(&cr, *rect, radius);
                set(&cr, a.color.rgba().with_alpha(0.38));
                let _ = cr.fill();
            }
            Kind::Redact(rect) => self.draw_redaction(snapshot, *rect, a, base, scale, view, visible),
            Kind::Counter(center, number) => {
                let radius = a.weight.counter_radius();
                shadowed(snapshot, zoom, || {
                    let cr = cairo_in(snapshot, bounds, view);
                    set(&cr, a.color.halo());
                    cr.arc(center.x, center.y, radius + HALO_WIDTH + 0.5, 0.0, 2.0 * PI);
                    let _ = cr.fill();
                    set(&cr, a.color.rgba());
                    cr.arc(center.x, center.y, radius, 0.0, 2.0 * PI);
                    let _ = cr.fill();
                });
                let cr = cairo_in(snapshot, bounds, view);
                let layout = counter_layout(*number, radius);
                let (_, logical) = layout.extents();
                let s = gtk::pango::SCALE as f64;
                let (lw, lh) = (logical.width() as f64 / s, logical.height() as f64 / s);
                cr.move_to(center.x - lw / 2.0 - logical.x() as f64 / s, center.y - lh / 2.0);
                set(&cr, a.color.on_ink());
                pangocairo::functions::show_layout(&cr, &layout);
            }
            Kind::Text(origin, string) => self.draw_text(snapshot, bounds, view, *origin, string, a),
            _ => {
                shadowed(snapshot, zoom, || {
                    let cr = cairo_in(snapshot, bounds, view);
                    cr.set_line_cap(cairo::LineCap::Round);
                    cr.set_line_join(cairo::LineJoin::Round);
                    match &a.kind {
                        Kind::Arrow(from, to) => match a.bend {
                            Some(c) => curved_arrow(&cr, *from, c, *to, a),
                            None => arrow(&cr, *from, *to, a),
                        },
                        Kind::Line(from, to) => {
                            cr.move_to(from.x, from.y);
                            match a.bend {
                                Some(c) => quad_to(&cr, *from, c, *to),
                                None => cr.line_to(to.x, to.y),
                            }
                            stroke(&cr, a);
                        }
                        Kind::Rectangle(rect) => {
                            let radius = (w + 2.0).min(rect.w.min(rect.h) / 2.0);
                            rounded_rect(&cr, *rect, radius);
                            stroke(&cr, a);
                        }
                        Kind::Ellipse(rect) => {
                            if rect.w > 0.0 && rect.h > 0.0 {
                                cr.save().ok();
                                cr.translate(rect.mid_x(), rect.mid_y());
                                cr.scale(rect.w / 2.0, rect.h / 2.0);
                                cr.arc(0.0, 0.0, 1.0, 0.0, 2.0 * PI);
                                cr.restore().ok();
                                stroke(&cr, a);
                            }
                        }
                        Kind::Pen(points) => {
                            smooth_path(&cr, points);
                            stroke(&cr, a);
                        }
                        _ => {}
                    }
                });
            }
        }
    }

    fn draw_text(&self, snapshot: &gtk::Snapshot, bounds: R, view: View, origin: P, string: &str, a: &Annotation) {
        let frame = text_metrics::frame(origin, string, a.weight);
        let (px, py) = text_metrics::padding(a.weight);
        let layout = text_metrics::layout(string, a.weight);
        let text_origin = p(frame.x + px, frame.y + py);
        if a.filled {
            let radius = (frame.h / 2.0).min(a.weight.font_size() * 0.45);
            shadowed(snapshot, view.zoom, || {
                let cr = cairo_in(snapshot, bounds, view);
                rounded_rect(&cr, frame, radius);
                set(&cr, a.color.halo());
                let _ = cr.fill_preserve();
                cr.set_line_width(HALO_WIDTH * 2.0);
                let _ = cr.stroke();
                rounded_rect(&cr, frame, radius);
                set(&cr, a.color.rgba());
                let _ = cr.fill();
            });
            let cr = cairo_in(snapshot, bounds, view);
            cr.move_to(text_origin.x, text_origin.y);
            set(&cr, a.color.on_ink());
            pangocairo::functions::show_layout(&cr, &layout);
        } else {
            // Outline pass first, then the fill, so the halo sits behind the glyphs.
            shadowed(snapshot, view.zoom, || {
                let cr = cairo_in(snapshot, bounds, view);
                cr.set_line_join(cairo::LineJoin::Round);
                cr.move_to(text_origin.x, text_origin.y);
                pangocairo::functions::layout_path(&cr, &layout);
                set(&cr, a.color.halo());
                cr.set_line_width(HALO_WIDTH * 2.0 + 1.0);
                let _ = cr.stroke_preserve();
                let _ = cr.fill();
                cr.move_to(text_origin.x, text_origin.y);
                set(&cr, a.color.rgba());
                pangocairo::functions::show_layout(&cr, &layout);
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_redaction(
        &self,
        snapshot: &gtk::Snapshot,
        rect: R,
        a: &Annotation,
        base: &BaseImage,
        scale: f64,
        view: View,
        visible: R,
    ) {
        let image = R::new(0.0, 0.0, base.width() as f64 / scale, base.height() as f64 / scale);
        let Some(area) = rect.intersection(&image) else { return };
        if area.w < 1.0 || area.h < 1.0 {
            return;
        }
        let Some(bounds) = view.rect(area).inset(-1.0, -1.0).intersection(&visible) else { return };
        if a.filled {
            let cr = cairo_in(snapshot, bounds, view);
            rounded_rect(&cr, area, 2f64.min(area.w.min(area.h) / 2.0));
            set(&cr, a.color.rgba());
            let _ = cr.fill();
            return;
        }
        let Some(mosaic) = self.mosaic(area, a.id, base, scale) else { return };
        snapshot.push_clip(&grect(bounds));
        snapshot.append_scaled_texture(&mosaic, gsk::ScalingFilter::Nearest, &grect(view.rect(area)));
        snapshot.pop();
    }

    /// The region averaged down to a handful of cells. Each cell is the mean of what it covers,
    /// so text turns into grey blocks instead of vanishing, and the cells are too coarse to read
    /// back. Drawn scaled up without smoothing, it gives the familiar mosaic.
    fn mosaic(&self, area: R, id: u64, base: &BaseImage, scale: f64) -> Option<gdk::Texture> {
        if let Some((rect, base_id, tex)) = self.mosaics.borrow().get(&id) {
            if *rect == area && *base_id == base.id {
                return Some(tex.clone());
            }
        }
        let px = R::new(area.x * scale, area.y * scale, area.w * scale, area.h * scale).integral();
        let (bw, bh) = (base.width() as f64, base.height() as f64);
        let px = px.intersection(&R::new(0.0, 0.0, bw, bh))?;
        let cell = 10f64.max(area.w.min(area.h) / 5.0) * scale;
        let columns = ((px.w / cell).ceil() as usize).max(1);
        let rows = ((px.h / cell).ceil() as usize).max(1);
        let pixels = base.pixels();
        let (data, stride) = (&pixels.0, pixels.1);
        let mut out = vec![0u8; columns * rows * 4];
        for row in 0..rows {
            let y0 = (px.y + px.h * row as f64 / rows as f64) as usize;
            let y1 = ((px.y + px.h * (row + 1) as f64 / rows as f64) as usize).max(y0 + 1).min(bh as usize);
            for col in 0..columns {
                let x0 = (px.x + px.w * col as f64 / columns as f64) as usize;
                let x1 = ((px.x + px.w * (col + 1) as f64 / columns as f64) as usize).max(x0 + 1).min(bw as usize);
                let mut sum = [0u64; 4];
                let mut n = 0u64;
                for y in y0..y1 {
                    let line = &data[y * stride..];
                    for x in x0..x1 {
                        let i = x * 4;
                        sum[0] += line[i] as u64;
                        sum[1] += line[i + 1] as u64;
                        sum[2] += line[i + 2] as u64;
                        sum[3] += line[i + 3] as u64;
                        n += 1;
                    }
                }
                let o = (row * columns + col) * 4;
                for c in 0..4 {
                    out[o + c] = (sum[c] / n.max(1)) as u8;
                }
            }
        }
        let texture = gdk::MemoryTexture::new(
            columns as i32,
            rows as i32,
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &glib::Bytes::from_owned(out),
            columns * 4,
        )
        .upcast::<gdk::Texture>();
        self.mosaics.borrow_mut().insert(id, (area, base.id, texture.clone()));
        Some(texture)
    }
}

// MARK: Drawing helpers

/// A cairo surface covering `bounds` in snapshot units, set up to draw in image points.
fn cairo_in(snapshot: &gtk::Snapshot, bounds: R, view: View) -> cairo::Context {
    let cr = snapshot.append_cairo(&grect(bounds));
    cr.translate(view.ox, view.oy);
    cr.scale(view.zoom, view.zoom);
    cr
}

/// Every stroked mark carries a soft shadow, so it reads on any background.
fn shadowed(snapshot: &gtk::Snapshot, zoom: f64, body: impl FnOnce()) {
    let color = gdk::RGBA::new(0.0, 0.0, 0.0, 0.28);
    snapshot.push_shadow(&[gsk::Shadow::new(color, 0.0, (1.5 * zoom) as f32, (4.0 * zoom) as f32)]);
    body();
    snapshot.pop();
}

fn set(cr: &cairo::Context, c: Rgba) {
    cr.set_source_rgba(c.0, c.1, c.2, c.3);
}

pub fn rounded_rect(cr: &cairo::Context, r: R, radius: f64) {
    let radius = radius.max(0.0).min(r.w / 2.0).min(r.h / 2.0);
    cr.new_sub_path();
    cr.arc(r.max_x() - radius, r.y + radius, radius, -PI / 2.0, 0.0);
    cr.arc(r.max_x() - radius, r.max_y() - radius, radius, 0.0, PI / 2.0);
    cr.arc(r.x + radius, r.max_y() - radius, radius, PI / 2.0, PI);
    cr.arc(r.x + radius, r.y + radius, radius, PI, 1.5 * PI);
    cr.close_path();
}

fn quad_to(cr: &cairo::Context, from: P, c: P, to: P) {
    let c1 = from + (c - from) * (2.0 / 3.0);
    let c2 = to + (c - to) * (2.0 / 3.0);
    cr.curve_to(c1.x, c1.y, c2.x, c2.y, to.x, to.y);
}

/// Quadratic midpoint smoothing, which removes the jitter of raw mouse samples.
fn smooth_path(cr: &cairo::Context, points: &[P]) {
    let Some(first) = points.first() else { return };
    cr.move_to(first.x, first.y);
    if points.len() <= 2 {
        let last = points.last().unwrap_or(first);
        cr.line_to(last.x, last.y);
        return;
    }
    let mut current = *first;
    for i in 1..points.len() - 1 {
        let mid = (points[i] + points[i + 1]) * 0.5;
        quad_to(cr, current, points[i], mid);
        current = mid;
    }
    let last = points[points.len() - 1];
    cr.line_to(last.x, last.y);
}

fn stroke(cr: &cairo::Context, a: &Annotation) {
    let width = a.weight.stroke();
    let path = cr.copy_path().ok();
    set(cr, a.color.halo());
    cr.set_line_width(width + HALO_WIDTH * 2.0);
    let _ = cr.stroke();
    if a.filled && a.tool().supports_fill() {
        if let Some(path) = &path {
            cr.append_path(path);
            set(cr, a.color.rgba().with_alpha(0.2));
            let _ = cr.fill();
        }
    }
    if let Some(path) = &path {
        cr.append_path(path);
    }
    set(cr, a.color.rgba());
    cr.set_line_width(width);
    let _ = cr.stroke();
}

fn fill_and_stroke_twice(cr: &cairo::Context, a: &Annotation, rounding: f64) {
    let path = cr.copy_path().ok();
    cr.set_line_join(cairo::LineJoin::Round);
    set(cr, a.color.halo());
    let _ = cr.fill_preserve();
    cr.set_line_width(rounding + HALO_WIDTH * 2.0);
    let _ = cr.stroke();
    if let Some(path) = &path {
        cr.append_path(path);
    }
    set(cr, a.color.rgba());
    let _ = cr.fill_preserve();
    cr.set_line_width(rounding);
    let _ = cr.stroke();
}

/// A tapered shaft that swells into a swept, round-cornered head.
fn arrow(cr: &cairo::Context, start: P, end: P, a: &Annotation) {
    let w = a.weight.stroke();
    let delta = end - start;
    let length = delta.len().max(0.001);
    let u = delta * (1.0 / length);
    let n = p(-u.y, u.x);
    // The outline is stroked with a round join to soften every corner, so the geometry sits
    // inside the final silhouette by half that stroke.
    let rounding = w * 0.5;
    let head_length = (w * 4.4).max(13.0).min(length * 0.62);
    let head_half = head_length * 0.52;
    let tip = end - u * (rounding / 2.0);
    let wings = tip - u * head_length;
    let neck = tip - u * (head_length * 0.8);
    let neck_half = (w * 0.38).min(head_half * 0.4);
    let tail_half = w * 0.14;
    let pts = [
        start + n * tail_half,
        neck + n * neck_half,
        wings + n * head_half,
        tip,
        wings - n * head_half,
        neck - n * neck_half,
        start - n * tail_half,
    ];
    cr.move_to(pts[0].x, pts[0].y);
    for q in &pts[1..] {
        cr.line_to(q.x, q.y);
    }
    cr.close_path();
    fill_and_stroke_twice(cr, a, rounding);
}

/// The same tapered arrow, with its shaft following a quadratic curve. The head points along
/// the curve's direction where it arrives.
fn curved_arrow(cr: &cairo::Context, start: P, control: P, end: P, a: &Annotation) {
    let w = a.weight.stroke();
    let arrival = end - control;
    let u = arrival * (1.0 / arrival.len().max(0.001));
    let n = p(-u.y, u.x);
    let samples = 48;
    let curve: Vec<P> = (0..=samples).map(|i| quad_point(start, control, end, i as f64 / samples as f64)).collect();
    let length: f64 = curve.windows(2).map(|s| s[0].dist(s[1])).sum();
    let rounding = w * 0.5;
    let head_length = (w * 4.4).max(13.0).min(length * 0.62);
    let head_half = head_length * 0.52;
    let tip = end - u * (rounding / 2.0);
    let wings = tip - u * head_length;
    let neck = tip - u * (head_length * 0.8);
    let neck_half = (w * 0.38).min(head_half * 0.4);
    let tail_half = w * 0.14;

    // The shaft runs along the curve until it reaches the neck of the head.
    let mut shaft: Vec<P> = Vec::new();
    for q in &curve {
        if q.dist(tip) <= head_length * 0.8 {
            break;
        }
        shaft.push(*q);
    }
    if shaft.len() < 2 {
        arrow(cr, start, end, a);
        return;
    }
    shaft.push(neck);
    let count = shaft.len();
    let mut left = Vec::with_capacity(count);
    let mut right = Vec::with_capacity(count);
    for (i, q) in shaft.iter().enumerate() {
        let prev = shaft[i.saturating_sub(1)];
        let next = shaft[(i + 1).min(count - 1)];
        let d = next - prev;
        let normal = p(-d.y, d.x) * (1.0 / d.len().max(0.001));
        let t = i as f64 / (count - 1) as f64;
        let half = tail_half + (neck_half - tail_half) * t;
        left.push(*q + normal * half);
        right.push(*q - normal * half);
    }
    cr.move_to(left[0].x, left[0].y);
    for q in &left[1..] {
        cr.line_to(q.x, q.y);
    }
    for q in [wings + n * head_half, tip, wings - n * head_half] {
        cr.line_to(q.x, q.y);
    }
    for q in right.iter().rev() {
        cr.line_to(q.x, q.y);
    }
    cr.close_path();
    fill_and_stroke_twice(cr, a, rounding);
}

fn counter_layout(number: i32, radius: f64) -> gtk::pango::Layout {
    thread_local! {
        static CONTEXT: gtk::pango::Context = pangocairo::FontMap::default().create_context();
    }
    CONTEXT.with(|ctx| {
        let layout = gtk::pango::Layout::new(ctx);
        let mut font = gtk::pango::FontDescription::from_string("Adwaita Sans Bold");
        let size = radius * if number > 9 { 0.95 } else { 1.15 };
        font.set_absolute_size(size * gtk::pango::SCALE as f64);
        layout.set_font_description(Some(&font));
        let attrs = gtk::pango::AttrList::new();
        attrs.insert(gtk::pango::AttrFontFeatures::new("tnum 1"));
        layout.set_attributes(Some(&attrs));
        layout.set_text(&number.to_string());
        layout
    })
}

// MARK: Export

/// Builds the flattened scene (base image and marks) in pixels, at `output_scale` pixels per point.
pub fn scene_node(
    renderer: &Renderer,
    base: &BaseImage,
    scale: f64,
    annotations: &[Annotation],
    output_scale: f64,
) -> Option<(gsk::RenderNode, i32, i32)> {
    let width = (base.width() as f64 / scale * output_scale).round() as i32;
    let height = (base.height() as f64 / scale * output_scale).round() as i32;
    if width <= 0 || height <= 0 {
        return None;
    }
    let snapshot = gtk::Snapshot::new();
    let full = R::new(0.0, 0.0, width as f64, height as f64);
    let filter = if (output_scale - scale).abs() < 1e-6 { gsk::ScalingFilter::Nearest } else { gsk::ScalingFilter::Trilinear };
    snapshot.append_scaled_texture(&base.texture, filter, &grect(full));
    renderer.draw(&snapshot, annotations, base, scale, View { ox: 0.0, oy: 0.0, zoom: output_scale }, full);
    let node = snapshot.to_node()?;
    // Keep the result exactly the image's size, even where a shadow spills past the edge.
    let clipped = gsk::ClipNode::new(&node, &grect(full)).upcast();
    Some((clipped, width, height))
}

/// Flattens the base image and its annotations on the GPU.
pub fn flatten(
    gsk_renderer: &gsk::Renderer,
    renderer: &Renderer,
    base: &BaseImage,
    scale: f64,
    annotations: &[Annotation],
    output_scale: f64,
) -> Option<gdk::Texture> {
    if annotations.is_empty() && (output_scale - scale).abs() < 1e-6 {
        return Some(base.texture.clone());
    }
    let (node, w, h) = scene_node(renderer, base, scale, annotations, output_scale)?;
    Some(gsk_renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32))))
}

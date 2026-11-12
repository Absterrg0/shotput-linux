//! The marks drawn on a screenshot, ported from Shotput's Annotation.swift, Geometry.swift and
//! ShapeRecognizer.swift. Coordinates are image points with a top-left origin.

use std::sync::atomic::{AtomicU64, Ordering};

// MARK: Geometry

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct P {
    pub x: f64,
    pub y: f64,
}

pub const fn p(x: f64, y: f64) -> P {
    P { x, y }
}

impl std::ops::Add for P {
    type Output = P;
    fn add(self, o: P) -> P {
        p(self.x + o.x, self.y + o.y)
    }
}
impl std::ops::Sub for P {
    type Output = P;
    fn sub(self, o: P) -> P {
        p(self.x - o.x, self.y - o.y)
    }
}
impl std::ops::Mul<f64> for P {
    type Output = P;
    fn mul(self, k: f64) -> P {
        p(self.x * k, self.y * k)
    }
}

impl P {
    pub fn len(self) -> f64 {
        self.x.hypot(self.y)
    }
    pub fn dist(self, o: P) -> f64 {
        (self - o).len()
    }
    pub fn dist_seg(self, a: P, b: P) -> f64 {
        let ab = b - a;
        let l2 = ab.x * ab.x + ab.y * ab.y;
        if l2 <= 0.0 {
            return self.dist(a);
        }
        let t = (((self.x - a.x) * ab.x + (self.y - a.y) * ab.y) / l2).clamp(0.0, 1.0);
        self.dist(a + ab * t)
    }
    pub fn clamped(self, r: R) -> P {
        p(self.x.clamp(r.x, r.max_x()), self.y.clamp(r.y, r.max_y()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct R {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl R {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> R {
        R { x, y, w, h }
    }
    pub fn from_points(a: P, b: P) -> R {
        R::new(a.x.min(b.x), a.y.min(b.y), (a.x - b.x).abs(), (a.y - b.y).abs())
    }
    pub fn max_x(&self) -> f64 {
        self.x + self.w
    }
    pub fn max_y(&self) -> f64 {
        self.y + self.h
    }
    pub fn mid_x(&self) -> f64 {
        self.x + self.w / 2.0
    }
    pub fn mid_y(&self) -> f64 {
        self.y + self.h / 2.0
    }
    pub fn center(&self) -> P {
        p(self.mid_x(), self.mid_y())
    }
    pub fn inset(&self, dx: f64, dy: f64) -> R {
        R::new(self.x + dx, self.y + dy, self.w - 2.0 * dx, self.h - 2.0 * dy)
    }
    pub fn offset(&self, d: P) -> R {
        R::new(self.x + d.x, self.y + d.y, self.w, self.h)
    }
    pub fn contains(&self, q: P) -> bool {
        self.w >= 0.0 && self.h >= 0.0 && q.x >= self.x && q.x <= self.max_x() && q.y >= self.y && q.y <= self.max_y()
    }
    pub fn union(&self, o: &R) -> R {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        R::new(x, y, self.max_x().max(o.max_x()) - x, self.max_y().max(o.max_y()) - y)
    }
    pub fn intersection(&self, o: &R) -> Option<R> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let mx = self.max_x().min(o.max_x());
        let my = self.max_y().min(o.max_y());
        (mx > x && my > y).then(|| R::new(x, y, mx - x, my - y))
    }
    pub fn integral(&self) -> R {
        let x = self.x.floor();
        let y = self.y.floor();
        R::new(x, y, self.max_x().ceil() - x, self.max_y().ceil() - y)
    }
    /// Top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self) -> [P; 4] {
        [p(self.x, self.y), p(self.max_x(), self.y), p(self.max_x(), self.max_y()), p(self.x, self.max_y())]
    }
    pub fn bounding(points: &[P]) -> R {
        let Some(first) = points.first() else { return R::default() };
        points[1..].iter().fold(R::new(first.x, first.y, 0.0, 0.0), |r, q| r.union(&R::new(q.x, q.y, 0.0, 0.0)))
    }
}

/// A point on the quadratic curve from `a` to `b` with control `c`.
pub fn quad_point(a: P, c: P, b: P, t: f64) -> P {
    let u = 1.0 - t;
    a * (u * u) + c * (2.0 * u * t) + b * (t * t)
}

// MARK: Tools, weights, inks

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Arrow,
    Line,
    Rectangle,
    Ellipse,
    Text,
    Counter,
    Pen,
    Highlight,
    Redact,
    Crop,
}

impl Tool {
    pub const ALL: [Tool; 10] = [
        Tool::Arrow,
        Tool::Line,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Text,
        Tool::Counter,
        Tool::Pen,
        Tool::Highlight,
        Tool::Redact,
        Tool::Crop,
    ];

    pub fn index(self) -> usize {
        Tool::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
    pub fn from_index(i: i32) -> Tool {
        Tool::ALL.get(i.max(0) as usize).copied().unwrap_or(Tool::Arrow)
    }

    pub fn title(self) -> &'static str {
        match self {
            Tool::Arrow => "Arrow",
            Tool::Line => "Line",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Text => "Text",
            Tool::Counter => "Step number",
            Tool::Pen => "Pen",
            Tool::Highlight => "Highlighter",
            Tool::Redact => "Redact",
            Tool::Crop => "Crop",
        }
    }

    /// Single-key shortcut while the editor has focus.
    pub fn key(self) -> char {
        match self {
            Tool::Arrow => 'a',
            Tool::Line => 'l',
            Tool::Rectangle => 'r',
            Tool::Ellipse => 'o',
            Tool::Text => 't',
            Tool::Counter => 'n',
            Tool::Pen => 'p',
            Tool::Highlight => 'h',
            Tool::Redact => 'b',
            Tool::Crop => 'c',
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Tool::Arrow => "shotput-arrow-symbolic",
            Tool::Line => "shotput-line-symbolic",
            Tool::Rectangle => "shotput-rectangle-symbolic",
            Tool::Ellipse => "shotput-ellipse-symbolic",
            Tool::Text => "shotput-text-symbolic",
            Tool::Counter => "shotput-counter-symbolic",
            Tool::Pen => "shotput-pen-symbolic",
            Tool::Highlight => "shotput-highlighter-symbolic",
            Tool::Redact => "shotput-redact-symbolic",
            Tool::Crop => "shotput-crop-symbolic",
        }
    }

    pub fn supports_fill(self) -> bool {
        matches!(self, Tool::Rectangle | Tool::Ellipse | Tool::Text | Tool::Redact)
    }
    pub fn supports_weight(self) -> bool {
        !matches!(self, Tool::Highlight | Tool::Redact | Tool::Crop)
    }
    pub fn supports_color(self) -> bool {
        self != Tool::Crop
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Light,
    Regular,
    Heavy,
    ExtraHeavy,
}

impl Weight {
    pub const ALL: [Weight; 4] = [Weight::Light, Weight::Regular, Weight::Heavy, Weight::ExtraHeavy];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn from_index(i: i32) -> Weight {
        Weight::ALL.get(i.max(0) as usize).copied().unwrap_or(Weight::Regular)
    }
    pub fn stroke(self) -> f64 {
        [2.5, 4.0, 6.5, 10.0][self.index()]
    }
    pub fn font_size(self) -> f64 {
        [14.0, 19.0, 28.0, 40.0][self.index()]
    }
    pub fn counter_radius(self) -> f64 {
        [11.0, 14.0, 19.0, 26.0][self.index()]
    }
    pub fn name(self) -> &'static str {
        ["Thin", "Regular", "Bold", "Extra bold"][self.index()]
    }
}

/// Annotation inks. Fixed values, since they are drawn onto screenshots rather than onto app chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Violet,
    Black,
    White,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f64, pub f64, pub f64, pub f64);

impl Rgba {
    pub const fn hex(hex: u32, a: f64) -> Rgba {
        Rgba(((hex >> 16) & 0xff) as f64 / 255.0, ((hex >> 8) & 0xff) as f64 / 255.0, (hex & 0xff) as f64 / 255.0, a)
    }
    pub fn with_alpha(self, a: f64) -> Rgba {
        Rgba(self.0, self.1, self.2, a)
    }
    pub fn gdk(self) -> gtk::gdk::RGBA {
        gtk::gdk::RGBA::new(self.0 as f32, self.1 as f32, self.2 as f32, self.3 as f32)
    }
    pub fn to_hex(self) -> String {
        format!(
            "#{:02x}{:02x}{:02x}",
            (self.0 * 255.0).round() as u8,
            (self.1 * 255.0).round() as u8,
            (self.2 * 255.0).round() as u8
        )
    }
    pub fn parse(s: &str) -> Option<Rgba> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        u32::from_str_radix(s, 16).ok().map(|v| Rgba::hex(v, 1.0))
    }
}

impl Ink {
    pub const ALL: [Ink; 8] = [Ink::Red, Ink::Orange, Ink::Yellow, Ink::Green, Ink::Blue, Ink::Violet, Ink::Black, Ink::White];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn from_index(i: i32) -> Ink {
        Ink::ALL.get(i.max(0) as usize).copied().unwrap_or(Ink::Red)
    }
    pub fn hex(self) -> u32 {
        match self {
            Ink::Red => 0xE5484D,
            Ink::Orange => 0xF76B15,
            Ink::Yellow => 0xFFC53D,
            Ink::Green => 0x30A46C,
            Ink::Blue => 0x1F6FE5,
            Ink::Violet => 0x8E4EC6,
            Ink::Black => 0x2B2A26,
            Ink::White => 0xFFFFFF,
        }
    }
    pub fn name(self) -> &'static str {
        ["Red", "Orange", "Yellow", "Green", "Blue", "Violet", "Black", "White"][self.index()]
    }
    pub fn rgba(self) -> Rgba {
        Rgba::hex(self.hex(), 1.0)
    }
    fn is_light(self) -> bool {
        matches!(self, Ink::Yellow | Ink::White)
    }
    /// Text or glyph colour that stays legible on top of this ink.
    pub fn on_ink(self) -> Rgba {
        if self.is_light() { Rgba::hex(0x2B2A26, 1.0) } else { Rgba(1.0, 1.0, 1.0, 1.0) }
    }
    /// Thin outline that keeps the ink visible on a background of its own colour.
    pub fn halo(self) -> Rgba {
        if self.is_light() { Rgba::hex(0x2B2A26, 0.5) } else { Rgba::hex(0xFFFFFF, 0.92) }
    }
}

// MARK: Annotation

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Arrow(P, P),
    Line(P, P),
    Rectangle(R),
    Ellipse(R),
    Text(P, String),
    Counter(P, i32),
    Pen(Vec<P>),
    Highlight(R),
    Redact(R),
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn new_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub id: u64,
    pub kind: Kind,
    pub color: Ink,
    pub weight: Weight,
    /// Shapes: tinted interior. Text: label pill. Redact: solid block instead of pixels.
    pub filled: bool,
    /// Lines and arrows: the control point of a quadratic curve. None keeps them straight.
    pub bend: Option<P>,
}

/// Measures label text the same way it is drawn.
pub mod text_metrics {
    use super::*;

    use gtk::pango::prelude::*;

    pub fn font(weight: Weight) -> gtk::pango::FontDescription {
        let mut font = gtk::pango::FontDescription::from_string("Adwaita Sans Semi-Bold");
        font.set_absolute_size(weight.font_size() * gtk::pango::SCALE as f64);
        font
    }

    pub fn padding(weight: Weight) -> (f64, f64) {
        ((weight.font_size() * 0.5).round(), (weight.font_size() * 0.22).round())
    }

    thread_local! {
        static CONTEXT: gtk::pango::Context = {
            let map = pangocairo::FontMap::default();
            let ctx = map.create_context();
            let mut options = gtk::cairo::FontOptions::new().expect("font options");
            options.set_hint_metrics(gtk::cairo::HintMetrics::Off);
            options.set_hint_style(gtk::cairo::HintStyle::None);
            pangocairo::functions::context_set_font_options(&ctx, Some(&options));
            ctx
        };
    }

    pub fn layout(string: &str, weight: Weight) -> gtk::pango::Layout {
        CONTEXT.with(|ctx| {
            let layout = gtk::pango::Layout::new(ctx);
            layout.set_font_description(Some(&font(weight)));
            layout.set_text(if string.is_empty() { " " } else { string });
            layout
        })
    }

    pub fn text_size(string: &str, weight: Weight) -> (f64, f64) {
        let (_, logical) = layout(string, weight).extents();
        let s = gtk::pango::SCALE as f64;
        ((logical.width() as f64 / s).ceil(), (logical.height() as f64 / s).ceil())
    }

    /// The label's outer frame, padding included.
    pub fn frame(origin: P, string: &str, weight: Weight) -> R {
        let (w, h) = text_size(string, weight);
        let (px, py) = padding(weight);
        R::new(origin.x, origin.y, w + px * 2.0, h + py * 2.0)
    }
}

impl Annotation {
    pub fn new(kind: Kind, color: Ink, weight: Weight, filled: bool) -> Annotation {
        Annotation { id: new_id(), kind, color, weight, filled, bend: None }
    }

    pub fn tool(&self) -> Tool {
        match self.kind {
            Kind::Arrow(..) => Tool::Arrow,
            Kind::Line(..) => Tool::Line,
            Kind::Rectangle(_) => Tool::Rectangle,
            Kind::Ellipse(_) => Tool::Ellipse,
            Kind::Text(..) => Tool::Text,
            Kind::Counter(..) => Tool::Counter,
            Kind::Pen(_) => Tool::Pen,
            Kind::Highlight(_) => Tool::Highlight,
            Kind::Redact(_) => Tool::Redact,
        }
    }

    pub fn endpoints(&self) -> Option<(P, P)> {
        match self.kind {
            Kind::Arrow(a, b) | Kind::Line(a, b) => Some((a, b)),
            _ => None,
        }
    }

    /// Points along a line or arrow, curved or straight.
    pub fn path_points(&self, samples: usize) -> Option<Vec<P>> {
        let (a, b) = self.endpoints()?;
        Some(match self.bend {
            None => vec![a, b],
            Some(c) => (0..=samples).map(|i| quad_point(a, c, b, i as f64 / samples as f64)).collect(),
        })
    }

    /// The point halfway along the curve, where its bend handle sits.
    pub fn bend_handle(&self) -> Option<P> {
        let (a, b) = self.endpoints()?;
        Some(match self.bend {
            None => (a + b) * 0.5,
            Some(c) => quad_point(a, c, b, 0.5),
        })
    }

    /// Bends the curve so it passes through `handle` halfway along. Close to straight, it snaps straight.
    pub fn bent_through(&self, handle: P) -> Annotation {
        let mut copy = self.clone();
        if let Some((a, b)) = self.endpoints() {
            let mid = (a + b) * 0.5;
            copy.bend = if handle.dist(mid) < 4.0 { None } else { Some(handle * 2.0 - mid) };
        }
        copy
    }

    /// Geometric bounds, ignoring stroke width.
    pub fn frame(&self) -> R {
        match &self.kind {
            Kind::Arrow(..) | Kind::Line(..) => R::bounding(&self.path_points(24).unwrap_or_default()),
            Kind::Rectangle(r) | Kind::Ellipse(r) | Kind::Highlight(r) | Kind::Redact(r) => *r,
            Kind::Text(origin, s) => text_metrics::frame(*origin, s, self.weight),
            Kind::Counter(c, _) => {
                let r = self.weight.counter_radius();
                R::new(c.x - r, c.y - r, r * 2.0, r * 2.0)
            }
            Kind::Pen(points) => R::bounding(points),
        }
    }

    /// Rect-shaped kinds resize from their corners.
    pub fn resizable_rect(&self) -> Option<R> {
        match self.kind {
            Kind::Rectangle(r) | Kind::Ellipse(r) | Kind::Highlight(r) | Kind::Redact(r) => Some(r),
            _ => None,
        }
    }

    pub fn with_rect(&self, rect: R) -> Annotation {
        let mut copy = self.clone();
        copy.kind = match self.kind {
            Kind::Rectangle(_) => Kind::Rectangle(rect),
            Kind::Ellipse(_) => Kind::Ellipse(rect),
            Kind::Highlight(_) => Kind::Highlight(rect),
            Kind::Redact(_) => Kind::Redact(rect),
            ref other => other.clone(),
        };
        copy
    }

    pub fn with_endpoints(&self, from: P, to: P) -> Annotation {
        let mut copy = self.clone();
        // Dragging an end carries the curve along, so the bend keeps its character.
        if let (Some(bend), Some((a, b))) = (self.bend, self.endpoints()) {
            copy.bend = Some(bend + ((from - a) + (to - b)) * 0.5);
        }
        copy.kind = match self.kind {
            Kind::Arrow(..) => Kind::Arrow(from, to),
            Kind::Line(..) => Kind::Line(from, to),
            ref other => other.clone(),
        };
        copy
    }

    pub fn translated(&self, d: P) -> Annotation {
        let mut copy = self.clone();
        copy.kind = match &self.kind {
            Kind::Arrow(a, b) => Kind::Arrow(*a + d, *b + d),
            Kind::Line(a, b) => Kind::Line(*a + d, *b + d),
            Kind::Rectangle(r) => Kind::Rectangle(r.offset(d)),
            Kind::Ellipse(r) => Kind::Ellipse(r.offset(d)),
            Kind::Highlight(r) => Kind::Highlight(r.offset(d)),
            Kind::Redact(r) => Kind::Redact(r.offset(d)),
            Kind::Text(o, s) => Kind::Text(*o + d, s.clone()),
            Kind::Counter(c, n) => Kind::Counter(*c + d, *n),
            Kind::Pen(points) => Kind::Pen(points.iter().map(|q| *q + d).collect()),
        };
        copy.bend = self.bend.map(|b| b + d);
        copy
    }

    /// Marks whose whole interior is solid: highlights, redactions, and filled shapes.
    pub fn covers_area(&self) -> bool {
        match self.kind {
            Kind::Highlight(_) | Kind::Redact(_) => true,
            Kind::Rectangle(_) | Kind::Ellipse(_) => self.filled,
            _ => false,
        }
    }

    /// Whether the point is on the mark's edge, as opposed to anywhere inside it.
    pub fn hits_outline(&self, q: P, tolerance: f64) -> bool {
        let mut outline = self.clone();
        outline.filled = false;
        if let Kind::Highlight(r) | Kind::Redact(r) = self.kind {
            outline.kind = Kind::Rectangle(r);
        }
        outline.hit_test(q, tolerance)
    }

    pub fn hit_test(&self, q: P, tolerance: f64) -> bool {
        let reach = (self.weight.stroke() / 2.0).max(tolerance);
        match &self.kind {
            Kind::Arrow(..) | Kind::Line(..) => {
                // The head is wider than the shaft, so give arrows a little more reach.
                let extra = if self.tool() == Tool::Arrow { self.weight.stroke() } else { 0.0 };
                let points = self.path_points(24).unwrap_or_default();
                points.windows(2).any(|w| q.dist_seg(w[0], w[1]) <= reach + extra)
            }
            Kind::Rectangle(r) => {
                if !r.inset(-reach, -reach).contains(q) {
                    return false;
                }
                self.filled || !r.inset(reach, reach).contains(q)
            }
            Kind::Ellipse(r) => {
                if r.w <= 0.0 || r.h <= 0.0 {
                    return false;
                }
                let nx = (q.x - r.mid_x()) / (r.w / 2.0);
                let ny = (q.y - r.mid_y()) / (r.h / 2.0);
                let radial = nx.hypot(ny);
                let band = reach / (r.w.min(r.h) / 2.0);
                if self.filled { radial <= 1.0 + band } else { (radial - 1.0).abs() <= band }
            }
            Kind::Text(..) | Kind::Counter(..) | Kind::Highlight(_) | Kind::Redact(_) => {
                self.frame().inset(-2.0, -2.0).contains(q)
            }
            Kind::Pen(points) => {
                if points.len() < 2 {
                    return points.first().is_some_and(|f| f.dist(q) <= reach);
                }
                points.windows(2).any(|w| q.dist_seg(w[0], w[1]) <= reach)
            }
        }
    }
}

// MARK: Shape recognition

/// Turns a freehand stroke into the shape it was meant to be: a straight stroke becomes a line,
/// a stroke with a hook at its far end an arrow, and a closed loop an ellipse or a rectangle.
/// Anything ambiguous stays a sketch.
pub mod shapes {
    use super::*;

    pub enum Shape {
        Line(P, P),
        Arrow(P, P),
        Rectangle(R),
        Ellipse(R),
    }

    impl Shape {
        pub fn name(&self) -> &'static str {
            match self {
                Shape::Line(..) => "line",
                Shape::Arrow(..) => "arrow",
                Shape::Rectangle(r) => {
                    if (r.w - r.h).abs() < 0.5 { "square" } else { "rectangle" }
                }
                Shape::Ellipse(r) => {
                    if (r.w - r.h).abs() < 0.5 { "circle" } else { "ellipse" }
                }
            }
        }
        pub fn kind(&self) -> Kind {
            match *self {
                Shape::Line(a, b) => Kind::Line(a, b),
                Shape::Arrow(a, b) => Kind::Arrow(a, b),
                Shape::Rectangle(r) => Kind::Rectangle(r),
                Shape::Ellipse(r) => Kind::Ellipse(r),
            }
        }
    }

    pub fn recognize(raw: &[P]) -> Option<Shape> {
        let points = resample(raw, 64);
        if points.len() < 8 {
            return None;
        }
        let (first, last) = (points[0], points[points.len() - 1]);
        let bounds = R::bounding(&points);
        let size = bounds.w.max(bounds.h);
        if size < 16.0 {
            return None;
        }
        let length = path_length(&points);

        // Closed: the end comes back near the start, and the stroke goes most of the way round.
        if first.dist(last) < size * 0.22 && length > (bounds.w + bounds.h) * 1.5 {
            return closed_shape(&points, bounds);
        }

        // Straight: barely strays from the line between its ends.
        let chord = first.dist(last);
        if chord > 16.0 && max_deviation(&points, first, last) < chord * 0.07 && length < chord * 1.12 {
            return Some(Shape::Line(first, last));
        }
        arrow(&points)
    }

    /// A straight shaft to the point farthest from the start, then a short hook back from it.
    fn arrow(points: &[P]) -> Option<Shape> {
        let start = *points.first()?;
        let tip_index = (0..points.len()).max_by(|&a, &b| {
            points[a].dist(start).partial_cmp(&points[b].dist(start)).unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let fraction = tip_index as f64 / (points.len() - 1) as f64;
        if !(fraction > 0.45 && fraction < 0.93) {
            return None;
        }
        let tip = points[tip_index];
        let shaft_length = start.dist(tip);
        if shaft_length <= 24.0 {
            return None;
        }
        if max_deviation(&points[..=tip_index], start, tip) >= shaft_length * 0.08 {
            return None;
        }
        let hook = &points[tip_index..];
        let hook_length = path_length(hook);
        if hook_length > shaft_length * 0.08
            && hook_length < shaft_length * 0.9
            && hook.iter().all(|q| q.dist(tip) < shaft_length * 0.45)
        {
            return Some(Shape::Arrow(start, tip));
        }
        None
    }

    fn closed_shape(points: &[P], bounds: R) -> Option<Shape> {
        if bounds.w <= 12.0 || bounds.h <= 12.0 {
            return None;
        }
        let (hw, hh) = (bounds.w / 2.0, bounds.h / 2.0);
        let c = bounds.center();
        let n = points.len() as f64;
        let ellipse_error =
            points.iter().map(|q| (((q.x - c.x) / hw).hypot((q.y - c.y) / hh) - 1.0).abs()).sum::<f64>() / n;
        let rectangle_error = points
            .iter()
            .map(|q| {
                let to_edge = (q.x - bounds.x)
                    .abs()
                    .min((q.x - bounds.max_x()).abs())
                    .min((q.y - bounds.y).abs())
                    .min((q.y - bounds.max_y()).abs());
                to_edge / hw.min(hh)
            })
            .sum::<f64>()
            / n;

        // Near-equal sides were almost certainly meant to be equal.
        let mut rect = bounds;
        if (bounds.w - bounds.h).abs() < bounds.w.max(bounds.h) * 0.1 {
            let side = (bounds.w + bounds.h) / 2.0;
            rect = R::new(c.x - side / 2.0, c.y - side / 2.0, side, side);
        }
        if rectangle_error < ellipse_error && rectangle_error < 0.13 {
            return Some(Shape::Rectangle(rect));
        }
        if ellipse_error <= rectangle_error && ellipse_error < 0.14 {
            return Some(Shape::Ellipse(rect));
        }
        None
    }

    fn path_length(points: &[P]) -> f64 {
        points.windows(2).map(|w| w[0].dist(w[1])).sum()
    }

    fn max_deviation(points: &[P], a: P, b: P) -> f64 {
        points.iter().map(|q| q.dist_seg(a, b)).fold(0.0, f64::max)
    }

    /// Evenly spaced points along the stroke, so fast and slow drawing compare alike.
    fn resample(points: &[P], count: usize) -> Vec<P> {
        if points.len() <= 1 {
            return points.to_vec();
        }
        let total = path_length(points);
        if total <= 0.0 {
            return vec![points[0]];
        }
        let step = total / (count - 1) as f64;
        let mut result = vec![points[0]];
        let mut carried = 0.0;
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let mut segment = a.dist(b);
            let mut from = a;
            while carried + segment >= step && segment > 0.0 {
                let t = (step - carried) / segment;
                let q = from + (b - from) * t;
                result.push(q);
                segment -= step - carried;
                from = q;
                carried = 0.0;
            }
            carried += segment;
        }
        if result.len() < count {
            result.push(points[points.len() - 1]);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_line_and_circle() {
        let line: Vec<P> = (0..40).map(|i| p(i as f64 * 5.0, i as f64 * 2.0)).collect();
        assert!(matches!(shapes::recognize(&line), Some(shapes::Shape::Line(..))));
        let circle: Vec<P> = (0..=80)
            .map(|i| {
                let t = i as f64 / 80.0 * std::f64::consts::TAU;
                p(100.0 + 60.0 * t.cos(), 100.0 + 60.0 * t.sin())
            })
            .collect();
        assert!(matches!(shapes::recognize(&circle), Some(shapes::Shape::Ellipse(_))));
        let rect: Vec<P> = [p(0.0, 0.0), p(200.0, 0.0), p(200.0, 100.0), p(0.0, 100.0), p(0.0, 2.0)]
            .windows(2)
            .flat_map(|w| (0..20).map(move |i| w[0] + (w[1] - w[0]) * (i as f64 / 20.0)))
            .collect();
        assert!(matches!(shapes::recognize(&rect), Some(shapes::Shape::Rectangle(_))));
    }

    #[test]
    fn hit_tests() {
        let a = Annotation::new(Kind::Rectangle(R::new(10.0, 10.0, 100.0, 50.0)), Ink::Red, Weight::Regular, false);
        assert!(a.hit_test(p(10.0, 30.0), 6.0));
        assert!(!a.hit_test(p(60.0, 35.0), 6.0));
        let bent = Annotation::new(Kind::Line(p(0.0, 0.0), p(100.0, 0.0)), Ink::Red, Weight::Regular, false)
            .bent_through(p(50.0, 40.0));
        assert!(bent.bend.is_some());
        assert!((bent.bend_handle().unwrap().y - 40.0).abs() < 1e-9);
    }
}

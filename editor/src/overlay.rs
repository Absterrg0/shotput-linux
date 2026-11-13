//! The selection overlay on desktops without the GNOME extension (KDE Plasma, Hyprland, Sway).
//! A port of the extension's overlay.js: one surface per monitor shows that monitor's frozen
//! still, and the selection, guides, magnifier and size badge are drawn over it in layout
//! coordinates, so a drag can cross monitors the same way the pointer does.

use crate::canvas::Canvas;
use crate::grab::Still;
use crate::layer;
use crate::model::{P, R, p};
use crate::settings::Settings;
use gtk::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use std::cell::RefCell;
use std::rc::Rc;

const SNAP_DISTANCE: f64 = 7.0;
const LOUPE_SIZE: f64 = 116.0;
const LOUPE_ZOOM: f64 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Area,
    Window,
}

/// A window on screen, in layout coordinates, front to back.
#[derive(Clone, Debug)]
pub struct WinInfo {
    pub rect: R,
    pub name: Option<String>,
}

pub enum Outcome {
    Cancelled,
    Area(R),
    Window(WinInfo),
}

struct State {
    mode: Mode,
    pointer: Option<P>,
    anchor: Option<P>,
    selection: Option<R>,
    hovered: Option<usize>,
    moving: bool,
    space: bool,
    snapped: (bool, bool),
    hint: bool,
}

pub struct Overlay {
    stills: Vec<Still>,
    windows: Vec<WinInfo>,
    settings: Settings,
    state: RefCell<State>,
    surfaces: RefCell<Vec<(gtk::Window, Canvas)>>,
    done: RefCell<Option<Box<dyn FnOnce(Outcome)>>>,
}

fn rect_of(g: &gdk::Rectangle) -> R {
    R::new(g.x() as f64, g.y() as f64, g.width() as f64, g.height() as f64)
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(r, g, b, a)
}

fn accent() -> gdk::RGBA {
    let style = adw::StyleManager::default();
    if style.find_property("accent-color-rgba").is_some() {
        return style.property::<gdk::RGBA>("accent-color-rgba");
    }
    rgba(0.208, 0.518, 0.894, 1.0)
}

fn grect(r: R) -> graphene::Rect {
    graphene::Rect::new(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
}

impl Overlay {
    pub fn open(
        app: &adw::Application,
        stills: Vec<Still>,
        windows: Vec<WinInfo>,
        mode: Mode,
        done: impl FnOnce(Outcome) + 'static,
    ) -> Rc<Overlay> {
        let settings = Settings::new();
        let hint = consume_hint(&settings);
        let overlay = Rc::new(Overlay {
            stills,
            windows,
            settings,
            state: RefCell::new(State {
                mode,
                pointer: None,
                anchor: None,
                selection: None,
                hovered: None,
                moving: false,
                space: false,
                snapped: (false, false),
                hint,
            }),
            surfaces: RefCell::new(Vec::new()),
            done: RefCell::new(Some(Box::new(done))),
        });
        for index in 0..overlay.stills.len() {
            overlay.add_surface(app, index);
        }
        overlay
    }

    fn add_surface(self: &Rc<Self>, app: &adw::Application, index: usize) {
        let still = &self.stills[index];
        let window = gtk::Window::builder().application(app).decorated(false).title("Shotput Selection").build();
        window.add_css_class("shotput-overlay");
        match layer::Surface::init(&window, "shotput-selection") {
            Some(surface) => {
                surface
                    .layer(layer::Layer::Overlay)
                    .monitor(&still.monitor)
                    .anchor(layer::Edge::Left, true)
                    .anchor(layer::Edge::Right, true)
                    .anchor(layer::Edge::Top, true)
                    .anchor(layer::Edge::Bottom, true)
                    .exclusive_zone(-1)
                    .keyboard(layer::Keyboard::Exclusive);
            }
            None => {
                window.set_default_size(still.geometry.width(), still.geometry.height());
                window.fullscreen_on_monitor(&still.monitor);
            }
        }
        let canvas = Canvas::new();
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        canvas.set_cursor_from_name(Some(if self.state.borrow().mode == Mode::Window { "default" } else { "crosshair" }));
        canvas.set_focusable(true);
        window.set_child(Some(&canvas));

        let origin = p(still.geometry.x() as f64, still.geometry.y() as f64);
        {
            let weak = Rc::downgrade(self);
            let widget = canvas.downgrade();
            canvas.set_draw(move |snapshot, w, h| {
                if let (Some(o), Some(c)) = (weak.upgrade(), widget.upgrade()) {
                    o.draw(index, &c, snapshot, w, h);
                }
            });
        }

        let motion = gtk::EventControllerMotion::new();
        {
            let weak = Rc::downgrade(self);
            motion.connect_enter(move |c, x, y| {
                if let Some(o) = weak.upgrade() {
                    let snap = !c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
                    o.on_motion(p(x, y) + origin, snap, c.current_event_state());
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            motion.connect_motion(move |c, x, y| {
                if let Some(o) = weak.upgrade() {
                    let state = c.current_event_state();
                    o.on_motion(p(x, y) + origin, !state.contains(gdk::ModifierType::CONTROL_MASK), state);
                }
            });
        }
        canvas.add_controller(motion);

        let click = gtk::GestureClick::new();
        click.set_button(0);
        {
            let weak = Rc::downgrade(self);
            click.connect_pressed(move |g, _, x, y| {
                if let Some(o) = weak.upgrade() {
                    let state = g.current_event_state();
                    o.on_press(g.current_button(), p(x, y) + origin, !state.contains(gdk::ModifierType::CONTROL_MASK));
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            click.connect_released(move |g, _, x, y| {
                if let Some(o) = weak.upgrade() {
                    o.on_release(g.current_button(), p(x, y) + origin);
                }
            });
        }
        canvas.add_controller(click);

        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, key, _, _| {
                if let Some(o) = weak.upgrade() {
                    o.on_key(key, true);
                }
                glib::Propagation::Stop
            });
        }
        {
            let weak = Rc::downgrade(self);
            keys.connect_key_released(move |_, key, _, _| {
                if let Some(o) = weak.upgrade() {
                    o.on_key(key, false);
                }
            });
        }
        window.add_controller(keys);

        window.present();
        canvas.grab_focus();
        self.surfaces.borrow_mut().push((window, canvas));
    }

    fn queue_draw(&self) {
        for (_, c) in self.surfaces.borrow().iter() {
            c.queue_draw();
        }
    }

    fn layout_bounds(&self) -> R {
        self.stills.iter().map(|s| rect_of(&s.geometry)).reduce(|a, b| a.union(&b)).unwrap_or_default()
    }

    fn monitor_at(&self, q: P) -> Option<&Still> {
        self.stills.iter().find(|s| rect_of(&s.geometry).contains(q))
    }

    pub fn finish(&self, outcome: Outcome) {
        let Some(done) = self.done.borrow_mut().take() else { return };
        for (w, _) in self.surfaces.borrow_mut().drain(..) {
            w.destroy();
        }
        done(outcome);
    }

    // MARK: Snapping

    /// Pulls the point onto nearby window and monitor edges. A window edge only counts where it is
    /// visible, so windows behind others do not make phantom snaps.
    fn snapped(&self, q: P, enabled: bool) -> (P, bool, bool) {
        let bounds = self.layout_bounds();
        let clamp = |v: P| p(v.x.clamp(bounds.x, bounds.max_x()), v.y.clamp(bounds.y, bounds.max_y()));
        if !enabled || !self.settings.boolean("snaps-to-edges", true) {
            return (clamp(p(q.x.round(), q.y.round())), false, false);
        }
        let (mut x, mut y) = (q.x.round(), q.y.round());
        let (mut best_x, mut best_y, mut hit_x, mut hit_y) = (SNAP_DISTANCE, SNAP_DISTANCE, false, false);
        let frontmost = |probe: P, index: usize| !self.windows[..index].iter().any(|w| w.rect.contains(probe));
        for (index, w) in self.windows.iter().enumerate() {
            let f = w.rect;
            if q.y >= f.y - SNAP_DISTANCE && q.y <= f.max_y() + SNAP_DISTANCE {
                let probe_y = q.y.clamp(f.y + 1.0, f.max_y() - 1.0);
                for (edge, inward) in [(f.x, 1.0), (f.max_x(), -1.0)] {
                    if (edge - q.x).abs() < best_x && frontmost(p(edge + inward, probe_y), index) {
                        best_x = (edge - q.x).abs();
                        x = edge;
                        hit_x = true;
                    }
                }
            }
            if q.x >= f.x - SNAP_DISTANCE && q.x <= f.max_x() + SNAP_DISTANCE {
                let probe_x = q.x.clamp(f.x + 1.0, f.max_x() - 1.0);
                for (edge, inward) in [(f.y, 1.0), (f.max_y(), -1.0)] {
                    if (edge - q.y).abs() < best_y && frontmost(p(probe_x, edge + inward), index) {
                        best_y = (edge - q.y).abs();
                        y = edge;
                        hit_y = true;
                    }
                }
            }
        }
        if let Some(still) = self.monitor_at(q) {
            let m = rect_of(&still.geometry);
            for edge in [m.x, m.max_x()] {
                if (edge - q.x).abs() < best_x {
                    best_x = (edge - q.x).abs();
                    x = edge;
                    hit_x = true;
                }
            }
            for edge in [m.y, m.max_y()] {
                if (edge - q.y).abs() < best_y {
                    best_y = (edge - q.y).abs();
                    y = edge;
                    hit_y = true;
                }
            }
        }
        (clamp(p(x, y)), hit_x, hit_y)
    }

    // MARK: Input

    fn on_motion(&self, raw: P, snapping: bool, modifiers: gdk::ModifierType) {
        let mut s = self.state.borrow_mut();
        if s.mode == Mode::Window {
            s.pointer = Some(raw);
            s.hovered = self.windows.iter().position(|w| w.rect.contains(raw));
            drop(s);
            self.queue_draw();
            return;
        }
        let previous = s.pointer;
        let (q, hx, hy) = self.snapped(raw, snapping);
        s.pointer = Some(q);
        s.snapped = (hx, hy);
        if let Some(anchor) = s.anchor {
            let bounds = self.layout_bounds();
            if s.moving && s.selection.is_some() {
                // Space held: slide the whole selection.
                let cur = s.selection.unwrap();
                let d = q - previous.unwrap_or(q);
                let nx = (cur.x + d.x).clamp(bounds.x, bounds.max_x() - cur.w);
                let ny = (cur.y + d.y).clamp(bounds.y, bounds.max_y() - cur.h);
                s.anchor = Some(p(anchor.x + nx - cur.x, anchor.y + ny - cur.y));
                s.selection = Some(R::new(nx, ny, cur.w, cur.h));
            } else {
                let mut end = q;
                if modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
                    let side = (end.x - anchor.x).abs().max((end.y - anchor.y).abs());
                    end = p(
                        (anchor.x + if end.x < anchor.x { -side } else { side }).clamp(bounds.x, bounds.max_x()),
                        (anchor.y + if end.y < anchor.y { -side } else { side }).clamp(bounds.y, bounds.max_y()),
                    );
                }
                s.selection = Some(R::from_points(anchor, end));
            }
        }
        drop(s);
        self.queue_draw();
    }

    fn on_press(&self, button: u32, at: P, snapping: bool) {
        if button == gdk::BUTTON_SECONDARY {
            self.finish(Outcome::Cancelled);
            return;
        }
        if button != gdk::BUTTON_PRIMARY {
            return;
        }
        let mode = self.state.borrow().mode;
        if mode != Mode::Area {
            return;
        }
        let (q, hx, hy) = self.snapped(at, snapping);
        let mut s = self.state.borrow_mut();
        s.pointer = Some(q);
        s.snapped = (hx, hy);
        s.anchor = Some(q);
        s.selection = Some(R::new(q.x, q.y, 0.0, 0.0));
        s.hint = false;
        drop(s);
        self.queue_draw();
    }

    fn on_release(&self, button: u32, _at: P) {
        if button != gdk::BUTTON_PRIMARY {
            return;
        }
        let mut s = self.state.borrow_mut();
        if s.mode == Mode::Window {
            let hovered = s.hovered.and_then(|i| self.windows.get(i).cloned());
            drop(s);
            if let Some(w) = hovered {
                self.finish(Outcome::Window(w));
            }
            return;
        }
        let selection = s.selection.take();
        s.anchor = None;
        s.moving = false;
        drop(s);
        match selection {
            Some(sel) if sel.w >= 4.0 && sel.h >= 4.0 => {
                let (x, y) = (sel.x.floor(), sel.y.floor());
                self.finish(Outcome::Area(R::new(x, y, (sel.max_x()).ceil() - x, (sel.max_y()).ceil() - y)));
            }
            _ => self.queue_draw(),
        }
    }

    fn on_key(&self, key: gdk::Key, down: bool) {
        if key == gdk::Key::Escape && down {
            self.finish(Outcome::Cancelled);
            return;
        }
        if key != gdk::Key::space {
            return;
        }
        let mut s = self.state.borrow_mut();
        if !down {
            s.space = false;
            s.moving = false;
            return;
        }
        if s.space {
            return;
        }
        s.space = true;
        if s.anchor.is_some() {
            s.moving = true;
        } else {
            s.mode = if s.mode == Mode::Area { Mode::Window } else { Mode::Area };
            s.hovered = s.pointer.and_then(|q| self.windows.iter().position(|w| w.rect.contains(q)));
            let cursor = if s.mode == Mode::Window { "default" } else { "crosshair" };
            drop(s);
            for (_, c) in self.surfaces.borrow().iter() {
                c.set_cursor_from_name(Some(cursor));
            }
        }
        self.queue_draw();
    }

    // MARK: Drawing

    fn draw(&self, index: usize, canvas: &Canvas, snapshot: &gtk::Snapshot, w: f64, h: f64) {
        let still = &self.stills[index];
        let monitor = rect_of(&still.geometry);
        let origin = p(monitor.x, monitor.y);
        let local = |r: R| r.offset(p(-origin.x, -origin.y));
        snapshot.append_texture(&still.texture, &grect(R::new(0.0, 0.0, w, h)));

        let s = self.state.borrow();
        let accent = accent();
        let window_cut = (s.mode == Mode::Window).then(|| s.hovered.and_then(|i| self.windows.get(i))).flatten();
        let cut = match (s.mode, window_cut) {
            (Mode::Window, Some(win)) => Some(win.rect),
            (Mode::Area, _) => s.selection.filter(|r| r.w > 0.0 && r.h > 0.0),
            _ => None,
        };

        // Dim everything but the cut.
        let dim = rgba(0.0, 0.0, 0.0, 0.3);
        match cut.map(local) {
            None => snapshot.append_color(&dim, &grect(R::new(0.0, 0.0, w, h))),
            Some(c) => {
                for r in [
                    R::new(0.0, 0.0, w, c.y),
                    R::new(0.0, c.max_y(), w, h - c.max_y()),
                    R::new(0.0, c.y, c.x, c.h),
                    R::new(c.max_x(), c.y, w - c.max_x(), c.h),
                ] {
                    if r.w > 0.0 && r.h > 0.0 {
                        snapshot.append_color(&dim, &grect(r));
                    }
                }
                if window_cut.is_some() {
                    let rounded = gsk::RoundedRect::from_rect(grect(c), 10.0);
                    let mut fill = accent;
                    fill.set_alpha(0.12);
                    snapshot.push_rounded_clip(&rounded);
                    snapshot.append_color(&fill, &grect(c));
                    snapshot.pop();
                    snapshot.append_border(&rounded, &[2.0; 4], &[accent; 4]);
                } else {
                    let shadow = rgba(0.0, 0.0, 0.0, 0.35);
                    snapshot.append_border(&gsk::RoundedRect::from_rect(grect(c.inset(-1.0, -1.0)), 0.0), &[1.0; 4], &[shadow; 4]);
                    snapshot.append_border(&gsk::RoundedRect::from_rect(grect(c), 0.0), &[1.0; 4], &[gdk::RGBA::WHITE; 4]);
                }
            }
        }

        // Guides and magnifier follow the pointer in area mode before a drag starts.
        let area = s.mode == Mode::Area;
        if let Some(q) = s.pointer {
            let lq = q - origin;
            if area && s.anchor.is_none() {
                let guide = |hit: bool| if hit { accent } else { rgba(1.0, 1.0, 1.0, 0.7) };
                if lq.x >= 0.0 && lq.x <= w {
                    snapshot.append_color(&guide(s.snapped.0), &grect(R::new(lq.x, 0.0, 1.0, h)));
                }
                if lq.y >= 0.0 && lq.y <= h {
                    snapshot.append_color(&guide(s.snapped.1), &grect(R::new(0.0, lq.y, w, 1.0)));
                }
            }
            if area && monitor.contains(q) && self.settings.boolean("shows-magnifier", true) {
                self.draw_loupe(snapshot, still, lq, w, h, accent);
            }
        }

        // Size badge, in real pixels.
        if let Some(c) = cut {
            let scale = self.monitor_at(c.center()).map(|st| st.scale()).unwrap_or(1.0);
            let dims = format!("{} × {}", (c.w * scale).round(), (c.h * scale).round());
            let text = match window_cut.and_then(|win| win.name.clone()) {
                Some(name) => format!("{name}  {dims}"),
                None => dims,
            };
            let lc = local(c);
            if lc.intersection(&R::new(-40.0, -40.0, w + 80.0, h + 80.0)).is_some() {
                self.draw_pill(canvas, snapshot, &text, 11.0, (7.0, 3.0), 6.0, |pw, ph| {
                    let mut x = lc.max_x() - pw;
                    let mut y = lc.max_y() + 8.0;
                    if y + ph > h - 4.0 {
                        y = lc.max_y() - ph - 8.0;
                        x -= 8.0;
                    }
                    p(x.clamp(4.0, (w - pw - 4.0).max(4.0)), y)
                });
            }
        }

        // First few times: what to do.
        if s.hint && s.anchor.is_none() {
            let show_here = s.pointer.map(|q| monitor.contains(q)).unwrap_or(index == 0);
            if show_here {
                let text = if s.mode == Mode::Area {
                    "Drag to capture an area. Space picks a window. Esc cancels."
                } else {
                    "Click a window to capture it. Space goes back to areas. Esc cancels."
                };
                self.draw_pill(canvas, snapshot, text, 13.0, (16.0, 8.0), 17.0, |pw, ph| p((w - pw) / 2.0, h - ph - 48.0));
            }
        }
    }

    fn draw_loupe(&self, snapshot: &gtk::Snapshot, still: &Still, lq: P, w: f64, h: f64, accent: gdk::RGBA) {
        let (size, gap) = (LOUPE_SIZE, 22.0);
        let mut cx = lq.x + gap + size / 2.0;
        let mut cy = lq.y + gap + size / 2.0;
        if cx + size / 2.0 > w - 8.0 {
            cx = lq.x - gap - size / 2.0;
        }
        if cy + size / 2.0 > h - 8.0 {
            cy = lq.y - gap - size / 2.0;
        }
        let frame = R::new((cx - size / 2.0).round(), (cy - size / 2.0).round(), size, size);
        let circle = gsk::RoundedRect::from_rect(grect(frame), (size / 2.0) as f32);
        snapshot.append_outset_shadow(&circle, &rgba(0.0, 0.0, 0.0, 0.35), 0.0, 4.0, 0.0, 10.0);
        snapshot.push_rounded_clip(&circle);
        // The still, scaled about the pointer so its pixel sits in the middle of the loupe.
        let zoomed = R::new(
            frame.mid_x() - lq.x * LOUPE_ZOOM - LOUPE_ZOOM / 2.0,
            frame.mid_y() - lq.y * LOUPE_ZOOM - LOUPE_ZOOM / 2.0,
            w * LOUPE_ZOOM,
            h * LOUPE_ZOOM,
        );
        snapshot.append_color(&gdk::RGBA::BLACK, &grect(frame));
        snapshot.append_scaled_texture(&still.texture, gsk::ScalingFilter::Nearest, &grect(zoomed));
        let reticle = R::new(
            (size / 2.0 - LOUPE_ZOOM / 2.0).floor() + frame.x,
            (size / 2.0 - LOUPE_ZOOM / 2.0).floor() + frame.y,
            LOUPE_ZOOM,
            LOUPE_ZOOM,
        );
        snapshot.append_border(&gsk::RoundedRect::from_rect(grect(reticle), 0.0), &[1.0; 4], &[accent; 4]);
        snapshot.pop();
        snapshot.append_border(&circle, &[1.5; 4], &[rgba(1.0, 1.0, 1.0, 0.92); 4]);
    }

    /// A dark rounded label, positioned by `place(width, height)`.
    #[allow(clippy::too_many_arguments)]
    fn draw_pill(
        &self,
        canvas: &Canvas,
        snapshot: &gtk::Snapshot,
        text: &str,
        size: f64,
        padding: (f64, f64),
        radius: f64,
        place: impl Fn(f64, f64) -> P,
    ) {
        let layout = canvas.create_pango_layout(Some(text));
        let mut font = gtk::pango::FontDescription::from_string("Sans Medium");
        font.set_absolute_size(size * gtk::pango::SCALE as f64);
        layout.set_font_description(Some(&font));
        let (tw, th) = layout.pixel_size();
        let (pw, ph) = (tw as f64 + padding.0 * 2.0, th as f64 + padding.1 * 2.0);
        let at = place(pw, ph);
        let box_ = R::new(at.x.round(), at.y.round(), pw, ph);
        let rounded = gsk::RoundedRect::from_rect(grect(box_), radius.min(ph / 2.0) as f32);
        snapshot.push_rounded_clip(&rounded);
        snapshot.append_color(&rgba(0.098, 0.098, 0.09, 0.82), &grect(box_));
        snapshot.pop();
        snapshot.save();
        snapshot.translate(&graphene::Point::new((box_.x + padding.0) as f32, (box_.y + padding.1) as f32));
        snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
        snapshot.restore();
    }

    /// Drives the overlay from a test, since a nested compositor has no real pointer.
    pub fn test_event(&self, kind: &str, x: f64, y: f64, modifiers: gdk::ModifierType) {
        let q = p(x, y);
        match kind {
            "move" => self.on_motion(q, !modifiers.contains(gdk::ModifierType::CONTROL_MASK), modifiers),
            "press" => self.on_press(gdk::BUTTON_PRIMARY, q, true),
            "release" => self.on_release(gdk::BUTTON_PRIMARY, q),
            "space" => {
                self.on_key(gdk::Key::space, true);
                self.on_key(gdk::Key::space, false);
            }
            "escape" => self.on_key(gdk::Key::Escape, true),
            _ => {}
        }
    }
}

fn consume_hint(settings: &Settings) -> bool {
    match settings.string("selection-hints", "first-few").as_str() {
        "always" => true,
        "never" => false,
        _ => {
            let shown = settings.int("hints-shown", 0);
            if shown >= 4 {
                return false;
            }
            settings.set_int("hints-shown", shown + 1);
            true
        }
    }
}

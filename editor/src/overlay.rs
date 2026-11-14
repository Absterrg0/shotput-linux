//! The selection overlay on desktops without the GNOME extension (KDE Plasma, Hyprland, Sway).
//! A port of the extension's overlay.js: one surface per monitor shows that monitor's frozen
//! still, and the selection, guides, magnifier and size badge are drawn over it in layout
//! coordinates, so a drag can cross monitors the same way the pointer does.
//!
//! Like overlay.js, every line is placed on the surface's physical pixel grid so it stays crisp
//! while it moves (also with fractional scaling), the dim and chrome fade in over the still
//! instead of snapping on, and on layer-shell desktops the overlay fades out over the live screen
//! when it closes instead of cutting to it.

use crate::canvas::Canvas;
use crate::grab::Still;
use crate::layer;
use crate::model::{P, R, p};
use crate::settings::Settings;
use gtk::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

const SNAP_DISTANCE: f64 = 7.0;
const LOUPE_SIZE: f64 = 116.0;
const LOUPE_ZOOM: f64 = 6.0;
const FADE_IN: Duration = Duration::from_millis(140);
const FADE_OUT: Duration = Duration::from_millis(170);
const CANCEL_FADE: Duration = Duration::from_millis(110);
const HINT_FADE: Duration = Duration::from_millis(120);
/// How close the pointer may come to the hint bar before it gets out of the way.
const HINT_CLEARANCE: f64 = 56.0;
const DIM: f32 = 0.32;

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
}

/// A value easing between 0 and 1 over time, for fades driven by the frame clock.
#[derive(Clone, Copy)]
struct Fade {
    from: f64,
    to: f64,
    start: Instant,
    length: Duration,
}

impl Fade {
    fn new(from: f64, to: f64, length: Duration) -> Fade {
        Fade { from, to, start: Instant::now(), length }
    }
    fn progress(&self, now: Instant) -> f64 {
        if self.length.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.start).as_secs_f64() / self.length.as_secs_f64()).clamp(0.0, 1.0)
    }
    /// Ease-out quad, like the extension's fades.
    fn value(&self, now: Instant) -> f64 {
        let t = self.progress(now);
        let e = 1.0 - (1.0 - t) * (1.0 - t);
        self.from + (self.to - self.from) * e
    }
    fn running(&self, now: Instant) -> bool {
        self.progress(now) < 1.0
    }
}

pub struct Overlay {
    stills: Vec<Still>,
    windows: Vec<WinInfo>,
    magnifier: bool,
    snaps: bool,
    accent: gdk::RGBA,
    hint: bool,
    state: RefCell<State>,
    surfaces: RefCell<Vec<(gtk::Window, Canvas)>>,
    done: RefCell<Option<Box<dyn FnOnce(Outcome)>>>,
    /// The dim and chrome fading in, then (once finished) everything fading out.
    appear: Cell<Fade>,
    leaving: Cell<Option<Fade>>,
    hint_fade: Cell<Fade>,
    hint_shown: Cell<bool>,
    ticking: Cell<bool>,
}

thread_local! {
    /// Overlays fading out. A new capture removes them first so they never end up in its still.
    static FADING: RefCell<Vec<gtk::Window>> = const { RefCell::new(Vec::new()) };
}

/// Removes any overlay still fading out, straight away. Returns true if there was one.
pub fn flush_fading() -> bool {
    let windows: Vec<gtk::Window> = FADING.with(|f| f.borrow_mut().drain(..).collect());
    let any = !windows.is_empty();
    for w in windows {
        w.destroy();
    }
    if any {
        // Make sure the compositor has unmapped them before anything grabs the screen.
        if let Some(display) = gdk::Display::default() {
            display.sync();
        }
    }
    any
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

/// Keycaps and what they do, by mode.
fn hints(mode: Mode) -> &'static [(&'static str, &'static str)] {
    match mode {
        Mode::Area => &[("Drag", "area"), ("Space", "window"), ("Enter", "full screen"), ("Esc", "cancel")],
        Mode::Window => &[("Click", "window"), ("Space", "area"), ("Enter", "this window"), ("Esc", "cancel")],
    }
}

/// Rounds to the physical pixel grid of a surface drawn at `scale`.
struct Grid(f64);

impl Grid {
    fn px(&self, v: f64) -> f64 {
        (v * self.0).round() / self.0
    }
    fn rect(&self, r: R) -> R {
        let (x, y) = (self.px(r.x), self.px(r.y));
        R::new(x, y, self.px(r.max_x()) - x, self.px(r.max_y()) - y)
    }
    /// One device pixel (two on a 2× screen), in surface units.
    fn line(&self) -> f64 {
        self.0.round().max(1.0) / self.0
    }
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
            magnifier: settings.boolean("shows-magnifier", true),
            snaps: settings.boolean("snaps-to-edges", true),
            accent: accent(),
            hint,
            state: RefCell::new(State {
                mode,
                pointer: None,
                anchor: None,
                selection: None,
                hovered: None,
                moving: false,
                space: false,
                snapped: (false, false),
            }),
            surfaces: RefCell::new(Vec::new()),
            done: RefCell::new(Some(Box::new(done))),
            appear: Cell::new(Fade::new(0.0, 1.0, FADE_IN)),
            leaving: Cell::new(None),
            hint_fade: Cell::new(Fade::new(0.0, 1.0, FADE_IN + Duration::from_millis(80))),
            hint_shown: Cell::new(true),
            ticking: Cell::new(false),
        });
        for index in 0..overlay.stills.len() {
            overlay.add_surface(app, index);
        }
        // The still matches the live screen, so only the dim and chrome need to appear.
        overlay.appear.set(Fade::new(0.0, 1.0, FADE_IN));
        overlay.animate();
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
                    let state = c.current_event_state();
                    o.on_motion(p(x, y) + origin, !state.contains(gdk::ModifierType::CONTROL_MASK), state);
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

    /// Redraws every frame while a fade runs.
    fn animate(self: &Rc<Self>) {
        if self.ticking.replace(true) {
            return;
        }
        for (_, canvas) in self.surfaces.borrow().iter() {
            let weak = Rc::downgrade(self);
            canvas.add_tick_callback(move |c, _| {
                c.queue_draw();
                match weak.upgrade() {
                    Some(o) if o.animating() => glib::ControlFlow::Continue,
                    Some(o) => {
                        o.ticking.set(false);
                        glib::ControlFlow::Break
                    }
                    None => glib::ControlFlow::Break,
                }
            });
        }
    }

    fn animating(&self) -> bool {
        let now = Instant::now();
        self.appear.get().running(now) || self.hint_fade.get().running(now) || self.leaving.get().is_some_and(|f| f.running(now))
    }

    fn layout_bounds(&self) -> R {
        self.stills.iter().map(|s| rect_of(&s.geometry)).reduce(|a, b| a.union(&b)).unwrap_or_default()
    }

    fn monitor_at(&self, q: P) -> Option<&Still> {
        self.stills.iter().find(|s| rect_of(&s.geometry).contains(q))
    }

    pub fn finish(self: &Rc<Self>, outcome: Outcome) {
        let Some(done) = self.done.borrow_mut().take() else { return };
        let captured = !matches!(outcome, Outcome::Cancelled);
        let surfaces: Vec<(gtk::Window, Canvas)> = self.surfaces.borrow_mut().drain(..).collect();
        if layer::available() && !surfaces.is_empty() {
            // Input goes back to the desktop at once; the still fades out over the live screen,
            // so closing never cuts. Fullscreen windows have their own animations, so they just go.
            let fade = Fade::new(1.0, 0.0, if captured { FADE_OUT } else { CANCEL_FADE });
            self.leaving.set(Some(fade));
            for (window, canvas) in &surfaces {
                layer::set_keyboard(window, layer::Keyboard::None);
                if let Some(surface) = window.surface() {
                    surface.set_input_region(Some(&gtk::cairo::Region::create()));
                }
                canvas.set_cursor_from_name(None);
                FADING.with(|f| f.borrow_mut().push(window.clone()));
                let weak = Rc::downgrade(self);
                let window = window.downgrade();
                canvas.add_tick_callback(move |c, _| {
                    c.queue_draw();
                    let running = weak.upgrade().and_then(|o| o.leaving.get()).is_some_and(|f| f.running(Instant::now()));
                    if running {
                        return glib::ControlFlow::Continue;
                    }
                    if let Some(w) = window.upgrade() {
                        FADING.with(|f| f.borrow_mut().retain(|x| x != &w));
                        w.destroy();
                    }
                    glib::ControlFlow::Break
                });
            }
            // The tick callbacks hold the overlay only weakly; keep it alive for the fade.
            let keep = self.clone();
            glib::timeout_add_local_once(FADE_OUT + Duration::from_millis(200), move || drop(keep));
        } else {
            for (w, _) in surfaces {
                w.destroy();
            }
        }
        done(outcome);
    }

    // MARK: Snapping

    /// Pulls the point onto nearby window and monitor edges. A window edge only counts where it is
    /// visible, so windows behind others do not make phantom snaps.
    fn snapped(&self, q: P, enabled: bool) -> (P, bool, bool) {
        let bounds = self.layout_bounds();
        let clamp = |v: P| p(v.x.clamp(bounds.x, bounds.max_x()), v.y.clamp(bounds.y, bounds.max_y()));
        if !enabled || !self.snaps {
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

    fn live(&self) -> bool {
        self.done.borrow().is_some()
    }

    fn on_motion(&self, raw: P, snapping: bool, modifiers: gdk::ModifierType) {
        if !self.live() {
            return;
        }
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

    fn on_press(self: &Rc<Self>, button: u32, at: P, snapping: bool) {
        if !self.live() {
            return;
        }
        if button == gdk::BUTTON_SECONDARY {
            // A right click backs out of a drag first, then out of the overlay.
            if self.state.borrow().anchor.is_some() {
                self.reset_drag();
            } else {
                self.finish(Outcome::Cancelled);
            }
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
        drop(s);
        self.queue_draw();
    }

    fn reset_drag(&self) {
        let mut s = self.state.borrow_mut();
        s.anchor = None;
        s.selection = None;
        s.moving = false;
        drop(s);
        self.queue_draw();
    }

    fn on_release(self: &Rc<Self>, button: u32, _at: P) {
        if button != gdk::BUTTON_PRIMARY || !self.live() {
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
        if s.anchor.is_none() {
            return;
        }
        let selection = s.selection.take();
        s.anchor = None;
        s.moving = false;
        drop(s);
        match selection {
            Some(sel) if sel.w >= 4.0 && sel.h >= 4.0 => self.finish(Outcome::Area(capture_rect(sel))),
            // A click without a drag: stay, ready for the next try.
            _ => self.queue_draw(),
        }
    }

    fn on_key(self: &Rc<Self>, key: gdk::Key, down: bool) {
        if !self.live() {
            return;
        }
        if down && key == gdk::Key::Escape {
            self.finish(Outcome::Cancelled);
            return;
        }
        if down && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter) {
            let s = self.state.borrow();
            let outcome = match s.mode {
                Mode::Window => s.hovered.and_then(|i| self.windows.get(i).cloned()).map(Outcome::Window),
                Mode::Area => match (s.anchor, s.selection) {
                    (Some(_), Some(sel)) if sel.w >= 4.0 && sel.h >= 4.0 => Some(Outcome::Area(capture_rect(sel))),
                    // Nothing dragged: the whole monitor under the pointer.
                    _ => s
                        .pointer
                        .and_then(|q| self.monitor_at(q))
                        .or(self.stills.first())
                        .map(|still| Outcome::Area(rect_of(&still.geometry))),
                },
            };
            drop(s);
            if let Some(outcome) = outcome {
                self.finish(outcome);
            }
            return;
        }
        if down && matches!(key, gdk::Key::Tab | gdk::Key::ISO_Left_Tab) {
            if self.state.borrow().anchor.is_none() {
                self.toggle_mode();
            }
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
            drop(s);
        } else {
            drop(s);
            self.toggle_mode();
        }
        self.queue_draw();
    }

    fn toggle_mode(&self) {
        let mut s = self.state.borrow_mut();
        s.mode = if s.mode == Mode::Area { Mode::Window } else { Mode::Area };
        s.anchor = None;
        s.selection = None;
        s.moving = false;
        s.hovered = s.pointer.and_then(|q| self.windows.iter().position(|w| w.rect.contains(q)));
        let cursor = if s.mode == Mode::Window { "default" } else { "crosshair" };
        drop(s);
        for (_, c) in self.surfaces.borrow().iter() {
            c.set_cursor_from_name(Some(cursor));
        }
        self.queue_draw();
    }

    // MARK: Drawing

    fn draw(self: &Rc<Self>, index: usize, canvas: &Canvas, snapshot: &gtk::Snapshot, w: f64, h: f64) {
        let now = Instant::now();
        let still = &self.stills[index];
        let monitor = rect_of(&still.geometry);
        let origin = p(monitor.x, monitor.y);
        let local = |r: R| r.offset(p(-origin.x, -origin.y));
        let scale = canvas.native().and_then(|n| n.surface()).map(|s| s.scale()).filter(|s| *s > 0.0).unwrap_or(1.0);
        let grid = Grid(scale);
        let t = grid.line();

        let leaving = self.leaving.get().map(|f| f.value(now));
        if let Some(opacity) = leaving {
            snapshot.push_opacity(opacity);
        }
        snapshot.append_texture(&still.texture, &grect(R::new(0.0, 0.0, w, h)));

        let s = self.state.borrow();
        let accent = self.accent;
        let window_cut = (s.mode == Mode::Window).then(|| s.hovered.and_then(|i| self.windows.get(i))).flatten();
        let cut = match (s.mode, window_cut) {
            (Mode::Window, Some(win)) => Some(win.rect),
            (Mode::Area, _) => s.selection.filter(|r| r.w > 0.0 && r.h > 0.0),
            _ => None,
        };

        // Everything over the still fades in together.
        snapshot.push_opacity(self.appear.get().value(now));

        // Dim everything but the cut.
        let dim = rgba(0.0, 0.0, 0.0, DIM);
        let lc = cut.map(|c| grid.rect(local(c)));
        match lc {
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
                    // A white line just outside the selection, and a soft dark one outside that, so
                    // every captured pixel stays visible and the frame reads on any content.
                    let light = rgba(1.0, 1.0, 1.0, 0.95);
                    let dark = rgba(0.0, 0.0, 0.0, 0.28);
                    let outer = c.inset(-t, -t);
                    snapshot.append_border(&gsk::RoundedRect::from_rect(grect(outer.inset(-t, -t)), 0.0), &[t as f32; 4], &[dark; 4]);
                    snapshot.append_border(&gsk::RoundedRect::from_rect(grect(outer), 0.0), &[t as f32; 4], &[light; 4]);
                }
            }
        }

        // Guides and magnifier follow the pointer in area mode before a drag starts.
        let area = s.mode == Mode::Area;
        let mut pointer_local = None;
        if let Some(q) = s.pointer {
            let lq = q - origin;
            pointer_local = Some(lq);
            if area && s.anchor.is_none() && self.leaving.get().is_none() {
                let guide = |hit: bool| if hit { accent } else { rgba(1.0, 1.0, 1.0, 0.6) };
                if lq.x >= 0.0 && lq.x <= w {
                    snapshot.append_color(&guide(s.snapped.0), &grect(R::new(grid.px(lq.x), 0.0, t, h)));
                }
                if lq.y >= 0.0 && lq.y <= h {
                    snapshot.append_color(&guide(s.snapped.1), &grect(R::new(0.0, grid.px(lq.y), w, t)));
                }
            }
            if area && monitor.contains(q) && self.magnifier && self.leaving.get().is_none() {
                self.draw_loupe(snapshot, still, lq, w, h, accent);
            }
        }

        // Size badge, in real pixels.
        let mut badge_box = None;
        if let (Some(c), Some(lc)) = (cut, lc) {
            if self.leaving.get().is_none() {
                let px_scale = self.monitor_at(c.center()).map(|st| st.scale()).unwrap_or(1.0);
                let dims = format!("{} × {}", (c.w * px_scale).round(), (c.h * px_scale).round());
                let text = match window_cut.and_then(|win| win.name.clone()) {
                    Some(name) => format!("{name}  ·  {dims}"),
                    None => dims,
                };
                if lc.intersection(&R::new(-40.0, -40.0, w + 80.0, h + 80.0)).is_some() {
                    badge_box = Some(self.draw_pill(canvas, snapshot, &text, 11.0, (8.0, 3.0), 6.0, |pw, ph| {
                        let mut x = lc.max_x() - pw;
                        let mut y = lc.max_y() + 8.0;
                        if y + ph > h - 4.0 {
                            y = lc.max_y() - ph - 8.0;
                            x -= 8.0;
                        }
                        p(x.clamp(4.0, (w - pw - 4.0).max(4.0)), y.max(4.0))
                    }));
                }
            }
        }

        // The first few times: the keys, out of the way of the pointer and the badge.
        let mut hint = None;
        if self.hint && self.leaving.get().is_none() {
            let show_here = s.pointer.map(|q| monitor.contains(q)).unwrap_or(index == 0);
            if show_here {
                let items = hints(s.mode);
                let (hw, hh) = self.hint_size(canvas, items);
                let hint_box = R::new(((w - hw) / 2.0).round(), (h - hh - 48.0).round(), hw, hh);
                let near = pointer_local.is_some_and(|lq| hint_box.inset(-HINT_CLEARANCE, -HINT_CLEARANCE).contains(lq));
                let covered = badge_box.is_some_and(|b: R| b.intersection(&hint_box.inset(-8.0, -8.0)).is_some());
                hint = Some((items, hint_box, s.anchor.is_none() && !near && !covered));
            }
        }
        drop(s);
        if let Some((items, hint_box, show)) = hint {
            if show != self.hint_shown.get() {
                self.hint_shown.set(show);
                let from = self.hint_fade.get().value(now);
                self.hint_fade.set(Fade::new(from, if show { 1.0 } else { 0.0 }, HINT_FADE));
                self.animate();
            }
            self.draw_hint(canvas, snapshot, items, hint_box, self.hint_fade.get().value(now));
        }

        snapshot.pop();
        if leaving.is_some() {
            snapshot.pop();
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
        snapshot.append_outset_shadow(&circle, &rgba(0.0, 0.0, 0.0, 0.4), 0.0, 4.0, 0.0, 12.0);
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

    fn layout(canvas: &Canvas, text: &str, size: f64, bold: bool) -> gtk::pango::Layout {
        let layout = canvas.create_pango_layout(Some(text));
        let mut font = gtk::pango::FontDescription::from_string(if bold { "Sans Semi-Bold" } else { "Sans Medium" });
        font.set_absolute_size(size * gtk::pango::SCALE as f64);
        layout.set_font_description(Some(&font));
        layout
    }

    /// A dark rounded label, positioned by `place(width, height)`. Returns where it went.
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
    ) -> R {
        let layout = Self::layout(canvas, text, size, true);
        let (tw, th) = layout.pixel_size();
        let (pw, ph) = (tw as f64 + padding.0 * 2.0, th as f64 + padding.1 * 2.0);
        let at = place(pw, ph);
        let box_ = R::new(at.x.round(), at.y.round(), pw, ph);
        let rounded = gsk::RoundedRect::from_rect(grect(box_), radius.min(ph / 2.0) as f32);
        snapshot.push_rounded_clip(&rounded);
        snapshot.append_color(&rgba(0.098, 0.098, 0.09, 0.86), &grect(box_));
        snapshot.pop();
        snapshot.save();
        snapshot.translate(&graphene::Point::new((box_.x + padding.0) as f32, (box_.y + padding.1) as f32));
        snapshot.append_layout(&layout, &gdk::RGBA::WHITE);
        snapshot.restore();
        box_
    }

    const HINT_PAD: (f64, f64) = (14.0, 7.0);
    const HINT_GAP: f64 = 14.0;
    const KEY_GAP: f64 = 6.0;
    const KEY_PAD: (f64, f64) = (6.0, 1.0);

    fn hint_size(&self, canvas: &Canvas, items: &[(&str, &str)]) -> (f64, f64) {
        let mut width = Self::HINT_PAD.0 * 2.0 + Self::HINT_GAP * (items.len().saturating_sub(1)) as f64;
        let mut height: f64 = 0.0;
        for (key, what) in items {
            let (kw, kh) = Self::layout(canvas, key, 11.0, true).pixel_size();
            let (tw, th) = Self::layout(canvas, what, 12.0, false).pixel_size();
            width += kw as f64 + Self::KEY_PAD.0 * 2.0 + Self::KEY_GAP + tw as f64;
            height = height.max(kh as f64 + Self::KEY_PAD.1 * 2.0 + 2.0).max(th as f64);
        }
        (width.round(), (height + Self::HINT_PAD.1 * 2.0).round())
    }

    fn draw_hint(&self, canvas: &Canvas, snapshot: &gtk::Snapshot, items: &[(&str, &str)], at: R, opacity: f64) {
        if opacity <= 0.001 {
            return;
        }
        snapshot.push_opacity(opacity);
        let pill = gsk::RoundedRect::from_rect(grect(at), (at.h / 2.0) as f32);
        snapshot.append_outset_shadow(&pill, &rgba(0.0, 0.0, 0.0, 0.3), 0.0, 4.0, 0.0, 16.0);
        snapshot.push_rounded_clip(&pill);
        snapshot.append_color(&rgba(0.098, 0.098, 0.09, 0.86), &grect(at));
        snapshot.pop();
        let mut x = at.x + Self::HINT_PAD.0;
        let mid = at.mid_y();
        for (key, what) in items {
            let key_layout = Self::layout(canvas, key, 11.0, true);
            let (kw, kh) = key_layout.pixel_size();
            let cap = R::new(x, (mid - (kh as f64 + Self::KEY_PAD.1 * 2.0 + 2.0) / 2.0).round(), kw as f64 + Self::KEY_PAD.0 * 2.0, kh as f64 + Self::KEY_PAD.1 * 2.0 + 2.0);
            let rounded = gsk::RoundedRect::from_rect(grect(cap), 5.0);
            snapshot.push_rounded_clip(&rounded);
            snapshot.append_color(&rgba(1.0, 1.0, 1.0, 0.14), &grect(cap));
            snapshot.pop();
            snapshot.append_border(&rounded, &[1.0; 4], &[rgba(1.0, 1.0, 1.0, 0.18); 4]);
            snapshot.save();
            snapshot.translate(&graphene::Point::new((cap.x + Self::KEY_PAD.0) as f32, (cap.y + Self::KEY_PAD.1 + 1.0) as f32));
            snapshot.append_layout(&key_layout, &gdk::RGBA::WHITE);
            snapshot.restore();
            x = cap.max_x() + Self::KEY_GAP;

            let text_layout = Self::layout(canvas, what, 12.0, false);
            let (tw, th) = text_layout.pixel_size();
            snapshot.save();
            snapshot.translate(&graphene::Point::new(x as f32, (mid - th as f64 / 2.0).round() as f32));
            snapshot.append_layout(&text_layout, &rgba(1.0, 1.0, 1.0, 0.78));
            snapshot.restore();
            x += tw as f64 + Self::HINT_GAP;
        }
        snapshot.pop();
    }

    /// Drives the overlay from a test, since a nested compositor has no real pointer.
    pub fn test_event(self: &Rc<Self>, kind: &str, x: f64, y: f64, modifiers: gdk::ModifierType) {
        let q = p(x, y);
        match kind {
            "move" => self.on_motion(q, !modifiers.contains(gdk::ModifierType::CONTROL_MASK), modifiers),
            "press" => self.on_press(gdk::BUTTON_PRIMARY, q, true),
            "release" => self.on_release(gdk::BUTTON_PRIMARY, q),
            "rightclick" => self.on_press(gdk::BUTTON_SECONDARY, q, true),
            "space" => {
                self.on_key(gdk::Key::space, true);
                self.on_key(gdk::Key::space, false);
            }
            "tab" => self.on_key(gdk::Key::Tab, true),
            "enter" => self.on_key(gdk::Key::Return, true),
            "escape" => self.on_key(gdk::Key::Escape, true),
            _ => {}
        }
    }
}

/// The selection as whole layout pixels, covering everything inside the frame.
fn capture_rect(sel: R) -> R {
    let (x, y) = (sel.x.floor(), sel.y.floor());
    R::new(x, y, sel.max_x().ceil() - x, sel.max_y().ceil() - y)
}

fn consume_hint(settings: &Settings) -> bool {
    match settings.string("selection-hints", "first-few").as_str() {
        "always" => true,
        "never" => false,
        _ => {
            let shown = settings.int("hints-shown", 0);
            if shown >= 8 {
                return false;
            }
            settings.set_int("hints-shown", shown + 1);
            true
        }
    }
}

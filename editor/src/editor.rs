//! The editor: its document model (EditorModel.swift), canvas input (CanvasView.swift),
//! window and toolbar (EditorWindowController.swift), style bar (StyleBar.swift) and the
//! background popover (BackdropPicker.swift), ported to GTK 4 and libadwaita.

use crate::backdrop::{self, Fill, Style};
use crate::canvas::Canvas;
use crate::model::*;
use crate::output;
use crate::render::{self, grect, BaseImage, Renderer, View};
use crate::segmented::Segmented;
use crate::settings::Settings;
use adw::prelude::*;
use gtk::{gdk, gio, glib, gsk};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

const MARGIN: f64 = 40.0;
/// Space kept clear under the image for the floating style bar.
const BOTTOM_RESERVE: f64 = 68.0;
const MIN_CONTENT: (f64, f64) = (760.0, 460.0);

/// One screenshot and where it came from.
pub struct CaptureInfo {
    /// Set when the capture came from the Shell extension, which wants the edited result back.
    pub id: Option<String>,
    pub path: Option<PathBuf>,
    /// Pixels per point.
    pub scale: f64,
    /// Size of the monitor the capture was taken on, in points.
    pub monitor: Option<(f64, f64)>,
    pub app_name: Option<String>,
    pub date: glib::DateTime,
    /// The unframed image and its backdrop, when the capture was framed as it was taken.
    pub framed: Option<(gdk::Texture, Style)>,
}

// MARK: Model

#[derive(Clone)]
struct Applied {
    plain: BaseImage,
    style: Style,
}

struct Snap {
    base: BaseImage,
    annotations: Vec<Annotation>,
    backdrop: Option<Applied>,
}

struct Model {
    scale: f64,
    base: BaseImage,
    annotations: Vec<Annotation>,
    selected: Option<u64>,
    tool: Tool,
    color: Ink,
    weight: Weight,
    filled: bool,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    gesture_pending: bool,
    gesture_checkpointed: bool,
    fill_by_tool: HashMap<Tool, bool>,
    backdrop: Option<Applied>,
}

impl Model {
    fn size(&self) -> (f64, f64) {
        (self.base.width() as f64 / self.scale, self.base.height() as f64 / self.scale)
    }
    fn image_bounds(&self) -> R {
        let (w, h) = self.size();
        R::new(0.0, 0.0, w, h)
    }
    fn selected(&self) -> Option<&Annotation> {
        let id = self.selected?;
        self.annotations.iter().find(|a| a.id == id)
    }
    /// The tool whose style controls apply right now.
    fn style_tool(&self) -> Tool {
        self.selected().map(|a| a.tool()).unwrap_or(self.tool)
    }
    fn next_counter(&self) -> i32 {
        self.annotations.iter().filter_map(|a| if let Kind::Counter(_, n) = a.kind { Some(n) } else { None }).max().unwrap_or(0) + 1
    }
    fn make(&self, kind: Kind) -> Annotation {
        let mut a = Annotation::new(kind, self.color, self.weight, false);
        a.filled = a.tool().supports_fill() && *self.fill_by_tool.get(&a.tool()).unwrap_or(&false);
        a
    }

    /// Call at the start of a gesture. The first `update` after it records one undo step.
    fn begin_gesture(&mut self) {
        self.gesture_pending = true;
        self.gesture_checkpointed = false;
    }
    fn update(&mut self, body: impl FnOnce(&mut Vec<Annotation>)) {
        if self.gesture_pending {
            self.checkpoint();
            self.gesture_pending = false;
            self.gesture_checkpointed = true;
        }
        body(&mut self.annotations);
    }
    fn perform(&mut self, body: impl FnOnce(&mut Vec<Annotation>)) {
        self.begin_gesture();
        self.update(body);
    }
    fn replace(&mut self, id: u64, f: impl FnOnce(&Annotation) -> Annotation) {
        self.update(|list| {
            if let Some(i) = list.iter().position(|a| a.id == id) {
                list[i] = f(&list[i]);
            }
        });
    }
    /// Rolls back a gesture that turned out to be a no-op, such as a click with a shape tool.
    fn discard_gesture(&mut self) {
        self.gesture_pending = false;
        if !self.gesture_checkpointed {
            return;
        }
        if let Some(snap) = self.undo.pop() {
            self.gesture_checkpointed = false;
            self.annotations = snap.annotations;
        }
    }
    fn checkpoint(&mut self) {
        self.undo.push(Snap { base: self.base.clone(), annotations: self.annotations.clone(), backdrop: self.backdrop.clone() });
        self.redo.clear();
    }
    fn undo(&mut self) {
        if let Some(snap) = self.undo.pop() {
            self.redo.push(Snap { base: self.base.clone(), annotations: self.annotations.clone(), backdrop: self.backdrop.clone() });
            self.restore(snap);
        }
    }
    fn redo(&mut self) {
        if let Some(snap) = self.redo.pop() {
            self.undo.push(Snap { base: self.base.clone(), annotations: self.annotations.clone(), backdrop: self.backdrop.clone() });
            self.restore(snap);
        }
    }
    fn restore(&mut self, snap: Snap) {
        self.base = snap.base;
        self.annotations = snap.annotations;
        self.backdrop = snap.backdrop;
        self.selected = None;
    }
    fn delete_selected(&mut self) {
        if let Some(id) = self.selected.take() {
            self.perform(|list| list.retain(|a| a.id != id));
        }
    }
    fn crop(&mut self, rect: R) {
        let px = R::new(rect.x * self.scale, rect.y * self.scale, rect.w * self.scale, rect.h * self.scale).integral();
        let Some(cropped) = crop_texture(&self.base, px) else { return };
        let scale = self.scale;
        self.begin_gesture();
        self.update(|list| {
            let shift = p(-px.x / scale, -px.y / scale);
            *list = list.iter().map(|a| a.translated(shift)).collect();
        });
        self.base = BaseImage::new(cropped);
        // The crop now owns the pixels, backdrop and all, so there is nothing left to swap.
        self.backdrop = None;
        self.selected = None;
    }
    /// Puts the image on a backdrop, swaps the current one, or removes it with None. Marks keep
    /// their place on the screenshot, and each change is one undo step.
    fn set_backdrop(&mut self, style: Option<Style>, wallpaper: Option<&gdk::Texture>, coalesce: bool) {
        let plain = self.backdrop.as_ref().map(|b| b.plain.clone()).unwrap_or_else(|| self.base.clone());
        let old_padding = self.backdrop.as_ref().map(|b| b.style.padding).unwrap_or(0.0);
        let new_padding = style.map(|s| s.padding).unwrap_or(0.0);
        let composed = match style {
            Some(style) => match backdrop::compose(&plain.texture, self.scale, style, wallpaper, true) {
                Some(t) => Some((BaseImage::new(t), style)),
                None => return,
            },
            None => None,
        };
        if !coalesce {
            self.checkpoint();
        }
        let shift = p(new_padding - old_padding, new_padding - old_padding);
        self.annotations = self.annotations.iter().map(|a| a.translated(shift)).collect();
        match composed {
            Some((base, style)) => {
                self.base = base;
                self.backdrop = Some(Applied { plain, style });
            }
            None => {
                self.base = plain;
                self.backdrop = None;
            }
        }
        self.selected = None;
    }
    fn set_selected(&mut self, id: Option<u64>) {
        self.selected = id;
        if let Some(a) = self.selected().cloned() {
            self.color = a.color;
            self.weight = a.weight;
            if a.tool().supports_fill() {
                self.filled = a.filled;
            }
        }
    }
}

fn crop_texture(base: &BaseImage, px: R) -> Option<gdk::Texture> {
    let px = px.intersection(&R::new(0.0, 0.0, base.width() as f64, base.height() as f64))?;
    let (x, y, w, h) = (px.x as usize, px.y as usize, px.w as usize, px.h as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let pixels = base.pixels();
    let (data, stride) = (&pixels.0, pixels.1);
    let mut out = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        out.extend_from_slice(&data[row * stride + x * 4..row * stride + (x + w) * 4]);
    }
    Some(
        gdk::MemoryTexture::new(w as i32, h as i32, gdk::MemoryFormat::R8g8b8a8Premultiplied, &glib::Bytes::from_owned(out), w * 4)
            .upcast(),
    )
}

// MARK: Editor

enum Drag {
    None,
    Move { id: u64, last: P },
    Rect { id: u64, anchor: P, creating: bool },
    Endpoint { id: u64, moves_start: bool, creating: bool },
    Bend { id: u64 },
    Pen { id: u64 },
    Crop { anchor: P },
    Ignore,
}

struct TextEdit {
    id: Option<u64>,
    origin: P,
    weight: Weight,
    color: Ink,
    filled: bool,
    view: gtk::TextView,
}

struct Ui {
    tools: Vec<gtk::ToggleButton>,
    swatches: Vec<gtk::Button>,
    weights: Vec<gtk::Button>,
    fill: gtk::Button,
    undo: gtk::Button,
    redo: gtk::Button,
    toast: gtk::Label,
    toast_revealer: gtk::Revealer,
    copy: gtk::Button,
    save: gtk::Button,
    read_text: gtk::Button,
    bg_choice: Segmented,
    bg_color: gtk::ColorDialogButton,
    bg_color_row: gtk::Box,
    bg_padding: gtk::Scale,
    bg_padding_label: gtk::Label,
    bg_padding_row: gtk::Box,
}

pub struct Editor {
    pub window: adw::ApplicationWindow,
    canvas: Canvas,
    overlay: gtk::Overlay,
    settings: Settings,
    capture: RefCell<CaptureInfo>,
    model: RefCell<Model>,
    renderer: Renderer,
    view: Cell<View>,
    fits: Cell<bool>,
    zoom: Cell<f64>,
    pan: Cell<(f64, f64)>,
    drag: RefCell<Drag>,
    drag_start: Cell<(f64, f64)>,
    crop_rect: Cell<Option<R>>,
    clicked_area_mark: Cell<Option<u64>>,
    text: RefCell<Option<TextEdit>>,
    text_css: gtk::CssProvider,
    text_class: String,
    last_click: Cell<(i64, f64, f64, u32)>,
    ui: RefCell<Option<Ui>>,
    syncing: Cell<bool>,
    wallpaper: RefCell<Option<Option<gdk::Texture>>>,
    toast_generation: Rc<Cell<u64>>,
    last_padding_change: Cell<i64>,
    pinch_start: Cell<f64>,
}

thread_local! {
    static EDITORS: RefCell<Vec<Rc<Editor>>> = const { RefCell::new(Vec::new()) };
}

static EDITOR_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Editor {
    pub fn open(app: &adw::Application, texture: gdk::Texture, capture: CaptureInfo) -> Rc<Editor> {
        let settings = Settings::new();
        let stored_tool = Tool::from_index(settings.int("editor-tool", 0));
        // Reopening on crop would make the first drag destructive.
        let tool = if stored_tool == Tool::Crop { Tool::Arrow } else { stored_tool };
        let mut fill_by_tool = HashMap::new();
        fill_by_tool.insert(Tool::Text, true);
        let mut model = Model {
            scale: capture.scale,
            base: BaseImage::new(texture),
            annotations: Vec::new(),
            selected: None,
            tool,
            color: Ink::from_index(settings.int("editor-color", 0)),
            weight: Weight::from_index(settings.int("editor-weight", 1)),
            filled: tool == Tool::Text,
            undo: Vec::new(),
            redo: Vec::new(),
            gesture_pending: false,
            gesture_checkpointed: false,
            fill_by_tool,
            backdrop: None,
        };
        if let Some((plain, style)) = &capture.framed {
            model.backdrop = Some(Applied { plain: BaseImage::new(plain.clone()), style: *style });
        }

        let window = adw::ApplicationWindow::builder().application(app).title("Shotput").build();
        window.add_css_class("shotput-editor");
        let canvas = Canvas::new();
        canvas.add_css_class("shotput-canvas");
        canvas.set_cursor_from_name(Some("crosshair"));
        let style = adw::StyleManager::default();
        if style.is_dark() {
            canvas.add_css_class("dark");
        }
        {
            let canvas = canvas.downgrade();
            style.connect_dark_notify(move |s| {
                if let Some(c) = canvas.upgrade() {
                    if s.is_dark() { c.add_css_class("dark") } else { c.remove_css_class("dark") }
                }
            });
        }
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&canvas));

        let serial = EDITOR_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let text_css = gtk::CssProvider::new();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &text_css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        }

        let editor = Rc::new(Editor {
            window,
            canvas,
            overlay,
            settings,
            capture: RefCell::new(capture),
            model: RefCell::new(model),
            renderer: Renderer::default(),
            view: Cell::new(View { ox: 0.0, oy: 0.0, zoom: 1.0 }),
            fits: Cell::new(true),
            zoom: Cell::new(1.0),
            pan: Cell::new((0.0, 0.0)),
            drag: RefCell::new(Drag::None),
            drag_start: Cell::new((0.0, 0.0)),
            crop_rect: Cell::new(None),
            clicked_area_mark: Cell::new(None),
            text: RefCell::new(None),
            text_css,
            text_class: format!("shotput-text-e{serial}"),
            last_click: Cell::new((0, 0.0, 0.0, 0)),
            ui: RefCell::new(None),
            syncing: Cell::new(false),
            wallpaper: RefCell::new(None),
            toast_generation: Rc::new(Cell::new(0)),
            last_padding_change: Cell::new(0),
            pinch_start: Cell::new(1.0),
        });
        editor.build();
        editor.lay_out();
        EDITORS.with(|list| list.borrow_mut().push(editor.clone()));
        let weak = Rc::downgrade(&editor);
        editor.window.connect_destroy(move |_| {
            if let Some(editor) = weak.upgrade() {
                if let Some(display) = gdk::Display::default() {
                    gtk::style_context_remove_provider_for_display(&display, &editor.text_css);
                }
                EDITORS.with(|list| list.borrow_mut().retain(|e| !Rc::ptr_eq(e, &editor)));
            }
        });
        editor.sync();
        editor.window.present();
        editor.canvas.grab_focus();
        editor
    }

    // MARK: Building the window

    fn build(self: &Rc<Self>) {
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));

        // Tools.
        let tools_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        tools_box.add_css_class("shotput-tools");
        let mut tools = Vec::new();
        for tool in Tool::ALL {
            let button = gtk::ToggleButton::new();
            button.set_icon_name(tool.icon());
            button.set_tooltip_text(Some(&format!("{}  {}", tool.title(), tool.key().to_ascii_uppercase())));
            button.set_focus_on_click(false);
            button.add_css_class("flat");
            if let Some(first) = tools.first() {
                button.set_group(Some(first));
            }
            let weak = Rc::downgrade(self);
            button.connect_toggled(move |b| {
                let Some(e) = weak.upgrade() else { return };
                if b.is_active() && !e.syncing.get() {
                    e.commit_text();
                    e.set_tool(tool);
                    e.canvas.grab_focus();
                }
            });
            tools_box.append(&button);
            tools.push(button);
        }
        header.pack_start(&tools_box);

        let done = gtk::Button::with_label("Done");
        done.add_css_class("suggested-action");
        done.set_tooltip_text(Some("Copy and close  Return"));
        done.set_focus_on_click(false);
        done.set_action_name(Some("win.done"));
        header.pack_end(&done);

        let action_button = |icon: &str, tip: &str, action: &str| {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(tip));
            b.set_focus_on_click(false);
            b.set_action_name(Some(action));
            b
        };
        let copy_tip = if self.settings.closes_on_copy() { "Copy and close  Ctrl+C" } else { "Copy image  Ctrl+C" };
        let copy = action_button("edit-copy-symbolic", copy_tip, "win.copy");
        let save = action_button("document-save-symbolic", "Save  Ctrl+S", "win.save");
        let read_text = action_button(
            "shotput-read-text-symbolic",
            "Copy text: reads the text in this screenshot and copies it",
            "win.copy-text",
        );
        let drag_handle = gtk::Button::from_icon_name("shotput-drag-symbolic");
        drag_handle.set_tooltip_text(Some("Drag into any app"));
        drag_handle.set_focus_on_click(false);
        drag_handle.set_cursor_from_name(Some("grab"));
        self.attach_drag_source(&drag_handle);
        {
            let weak = Rc::downgrade(self);
            drag_handle.connect_clicked(move |_| {
                if let Some(e) = weak.upgrade() {
                    e.show_toast("Drag this button into any app");
                }
            });
        }
        header.pack_end(&drag_handle);
        header.pack_end(&copy);
        header.pack_end(&save);
        header.pack_end(&read_text);

        // Background popover.
        let (bg_button, bg_parts) = self.build_background_popover();
        header.pack_end(&bg_button);

        // Style bar and toast.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("shotput-bar");
        let swatch_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let mut swatches = Vec::new();
        for ink in Ink::ALL {
            let b = gtk::Button::new();
            b.add_css_class("shotput-circle");
            b.add_css_class("shotput-swatch");
            b.set_focus_on_click(false);
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("shotput-swatch-dot");
            dot.add_css_class(&format!("ink-{}", ink.index()));
            dot.set_halign(gtk::Align::Center);
            dot.set_valign(gtk::Align::Center);
            b.set_child(Some(&dot));
            b.set_tooltip_text(Some(&format!("{} ({})", ink.name(), ink.index() + 1)));
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(e) = weak.upgrade() {
                    e.set_color(ink);
                }
            });
            swatch_box.append(&b);
            swatches.push(b);
        }
        let weight_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let mut weights = Vec::new();
        for weight in Weight::ALL {
            let b = gtk::Button::new();
            b.add_css_class("shotput-circle");
            b.set_focus_on_click(false);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            line.add_css_class("shotput-weight-bar");
            line.add_css_class(&format!("weight-{}", weight.index()));
            line.set_halign(gtk::Align::Center);
            line.set_valign(gtk::Align::Center);
            b.set_child(Some(&line));
            b.set_tooltip_text(Some(weight.name()));
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(e) = weak.upgrade() {
                    e.set_weight(weight);
                }
            });
            weight_box.append(&b);
            weights.push(b);
        }
        let fill = gtk::Button::from_icon_name("shotput-unfilled-symbolic");
        fill.add_css_class("shotput-circle");
        fill.set_focus_on_click(false);
        {
            let weak = Rc::downgrade(self);
            fill.connect_clicked(move |_| {
                if let Some(e) = weak.upgrade() {
                    let filled = !e.model.borrow().filled;
                    e.set_filled(filled);
                }
            });
        }
        let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
        undo.set_tooltip_text(Some("Undo"));
        undo.set_action_name(Some("win.undo"));
        let redo = gtk::Button::from_icon_name("edit-redo-symbolic");
        redo.set_tooltip_text(Some("Redo"));
        redo.set_action_name(Some("win.redo"));
        for b in [&undo, &redo] {
            b.add_css_class("shotput-circle");
            b.set_focus_on_click(false);
        }
        let history = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        history.append(&undo);
        history.append(&redo);
        let sep = || {
            let s = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            s.add_css_class("shotput-sep");
            s.set_valign(gtk::Align::Center);
            s
        };
        bar.append(&swatch_box);
        bar.append(&sep());
        bar.append(&weight_box);
        bar.append(&sep());
        bar.append(&fill);
        bar.append(&sep());
        bar.append(&history);

        let toast = gtk::Label::new(None);
        toast.add_css_class("shotput-toast");
        let toast_revealer = gtk::Revealer::new();
        toast_revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        toast_revealer.set_transition_duration(200);
        toast_revealer.set_child(Some(&toast));
        toast_revealer.set_halign(gtk::Align::Center);
        toast_revealer.set_can_target(false);

        let bottom = gtk::Box::new(gtk::Orientation::Vertical, 8);
        bottom.set_halign(gtk::Align::Center);
        bottom.set_valign(gtk::Align::End);
        bottom.set_margin_bottom(8);
        bottom.append(&toast_revealer);
        bottom.append(&bar);
        self.overlay.add_overlay(&bottom);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&self.overlay));
        self.window.set_content(Some(&toolbar_view));

        *self.ui.borrow_mut() = Some(Ui {
            tools,
            swatches,
            weights,
            fill,
            undo,
            redo,
            toast,
            toast_revealer,
            copy,
            save,
            read_text,
            bg_choice: bg_parts.0,
            bg_color: bg_parts.1,
            bg_color_row: bg_parts.2,
            bg_padding: bg_parts.3,
            bg_padding_label: bg_parts.4,
            bg_padding_row: bg_parts.5,
        });

        self.install_actions();
        self.install_canvas();
    }

    #[allow(clippy::type_complexity)]
    fn build_background_popover(
        self: &Rc<Self>,
    ) -> (gtk::MenuButton, (Segmented, gtk::ColorDialogButton, gtk::Box, gtk::Scale, gtk::Label, gtk::Box)) {
        let button = gtk::MenuButton::new();
        button.set_icon_name("shotput-background-symbolic");
        button.set_tooltip_text(Some("Background"));
        button.set_focus_on_click(false);
        button.set_always_show_arrow(false);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.set_margin_top(14);
        content.set_margin_bottom(14);
        content.set_margin_start(14);
        content.set_margin_end(14);
        content.set_width_request(300);

        let choice = Segmented::new(&[("none", "None"), ("wallpaper", "Wallpaper"), ("color", "Colour")]);
        choice.set_active_name(Some("none"));
        content.append(&choice.widget);

        let color_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let color_label = gtk::Label::new(Some("Colour"));
        color_label.set_hexpand(true);
        color_label.set_xalign(0.0);
        let dialog = gtk::ColorDialog::new();
        dialog.set_with_alpha(false);
        let color = gtk::ColorDialogButton::new(Some(dialog));
        let initial = Rgba::parse(&self.settings.string("window-background-color", "#E9E6DF")).unwrap_or(Rgba::hex(0xE9E6DF, 1.0));
        color.set_rgba(&initial.gdk());
        color_row.append(&color_label);
        color_row.append(&color);
        content.append(&color_row);

        let padding_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        padding_row.append(&gtk::Label::new(Some("Padding")));
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 8.0, 160.0, 4.0);
        scale.set_hexpand(true);
        scale.set_draw_value(false);
        scale.set_value(self.settings.double("window-padding", 48.0).clamp(8.0, 160.0));
        let value_label = gtk::Label::new(Some(&format!("{} pt", scale.value() as i32)));
        value_label.add_css_class("dim-label");
        value_label.add_css_class("numeric");
        value_label.set_width_chars(6);
        value_label.set_xalign(1.0);
        padding_row.append(&scale);
        padding_row.append(&value_label);
        content.append(&padding_row);

        let popover = gtk::Popover::new();
        popover.set_child(Some(&content));
        button.set_popover(Some(&popover));

        {
            let weak = Rc::downgrade(self);
            popover.connect_show(move |_| {
                if let Some(e) = weak.upgrade() {
                    e.sync_background_controls();
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            choice.connect_active_name_notify(move |_| {
                if let Some(e) = weak.upgrade() {
                    if !e.syncing.get() {
                        e.sync_background_rows();
                        e.apply_backdrop(false);
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            color.connect_rgba_notify(move |_| {
                if let Some(e) = weak.upgrade() {
                    if !e.syncing.get() {
                        e.apply_backdrop(e.padding_continuing());
                    }
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let label = value_label.clone();
            scale.connect_value_changed(move |s| {
                label.set_text(&format!("{} pt", s.value() as i32));
                if let Some(e) = weak.upgrade() {
                    if !e.syncing.get() {
                        // Slider moves fold into one undo step per drag.
                        e.apply_backdrop(e.padding_continuing());
                    }
                }
            });
        }
        (button, (choice, color, color_row, scale, value_label, padding_row))
    }

    fn padding_continuing(&self) -> bool {
        let now = glib::monotonic_time();
        let continuing = now - self.last_padding_change.get() < 600_000;
        self.last_padding_change.set(now);
        continuing
    }

    fn install_actions(self: &Rc<Self>) {
        let add = |name: &str, f: Box<dyn Fn(&Rc<Editor>)>| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(e) = weak.upgrade() {
                    f(&e);
                }
            });
            self.window.add_action(&action);
        };
        add("done", Box::new(|e| e.done()));
        add("copy", Box::new(|e| {
            if let Some(view) = e.focused_text_view() {
                view.emit_copy_clipboard();
            } else {
                e.copy();
            }
        }));
        add("save", Box::new(|e| e.save()));
        add("copy-text", Box::new(|e| e.copy_recognized_text()));
        add("undo", Box::new(|e| {
            if let Some(view) = e.focused_text_view() {
                view.buffer().undo();
            } else {
                e.commit_text();
                e.model.borrow_mut().undo();
                e.sync();
            }
        }));
        add("redo", Box::new(|e| {
            if let Some(view) = e.focused_text_view() {
                view.buffer().redo();
            } else {
                e.commit_text();
                e.model.borrow_mut().redo();
                e.sync();
            }
        }));
        add("zoom-in", Box::new(|e| e.zoom_by(1.5, None)));
        add("zoom-out", Box::new(|e| e.zoom_by(1.0 / 1.5, None)));
        add("zoom-actual", Box::new(|e| {
            let z = e.zoom.get().max(1e-6);
            e.zoom_by(1.0 / z, None)
        }));
        add("zoom-fit", Box::new(|e| {
            e.fits.set(true);
            e.reposition_text_editor();
            e.canvas.queue_draw();
        }));
        add("close", Box::new(|e| e.window.close()));
    }

    fn focused_text_view(&self) -> Option<gtk::TextView> {
        let text = self.text.borrow();
        let view = &text.as_ref()?.view;
        view.has_focus().then(|| view.clone())
    }

    fn install_canvas(self: &Rc<Self>) {
        {
            let weak = Rc::downgrade(self);
            self.canvas.set_draw(move |snapshot, w, h| {
                if let Some(e) = weak.upgrade() {
                    e.draw(snapshot, w, h);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            self.canvas.set_resized(move |_, _| {
                if let Some(e) = weak.upgrade() {
                    e.reposition_text_editor();
                }
            });
        }

        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        {
            let weak = Rc::downgrade(self);
            drag.connect_drag_begin(move |g, x, y| {
                if let Some(e) = weak.upgrade() {
                    e.mouse_down(g, x, y);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            drag.connect_drag_update(move |g, dx, dy| {
                if let Some(e) = weak.upgrade() {
                    e.mouse_dragged(g, dx, dy);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            drag.connect_drag_end(move |_, _, _| {
                if let Some(e) = weak.upgrade() {
                    e.mouse_up();
                }
            });
        }
        self.canvas.add_controller(drag);

        // Middle-drag pans a zoomed image.
        let pan = gtk::GestureDrag::new();
        pan.set_button(gdk::BUTTON_MIDDLE);
        {
            let weak = Rc::downgrade(self);
            let start = Rc::new(Cell::new((0.0, 0.0)));
            let s2 = start.clone();
            pan.connect_drag_begin(move |_, _, _| {
                if let Some(e) = weak.upgrade() {
                    let v = e.view.get();
                    s2.set((v.ox, v.oy));
                }
            });
            let weak = Rc::downgrade(self);
            pan.connect_drag_update(move |_, dx, dy| {
                if let Some(e) = weak.upgrade() {
                    let (ox, oy) = start.get();
                    e.pan.set((ox + dx, oy + dy));
                    e.reposition_text_editor();
                    e.canvas.queue_draw();
                }
            });
        }
        self.canvas.add_controller(pan);

        // Ctrl+scroll zooms around the pointer; plain scrolling pans a zoomed image.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let pointer = Rc::new(Cell::new((0.0, 0.0)));
        {
            let motion = gtk::EventControllerMotion::new();
            let pointer = pointer.clone();
            motion.connect_motion(move |_, x, y| pointer.set((x, y)));
            self.canvas.add_controller(motion);
        }
        {
            let weak = Rc::downgrade(self);
            scroll.connect_scroll(move |c, dx, dy| {
                let Some(e) = weak.upgrade() else { return glib::Propagation::Proceed };
                let state = c.current_event_state();
                if state.contains(gdk::ModifierType::CONTROL_MASK) {
                    let factor = if c.unit() == gdk::ScrollUnit::Surface { (-dy * 0.01).exp() } else { 1.15f64.powf(-dy) };
                    e.zoom_by(factor, Some(pointer.get()));
                } else {
                    let step = if c.unit() == gdk::ScrollUnit::Surface { 1.0 } else { 40.0 };
                    let (dx, dy) = if state.contains(gdk::ModifierType::SHIFT_MASK) && dx == 0.0 { (dy, 0.0) } else { (dx, dy) };
                    let v = e.view.get();
                    e.pan.set((v.ox - dx * step, v.oy - dy * step));
                    e.reposition_text_editor();
                    e.canvas.queue_draw();
                }
                glib::Propagation::Stop
            });
        }
        self.canvas.add_controller(scroll);

        let pinch = gtk::GestureZoom::new();
        {
            let weak = Rc::downgrade(self);
            pinch.connect_begin(move |_, _| {
                if let Some(e) = weak.upgrade() {
                    e.pinch_start.set(e.view.get().zoom);
                }
            });
            let weak = Rc::downgrade(self);
            pinch.connect_scale_changed(move |g, scale| {
                if let Some(e) = weak.upgrade() {
                    let target = e.pinch_start.get() * scale;
                    let current = e.view.get().zoom;
                    let center = g.bounding_box_center().unwrap_or((0.0, 0.0));
                    e.zoom_by(target / current.max(1e-6), Some(center));
                }
            });
        }
        self.canvas.add_controller(pinch);

        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, key, _, state| {
                weak.upgrade().map_or(glib::Propagation::Proceed, |e| e.key_down(key, state))
            });
        }
        self.window.add_controller(keys);

        {
            let weak = Rc::downgrade(self);
            self.window.connect_close_request(move |_| {
                if let Some(e) = weak.upgrade() {
                    e.commit_text();
                }
                glib::Propagation::Proceed
            });
        }
    }

    // MARK: Layout and zoom

    /// Sizes the window around the image, then fits it.
    fn lay_out(&self) {
        let (iw, ih) = self.model.borrow().size();
        let (mw, mh) = self.capture.borrow().monitor.unwrap_or_else(|| {
            gdk::Display::default()
                .and_then(|d| d.monitors().item(0))
                .and_then(|m| m.downcast::<gdk::Monitor>().ok())
                .map(|m| {
                    let g = m.geometry();
                    (g.width() as f64, g.height() as f64)
                })
                .unwrap_or((1440.0, 900.0))
        });
        let header = 47.0;
        let limit = (mw * 0.94, mh * 0.94 - header - 32.0);
        let w = (iw + MARGIN * 2.0).max(MIN_CONTENT.0).min(limit.0);
        let h = (ih + MARGIN * 2.0 + BOTTOM_RESERVE).max(MIN_CONTENT.1).min(limit.1);
        self.window.set_default_size(w.round() as i32, (h + header).round() as i32);
        self.fits.set(true);
    }

    fn fit_zoom(&self, w: f64, h: f64) -> f64 {
        let (iw, ih) = self.model.borrow().size();
        if iw <= 0.0 || ih <= 0.0 {
            return 1.0;
        }
        let aw = (w - MARGIN * 2.0).max(1.0);
        let ah = (h - MARGIN * 2.0 - BOTTOM_RESERVE).max(1.0);
        1f64.min(aw / iw).min(ah / ih).max(0.02)
    }

    fn compute_view(&self, w: f64, h: f64) -> View {
        let (iw, ih) = self.model.borrow().size();
        let zoom = if self.fits.get() { self.fit_zoom(w, h) } else { self.zoom.get() };
        self.zoom.set(zoom);
        let (cw, ch) = (iw * zoom, ih * zoom);
        let available = h - BOTTOM_RESERVE;
        let (px, py) = self.pan.get();
        let ox = if cw <= w { (w - cw) / 2.0 } else { px.clamp(w - cw, 0.0) };
        let oy = if ch <= available { (available - ch) / 2.0 } else { py.clamp(available - ch, 0.0) };
        self.pan.set((ox, oy));
        View { ox, oy, zoom }
    }

    fn zoom_by(&self, factor: f64, anchor: Option<(f64, f64)>) {
        let (w, h) = (self.canvas.width() as f64, self.canvas.height() as f64);
        let view = self.compute_view(w, h);
        let target = (view.zoom * factor).clamp(0.1, 16.0);
        let (ax, ay) = anchor.unwrap_or((w / 2.0, (h - BOTTOM_RESERVE) / 2.0));
        let k = target / view.zoom;
        self.pan.set((ax - (ax - view.ox) * k, ay - (ay - view.oy) * k));
        self.zoom.set(target);
        self.fits.set(false);
        self.view.set(self.compute_view(w, h));
        self.reposition_text_editor();
        self.canvas.queue_draw();
    }

    // MARK: Drawing

    fn draw(&self, snapshot: &gtk::Snapshot, w: f64, h: f64) {
        let view = self.compute_view(w, h);
        self.view.set(view);
        let model = self.model.borrow();
        let image = view.rect(model.image_bounds());

        snapshot.append_outset_shadow(
            &gsk::RoundedRect::from_rect(grect(image), 0.0),
            &gdk::RGBA::new(0.0, 0.0, 0.0, 0.14),
            0.0,
            4.0,
            0.0,
            14.0,
        );
        let device = self.canvas.scale_factor() as f64;
        let magnification = view.zoom * device / model.scale;
        let filter = if magnification >= 0.999 { gsk::ScalingFilter::Nearest } else { gsk::ScalingFilter::Trilinear };
        snapshot.append_scaled_texture(&model.base.texture, filter, &grect(image));

        let editing = self.text.borrow().as_ref().and_then(|t| t.id);
        let visible = R::new(0.0, 0.0, w, h);
        if let Some(id) = editing {
            let list: Vec<Annotation> = model.annotations.iter().filter(|a| a.id != id).cloned().collect();
            self.renderer.draw(snapshot, &list, &model.base, model.scale, view, visible);
        } else {
            self.renderer.draw(snapshot, &model.annotations, &model.base, model.scale, view, visible);
        }

        if let Some(selected) = model.selected().filter(|a| Some(a.id) != editing) {
            self.draw_selection(snapshot, selected, view);
        }
        if let Some(crop) = self.crop_rect.get() {
            let c = view.rect(crop);
            let dim = gdk::RGBA::new(0.0, 0.0, 0.0, 0.45);
            for r in [
                R::new(image.x, image.y, image.w, c.y - image.y),
                R::new(image.x, c.max_y(), image.w, image.max_y() - c.max_y()),
                R::new(image.x, c.y, c.x - image.x, c.h),
                R::new(c.max_x(), c.y, image.max_x() - c.max_x(), c.h),
            ] {
                if r.w > 0.0 && r.h > 0.0 {
                    snapshot.append_color(&dim, &grect(r));
                }
            }
            snapshot.append_border(
                &gsk::RoundedRect::from_rect(grect(c), 0.0),
                &[1.0; 4],
                &[gdk::RGBA::WHITE, gdk::RGBA::WHITE, gdk::RGBA::WHITE, gdk::RGBA::WHITE],
            );
        }
    }

    /// The system accent colour (libadwaita 1.6 and later), or GNOME's blue before that.
    fn accent(&self) -> gdk::RGBA {
        let style = adw::StyleManager::default();
        if style.find_property("accent-color-rgba").is_some() {
            return style.property::<gdk::RGBA>("accent-color-rgba");
        }
        gdk::RGBA::new(0.208, 0.518, 0.894, 1.0)
    }

    fn draw_selection(&self, snapshot: &gtk::Snapshot, selected: &Annotation, view: View) {
        let accent = self.accent();
        let bounds = view.rect(selected.frame()).inset(-24.0, -24.0);
        let cr = snapshot.append_cairo(&grect(bounds));
        let set = |c: &gdk::RGBA| cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64);
        let handle = |q: P| {
            let q = view.to_view(q);
            let r = 4.5;
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
            cr.arc(q.x, q.y + 0.5, r + 1.0, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.arc(q.x, q.y, r, 0.0, std::f64::consts::TAU);
            let _ = cr.fill_preserve();
            set(&accent);
            cr.set_line_width(1.5);
            let _ = cr.stroke();
        };
        if let Some((a, b)) = selected.endpoints() {
            handle(a);
            handle(b);
            // The bend handle is smaller and filled, so it reads as "shape this" rather than "move an end".
            if let Some(mid) = selected.bend_handle() {
                if a.dist(b) * view.zoom > 28.0 {
                    let q = view.to_view(mid);
                    let r = 3.5;
                    if selected.bend.is_none() {
                        cr.set_source_rgb(1.0, 1.0, 1.0);
                    } else {
                        set(&accent);
                    }
                    cr.arc(q.x, q.y, r, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill_preserve();
                    if selected.bend.is_none() {
                        set(&accent);
                    } else {
                        cr.set_source_rgb(1.0, 1.0, 1.0);
                    }
                    cr.set_line_width(1.5);
                    let _ = cr.stroke();
                }
            }
        } else if let Some(rect) = selected.resizable_rect() {
            let r = view.rect(rect);
            set(&accent);
            cr.set_line_width(1.0);
            cr.rectangle(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0);
            let _ = cr.stroke();
            for c in rect.corners() {
                handle(c);
            }
        } else {
            let outline = view.rect(selected.frame()).inset(-4.0, -4.0);
            set(&accent);
            cr.set_line_width(1.0);
            cr.set_dash(&[4.0, 3.0], 0.0);
            render::rounded_rect(&cr, outline, 4.0);
            let _ = cr.stroke();
        }
    }

    // MARK: Mouse

    fn mouse_down(self: &Rc<Self>, _gesture: &gtk::GestureDrag, x: f64, y: f64) {
        self.drag_start.set((x, y));
        if self.text.borrow().is_some() {
            self.commit_text();
            *self.drag.borrow_mut() = Drag::Ignore;
            return;
        }
        self.canvas.grab_focus();
        let view = self.view.get();
        let q = view.to_image(p(x, y));
        let zoom = view.zoom;

        // Double clicks, counted by hand so the drag gesture sees every press.
        let now = glib::monotonic_time();
        let (last_t, lx, ly, count) = self.last_click.get();
        let click_count = if now - last_t < 400_000 && (lx - x).abs() < 5.0 && (ly - y).abs() < 5.0 { count + 1 } else { 1 };
        self.last_click.set((now, x, y, click_count));

        let mut model = self.model.borrow_mut();
        model.begin_gesture();

        if let Some(selected) = model.selected().cloned() {
            let reach = 9.0 / zoom;
            if let Some((a, b)) = selected.endpoints() {
                if q.dist(b) <= reach {
                    *self.drag.borrow_mut() = Drag::Endpoint { id: selected.id, moves_start: false, creating: false };
                    return;
                }
                if q.dist(a) <= reach {
                    *self.drag.borrow_mut() = Drag::Endpoint { id: selected.id, moves_start: true, creating: false };
                    return;
                }
                if let Some(mid) = selected.bend_handle() {
                    if q.dist(mid) <= reach {
                        // Double click a bent line to straighten it again.
                        if click_count == 2 {
                            model.replace(selected.id, |a| {
                                let mut c = a.clone();
                                c.bend = None;
                                c
                            });
                            drop(model);
                            self.sync();
                            *self.drag.borrow_mut() = Drag::Ignore;
                            return;
                        }
                        *self.drag.borrow_mut() = Drag::Bend { id: selected.id };
                        return;
                    }
                }
            } else if let Some(rect) = selected.resizable_rect() {
                let corners = rect.corners();
                if let Some(i) = corners.iter().position(|c| q.dist(*c) <= reach) {
                    *self.drag.borrow_mut() = Drag::Rect { id: selected.id, anchor: corners[(i + 2) % 4], creating: false };
                    return;
                }
            }
        }

        self.clicked_area_mark.set(None);
        if model.tool != Tool::Crop {
            if let Some(hit) = model.annotations.iter().rev().find(|a| a.hit_test(q, 6.0 / zoom)).cloned() {
                // Solid marks only grab by their edge, or once selected. Otherwise nothing could be
                // drawn or typed on top of a redaction or highlight. A plain click inside still selects.
                let grabs = !hit.covers_area() || Some(hit.id) == model.selected || hit.hits_outline(q, 6.0 / zoom);
                if grabs {
                    model.set_selected(Some(hit.id));
                    if let (2, Kind::Text(origin, _)) = (click_count, &hit.kind) {
                        let origin = *origin;
                        drop(model);
                        *self.drag.borrow_mut() = Drag::Ignore;
                        self.begin_text_editing(origin, Some(hit));
                    } else {
                        *self.drag.borrow_mut() = Drag::Move { id: hit.id, last: q };
                        drop(model);
                        self.sync();
                    }
                    return;
                }
                self.clicked_area_mark.set(Some(hit.id));
            }
        }

        model.set_selected(None);
        let start = q.clamped(model.image_bounds());
        let tool = model.tool;
        fn create(model: &mut Model, kind: Kind) -> u64 {
            let a = model.make(kind);
            let id = a.id;
            model.update(|list| list.push(a));
            id
        }
        let drag = match tool {
            Tool::Crop => {
                self.crop_rect.set(None);
                Drag::Crop { anchor: start }
            }
            Tool::Text => {
                let weight = model.weight;
                drop(model);
                let (px, py) = text_metrics::padding(weight);
                let line = text_metrics::text_size("", weight).1;
                *self.drag.borrow_mut() = Drag::Ignore;
                self.begin_text_editing(p(q.x - px, q.y - py - line / 2.0), None);
                return;
            }
            Tool::Counter => {
                let next = model.next_counter();
                let id = create(&mut model, Kind::Counter(q, next));
                model.set_selected(Some(id));
                Drag::Move { id, last: q }
            }
            Tool::Pen => Drag::Pen { id: create(&mut model, Kind::Pen(vec![q])) },
            Tool::Arrow => Drag::Endpoint { id: create(&mut model, Kind::Arrow(q, q)), moves_start: false, creating: true },
            Tool::Line => Drag::Endpoint { id: create(&mut model, Kind::Line(q, q)), moves_start: false, creating: true },
            Tool::Rectangle => {
                Drag::Rect { id: create(&mut model, Kind::Rectangle(R::new(q.x, q.y, 0.0, 0.0))), anchor: q, creating: true }
            }
            Tool::Ellipse => {
                Drag::Rect { id: create(&mut model, Kind::Ellipse(R::new(q.x, q.y, 0.0, 0.0))), anchor: q, creating: true }
            }
            Tool::Highlight => {
                Drag::Rect { id: create(&mut model, Kind::Highlight(R::new(q.x, q.y, 0.0, 0.0))), anchor: q, creating: true }
            }
            Tool::Redact => {
                Drag::Rect { id: create(&mut model, Kind::Redact(R::new(q.x, q.y, 0.0, 0.0))), anchor: q, creating: true }
            }
        };
        drop(model);
        *self.drag.borrow_mut() = drag;
        self.sync();
    }

    fn mouse_dragged(&self, gesture: &gtk::GestureDrag, dx: f64, dy: f64) {
        let (sx, sy) = self.drag_start.get();
        let view = self.view.get();
        let raw = view.to_image(p(sx + dx, sy + dy));
        let constrain = gesture.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
        let zoom = view.zoom;
        let mut drag = self.drag.borrow_mut();
        let mut model = self.model.borrow_mut();
        match &mut *drag {
            Drag::None | Drag::Ignore => return,
            Drag::Move { id, last } => {
                let delta = raw - *last;
                model.replace(*id, |a| a.translated(delta));
                *last = raw;
            }
            Drag::Rect { id, anchor, .. } => {
                let anchor = *anchor;
                let mut q = raw;
                if constrain {
                    let side = (q.x - anchor.x).abs().max((q.y - anchor.y).abs());
                    q = p(
                        anchor.x + if q.x < anchor.x { -side } else { side },
                        anchor.y + if q.y < anchor.y { -side } else { side },
                    );
                }
                model.replace(*id, |a| a.with_rect(R::from_points(anchor, q)));
            }
            Drag::Endpoint { id, moves_start, .. } => {
                let moves_start = *moves_start;
                model.replace(*id, |a| {
                    let Some((from, to)) = a.endpoints() else { return a.clone() };
                    let fixed = if moves_start { to } else { from };
                    let mut q = raw;
                    if constrain {
                        // Snap to 45 degree steps.
                        let d = raw - fixed;
                        let step = std::f64::consts::PI / 4.0;
                        let angle = (d.y.atan2(d.x) / step).round() * step;
                        q = fixed + p(angle.cos(), angle.sin()) * d.len();
                    }
                    if moves_start { a.with_endpoints(q, fixed) } else { a.with_endpoints(fixed, q) }
                });
            }
            Drag::Bend { id } => model.replace(*id, |a| a.bent_through(raw)),
            Drag::Pen { id } => model.replace(*id, |a| {
                let Kind::Pen(points) = &a.kind else { return a.clone() };
                if points.last().is_some_and(|l| l.dist(raw) < 1.5 / zoom) {
                    return a.clone();
                }
                let mut c = a.clone();
                let mut points = points.clone();
                points.push(raw);
                c.kind = Kind::Pen(points);
                c
            }),
            Drag::Crop { anchor } => {
                let bounds = model.image_bounds();
                self.crop_rect.set(Some(R::from_points(*anchor, raw.clamped(bounds))));
            }
        }
        drop(model);
        drop(drag);
        self.canvas.queue_draw();
    }

    fn mouse_up(&self) {
        let drag = std::mem::replace(&mut *self.drag.borrow_mut(), Drag::None);
        let mut toast = None;
        {
            let mut model = self.model.borrow_mut();
            match drag {
                Drag::Rect { id, creating: true, .. } => {
                    self.finish_creating(&mut model, id, |a| a.frame().w >= 4.0 || a.frame().h >= 4.0)
                }
                Drag::Endpoint { id, creating: true, .. } => {
                    self.finish_creating(&mut model, id, |a| a.endpoints().is_some_and(|(f, t)| f.dist(t) >= 6.0))
                }
                Drag::Pen { id } => {
                    let points = model.annotations.iter().find(|a| a.id == id).and_then(|a| match &a.kind {
                        Kind::Pen(points) => Some(points.clone()),
                        _ => None,
                    });
                    if let Some(points) = points {
                        if points.len() < 2 {
                            model.discard_gesture();
                            let mark = self.clicked_area_mark.get();
                            model.set_selected(mark);
                        } else if self.settings.recognizes_shapes() {
                            if let Some(shape) = shapes::recognize(&points) {
                                // A separate undo step, so Ctrl+Z brings the sketch back.
                                model.begin_gesture();
                                let kind = shape.kind();
                                model.replace(id, |a| {
                                    let mut c = a.clone();
                                    c.kind = kind;
                                    c
                                });
                                toast = Some(format!("Made it a {}. Ctrl+Z keeps the sketch", shape.name()));
                            }
                        }
                    }
                }
                Drag::Crop { .. } => {
                    if let Some(rect) = self.crop_rect.get().map(|r| r.integral()) {
                        if rect.w >= 8.0 && rect.h >= 8.0 {
                            model.crop(rect);
                        }
                    }
                    self.crop_rect.set(None);
                }
                _ => {}
            }
        }
        if let Some(message) = toast {
            self.show_toast(&message);
        }
        self.sync();
    }

    fn finish_creating(&self, model: &mut Model, id: u64, valid: impl Fn(&Annotation) -> bool) {
        let Some(a) = model.annotations.iter().find(|a| a.id == id).cloned() else { return };
        if valid(&a) {
            model.set_selected(Some(id));
        } else {
            model.discard_gesture();
            model.set_selected(self.clicked_area_mark.get());
        }
    }

    // MARK: Keyboard

    fn key_down(self: &Rc<Self>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        if self.text.borrow().is_some() {
            return glib::Propagation::Proceed;
        }
        if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Delete | gdk::Key::BackSpace | gdk::Key::KP_Delete => {
                self.model.borrow_mut().delete_selected();
                self.sync();
            }
            gdk::Key::Escape => {
                let has_selection = self.model.borrow().selected.is_some();
                if self.crop_rect.get().is_none() && !has_selection {
                    if self.settings.escape_copies() {
                        self.done();
                    } else {
                        self.window.close();
                    }
                    return glib::Propagation::Stop;
                }
                self.crop_rect.set(None);
                self.model.borrow_mut().set_selected(None);
                self.sync();
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => self.done(),
            gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down => {
                let Some(id) = self.model.borrow().selected else { return glib::Propagation::Stop };
                let step = if state.contains(gdk::ModifierType::SHIFT_MASK) { 10.0 } else { 1.0 };
                let delta = match key {
                    gdk::Key::Left => p(-step, 0.0),
                    gdk::Key::Right => p(step, 0.0),
                    gdk::Key::Down => p(0.0, step),
                    _ => p(0.0, -step),
                };
                let mut model = self.model.borrow_mut();
                model.begin_gesture();
                model.replace(id, |a| a.translated(delta));
                drop(model);
                self.sync();
            }
            _ => {
                let Some(c) = key.to_unicode().map(|c| c.to_ascii_lowercase()) else { return glib::Propagation::Proceed };
                if let Some(tool) = Tool::ALL.iter().find(|t| t.key() == c) {
                    self.set_tool(*tool);
                } else if let Some(d) = c.to_digit(10).filter(|d| (1..=8).contains(d)) {
                    self.set_color(Ink::from_index(d as i32 - 1));
                } else {
                    return glib::Propagation::Proceed;
                }
            }
        }
        glib::Propagation::Stop
    }

    // MARK: Style

    fn set_tool(&self, tool: Tool) {
        {
            let mut model = self.model.borrow_mut();
            model.set_selected(None);
            model.tool = tool;
            model.filled = *model.fill_by_tool.get(&tool).unwrap_or(&false);
        }
        self.crop_rect.set(None);
        self.settings.set_int("editor-tool", tool.index() as i32);
        self.sync();
    }

    fn set_color(&self, color: Ink) {
        self.settings.set_int("editor-color", color.index() as i32);
        let mut model = self.model.borrow_mut();
        model.color = color;
        if let Some(id) = model.selected {
            model.begin_gesture();
            model.replace(id, |a| {
                let mut c = a.clone();
                c.color = color;
                c
            });
        }
        drop(model);
        self.sync();
    }

    fn set_weight(&self, weight: Weight) {
        self.settings.set_int("editor-weight", weight.index() as i32);
        let mut model = self.model.borrow_mut();
        model.weight = weight;
        if let Some(id) = model.selected {
            model.begin_gesture();
            model.replace(id, |a| {
                let mut c = a.clone();
                c.weight = weight;
                c
            });
        }
        drop(model);
        self.sync();
    }

    fn set_filled(&self, filled: bool) {
        let mut model = self.model.borrow_mut();
        let tool = model.style_tool();
        if !tool.supports_fill() {
            return;
        }
        model.filled = filled;
        model.fill_by_tool.insert(tool, filled);
        if let Some(id) = model.selected {
            model.begin_gesture();
            model.replace(id, |a| {
                let mut c = a.clone();
                c.filled = filled;
                c
            });
        }
        drop(model);
        self.sync();
    }

    /// Matches every control to the model and redraws.
    fn sync(&self) {
        self.syncing.set(true);
        if let Some(ui) = self.ui.borrow().as_ref() {
            let model = self.model.borrow();
            for (i, b) in ui.tools.iter().enumerate() {
                b.set_active(Tool::ALL[i] == model.tool);
            }
            let style = model.style_tool();
            for (i, b) in ui.swatches.iter().enumerate() {
                if Ink::ALL[i] == model.color { b.add_css_class("selected") } else { b.remove_css_class("selected") }
                b.set_sensitive(style.supports_color());
            }
            for (i, b) in ui.weights.iter().enumerate() {
                if Weight::ALL[i] == model.weight { b.add_css_class("selected") } else { b.remove_css_class("selected") }
                b.set_sensitive(style.supports_weight());
            }
            ui.fill.set_icon_name(if model.filled { "shotput-fill-symbolic" } else { "shotput-unfilled-symbolic" });
            if model.filled { ui.fill.add_css_class("accent-on") } else { ui.fill.remove_css_class("accent-on") }
            ui.fill.set_tooltip_text(Some(if style == Tool::Redact { "Solid block" } else { "Fill" }));
            ui.fill.set_sensitive(style.supports_fill());
            if let Some(a) = self.window.lookup_action("undo").and_downcast::<gio::SimpleAction>() {
                a.set_enabled(!model.undo.is_empty());
            }
            if let Some(a) = self.window.lookup_action("redo").and_downcast::<gio::SimpleAction>() {
                a.set_enabled(!model.redo.is_empty());
            }
            let _ = (&ui.undo, &ui.redo);
        }
        self.syncing.set(false);
        self.canvas.queue_draw();
    }

    // MARK: Background

    fn sync_background_controls(&self) {
        self.syncing.set(true);
        if let Some(ui) = self.ui.borrow().as_ref() {
            let model = self.model.borrow();
            match model.backdrop.as_ref().map(|b| b.style) {
                Some(style) => {
                    ui.bg_padding.set_value(style.padding);
                    ui.bg_padding_label.set_text(&format!("{} pt", style.padding as i32));
                    match style.fill {
                        Fill::Wallpaper => ui.bg_choice.set_active_name(Some("wallpaper")),
                        Fill::Color(c) => {
                            ui.bg_choice.set_active_name(Some("color"));
                            ui.bg_color.set_rgba(&c.gdk());
                        }
                    }
                }
                None => ui.bg_choice.set_active_name(Some("none")),
            }
        }
        self.syncing.set(false);
        self.sync_background_rows();
    }

    fn sync_background_rows(&self) {
        if let Some(ui) = self.ui.borrow().as_ref() {
            let choice = ui.bg_choice.active_name().unwrap_or_default();
            ui.bg_color_row.set_visible(choice == "color");
            ui.bg_padding_row.set_visible(choice != "none");
        }
    }

    fn chosen_style(&self) -> Option<Style> {
        let ui = self.ui.borrow();
        let ui = ui.as_ref()?;
        let padding = ui.bg_padding.value();
        match ui.bg_choice.active_name()?.as_str() {
            "wallpaper" => Some(Style { fill: Fill::Wallpaper, padding }),
            "color" => {
                let c = ui.bg_color.rgba();
                Some(Style { fill: Fill::Color(Rgba(c.red() as f64, c.green() as f64, c.blue() as f64, 1.0)), padding })
            }
            _ => None,
        }
    }

    fn apply_backdrop(self: &Rc<Self>, continuing: bool) {
        self.commit_text();
        let style = self.chosen_style();
        let needs_wallpaper = style.is_some_and(|s| s.fill == Fill::Wallpaper) && self.wallpaper.borrow().is_none();
        if !needs_wallpaper {
            let wallpaper = self.wallpaper.borrow().clone().flatten();
            self.model.borrow_mut().set_backdrop(style, wallpaper.as_ref(), continuing);
            self.after_size_change();
            return;
        }
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let loaded = gio::spawn_blocking(backdrop::wallpaper).await.ok().flatten();
            let Some(e) = weak.upgrade() else { return };
            *e.wallpaper.borrow_mut() = Some(loaded.clone());
            let style = e.chosen_style();
            e.model.borrow_mut().set_backdrop(style, loaded.as_ref(), continuing);
            e.after_size_change();
        });
    }

    fn after_size_change(&self) {
        if self.fits.get() {
            self.canvas.queue_draw();
        }
        self.sync();
    }

    // MARK: Text editing

    fn begin_text_editing(self: &Rc<Self>, origin: P, existing: Option<Annotation>) {
        let (weight, color, filled) = {
            let model = self.model.borrow();
            match &existing {
                Some(a) => (a.weight, a.color, a.filled),
                None => (model.weight, model.color, model.make(Kind::Text(origin, String::new())).filled),
            }
        };
        let view = gtk::TextView::new();
        view.add_css_class("shotput-text");
        view.add_css_class(&self.text_class);
        view.set_wrap_mode(gtk::WrapMode::None);
        view.set_accepts_tab(false);
        view.set_halign(gtk::Align::Start);
        view.set_valign(gtk::Align::Start);
        if let Some(Annotation { kind: Kind::Text(_, s), .. }) = &existing {
            view.buffer().set_text(s);
        }
        *self.text.borrow_mut() =
            Some(TextEdit { id: existing.as_ref().map(|a| a.id), origin, weight, color, filled, view: view.clone() });
        self.overlay.add_overlay(&view);
        self.reposition_text_editor();

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let weak = Rc::downgrade(self);
            keys.connect_key_pressed(move |_, key, _, state| {
                let Some(e) = weak.upgrade() else { return glib::Propagation::Proceed };
                match key {
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter
                        if !state.contains(gdk::ModifierType::SHIFT_MASK) =>
                    {
                        e.commit_text();
                        glib::Propagation::Stop
                    }
                    gdk::Key::Escape => {
                        e.commit_text();
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        view.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        {
            let weak = Rc::downgrade(self);
            focus.connect_leave(move |_| {
                let weak = weak.clone();
                glib::idle_add_local_once(move || {
                    if let Some(e) = weak.upgrade() {
                        let still = e.text.borrow().as_ref().is_some_and(|t| t.view.has_focus());
                        if !still {
                            e.commit_text();
                        }
                    }
                });
            });
        }
        view.add_controller(focus);
        {
            let weak = Rc::downgrade(self);
            view.buffer().connect_changed(move |_| {
                if let Some(e) = weak.upgrade() {
                    e.reposition_text_editor();
                }
            });
        }
        view.grab_focus();
        let buffer = view.buffer();
        buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
        self.canvas.queue_draw();
    }

    fn reposition_text_editor(&self) {
        let text = self.text.borrow();
        let Some(edit) = text.as_ref() else { return };
        let (w, h) = (self.canvas.width() as f64, self.canvas.height() as f64);
        let view = if w > 0.0 { self.compute_view(w, h) } else { self.view.get() };
        let zoom = view.zoom;
        let buffer = edit.view.buffer();
        let string = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
        let frame = text_metrics::frame(edit.origin, &string, edit.weight);
        let (px, py) = text_metrics::padding(edit.weight);
        let at = view.to_view(p(frame.x, frame.y));
        edit.view.set_margin_start(at.x.max(0.0).round() as i32);
        edit.view.set_margin_top(at.y.max(0.0).round() as i32);
        edit.view.set_size_request((frame.w * zoom + 3.0).ceil() as i32, (frame.h * zoom).ceil() as i32);
        edit.view.set_left_margin((px * zoom).round() as i32);
        edit.view.set_right_margin(0);
        edit.view.set_top_margin((py * zoom).round() as i32);
        edit.view.set_bottom_margin(0);
        let ink = if edit.filled { edit.color.on_ink() } else { edit.color.rgba() };
        let background = if edit.filled { edit.color.rgba().to_hex() } else { "transparent".into() };
        let radius = (edit.weight.font_size() * 0.45).min(frame.h / 2.0) * zoom;
        let class = &self.text_class;
        let css = format!(
            "textview.{class}, textview.{class} > text {{ background: transparent; color: {fg}; caret-color: {fg}; \
             font-family: \"Adwaita Sans\"; font-weight: 600; font-size: {size}px; padding: 0; }}\n\
             textview.{class} {{ background-color: {background}; border-radius: {radius}px; }}\n\
             textview.{class} > text selection {{ background-color: alpha({fg}, 0.25); }}",
            fg = ink.to_hex(),
            size = edit.weight.font_size() * zoom,
        );
        self.text_css.load_from_string(&css);
    }

    /// Ends text editing, keeping what was typed. Empty text removes the label.
    pub fn commit_text(&self) {
        let Some(edit) = self.text.borrow_mut().take() else { return };
        let buffer = edit.view.buffer();
        let string = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).trim().to_string();
        self.overlay.remove_overlay(&edit.view);
        {
            let mut model = self.model.borrow_mut();
            if let Some(id) = edit.id {
                model.begin_gesture();
                if string.is_empty() {
                    model.update(|list| list.retain(|a| a.id != id));
                    model.set_selected(None);
                } else {
                    let origin = edit.origin;
                    model.replace(id, |a| {
                        let mut c = a.clone();
                        c.kind = Kind::Text(origin, string.clone());
                        c
                    });
                }
            } else if !string.is_empty() {
                let a = Annotation::new(Kind::Text(edit.origin, string), edit.color, edit.weight, edit.filled);
                let id = a.id;
                model.perform(|list| list.push(a));
                model.set_selected(Some(id));
            }
        }
        self.canvas.grab_focus();
        self.sync();
    }

    // MARK: Output

    fn flattened(&self) -> Option<gdk::Texture> {
        self.commit_text();
        let renderer = self.canvas.native()?.renderer()?;
        let model = self.model.borrow();
        render::flatten(&renderer, &self.renderer, &model.base, model.scale, &model.annotations, model.scale)
    }

    /// The flattened image at output resolution (standard resolution when Settings asks for it).
    fn output_texture(&self, full: &gdk::Texture) -> gdk::Texture {
        let scale = self.model.borrow().scale;
        if self.settings.copy_at_point_size() && scale > 1.0 {
            if let Some(renderer) = self.canvas.native().and_then(|n| n.renderer()) {
                if let Some(t) = render::flatten(&renderer, &self.renderer, &BaseImage::new(full.clone()), scale, &[], 1.0) {
                    return t;
                }
            }
        }
        full.clone()
    }

    /// Encodes off the main thread, writes the result back to the capture so the preview and
    /// "annotate last" see it, then calls back with the PNG.
    fn commit_to_capture(self: &Rc<Self>, then: impl FnOnce(&Rc<Editor>, glib::Bytes, gdk::Texture) + 'static) {
        let Some(full) = self.flattened() else { return };
        let out = self.output_texture(&full);
        let (id, path) = {
            let c = self.capture.borrow();
            (c.id.clone(), c.path.clone())
        };
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let encode_out = out.clone();
            let Ok(png) = gio::spawn_blocking(move || encode_out.save_to_png_bytes()).await else { return };
            if let (Some(id), Some(path)) = (id, path) {
                if std::fs::write(&path, &png).is_ok() {
                    output::call_shell("CaptureEdited", &(id, path.to_string_lossy().to_string()).to_variant());
                }
            }
            if let Some(e) = weak.upgrade() {
                then(&e, png, out);
            }
        });
    }

    /// Drags the finished image out as a PNG file and as pixels.
    fn attach_drag_source(self: &Rc<Self>, widget: &impl IsA<gtk::Widget>) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        let staged: Rc<RefCell<Option<gdk::Texture>>> = Rc::new(RefCell::new(None));
        {
            let staged = staged.clone();
            source.connect_prepare(move |_, _, _| {
                let e = weak.upgrade()?;
                let full = e.flattened()?;
                let out = e.output_texture(&full);
                let png = out.save_to_png_bytes();
                let name = {
                    let c = e.capture.borrow();
                    let template = e.settings.string("filename-template", "Shotput %Y-%m-%d at %H.%M.%S");
                    format!("{}.png", output::file_name(&template, &c.date, c.app_name.as_deref()))
                };
                let path = crate::drag::stage_file(&png, &name)?;
                staged.replace(Some(out.clone()));
                Some(crate::drag::content(&path, &out))
            });
        }
        source.connect_drag_begin(move |_, drag| {
            if let Some(texture) = staged.borrow().as_ref() {
                crate::drag::set_icon(drag, texture);
            }
        });
        widget.add_controller(source);
    }

    fn copy(self: &Rc<Self>) {
        self.commit_to_capture(|e, png, tex| {
            output::copy_png(&png, &tex);
            if e.settings.closes_on_copy() {
                e.window.close();
            } else {
                e.confirm(|ui| ui.copy.clone(), "edit-copy-symbolic");
                e.show_toast("Copied");
            }
        });
    }

    fn done(self: &Rc<Self>) {
        // Hide straight away; the copy finishes a moment later in the background.
        self.commit_text();
        let window = self.window.clone();
        self.commit_to_capture(move |_, png, tex| {
            output::copy_png(&png, &tex);
            window.destroy();
        });
        self.window.set_visible(false);
    }

    fn save(self: &Rc<Self>) {
        let format = self.settings.string("save-format", "png");
        let format = if format == "jpeg" { "jpeg" } else { "png" }.to_string();
        let quality = self.settings.double("jpeg-quality", 0.85);
        let name = {
            let c = self.capture.borrow();
            let template = self.settings.string("filename-template", "Shotput %Y-%m-%d at %H.%M.%S");
            format!("{}.{}", output::file_name(&template, &c.date, c.app_name.as_deref()), output::extension_for(&format))
        };
        self.commit_to_capture(move |e, png, tex| {
            let data = if format == "png" { Some(png) } else { output::encode(&tex, &format, quality) };
            let Some(data) = data else {
                e.show_toast("Could not encode the image");
                return;
            };
            if let Some(folder) = output::save_folder(&e.settings) {
                let path = output::unique_path(&folder, &name);
                if std::fs::write(&path, &data).is_ok() {
                    e.saved(&path);
                    return;
                }
            }
            let dialog = gtk::FileDialog::new();
            dialog.set_initial_name(Some(&name));
            if let Some(pictures) = glib::user_special_dir(glib::UserDirectory::Pictures) {
                dialog.set_initial_folder(Some(&gio::File::for_path(pictures)));
            }
            let weak = Rc::downgrade(e);
            dialog.save(Some(&e.window), gio::Cancellable::NONE, move |result| {
                let (Ok(file), Some(e)) = (result, weak.upgrade()) else { return };
                let Some(path) = file.path() else { return };
                match std::fs::write(&path, &data) {
                    Ok(()) => e.saved(&path),
                    Err(err) => e.show_toast(&format!("Could not save: {err}")),
                }
            });
        });
    }

    fn saved(&self, path: &std::path::Path) {
        self.confirm(|ui| ui.save.clone(), "document-save-symbolic");
        let folder = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.show_toast(&format!("Saved to {folder}"));
    }

    /// Reads the text in the screenshot on this machine and copies it.
    fn copy_recognized_text(self: &Rc<Self>) {
        let texture = self.model.borrow().base.texture.clone();
        let path = output::runtime_dir().join(format!("ocr-{}.png", new_id()));
        if output::write_ocr_input(&texture, &path).is_none() {
            return;
        }
        if output::tesseract().is_none() {
            self.show_toast("Text recognition needs Tesseract");
            return;
        }
        self.show_toast("Reading text…");
        let keeps = self.settings.boolean("ocr-keeps-line-breaks", true);
        let language = self.settings.string("ocr-language", "eng");
        let weak = Rc::downgrade(self);
        output::recognize_text(&path.clone(), keeps, &language, move |result| {
            let _ = std::fs::remove_file(&path);
            let Some(e) = weak.upgrade() else { return };
            match result {
                Some((text, lines)) => {
                    output::copy_text(&text);
                    e.confirm(|ui| ui.read_text.clone(), "shotput-read-text-symbolic");
                    e.show_toast(&if lines == 1 { "Copied 1 line of text".to_string() } else { format!("Copied {lines} lines of text") });
                }
                None => e.show_toast("No text found"),
            }
        });
    }

    // MARK: Feedback

    /// Shows a check in place of the button's icon for a moment, so the action visibly landed.
    fn confirm(&self, button: impl Fn(&Ui) -> gtk::Button, icon: &'static str) {
        let Some(b) = self.ui.borrow().as_ref().map(button) else { return };
        b.set_icon_name("object-select-symbolic");
        glib::timeout_add_local_once(std::time::Duration::from_millis(1400), move || b.set_icon_name(icon));
    }

    fn show_toast(&self, message: &str) {
        let ui = self.ui.borrow();
        let Some(ui) = ui.as_ref() else { return };
        ui.toast.set_text(message);
        ui.toast_revealer.set_reveal_child(true);
        let generation = self.toast_generation.get() + 1;
        self.toast_generation.set(generation);
        let revealer = ui.toast_revealer.clone();
        let current = self.toast_generation.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1600), move || {
            // Only the latest toast hides it.
            if current.get() == generation {
                revealer.set_reveal_child(false);
            }
        });
    }
}

// MARK: Development harness

impl Editor {
    /// Adds one mark of every kind, for renders of the editor.
    pub fn dev_populate(&self) {
        let mut m = self.model.borrow_mut();
        let mut add = |kind: Kind, color: Ink, weight: Weight, filled: bool, bend: Option<P>| {
            let mut a = Annotation::new(kind, color, weight, filled);
            a.bend = bend;
            m.annotations.push(a);
        };
        add(Kind::Ellipse(R::new(20.0, 70.0, 150.0, 150.0)), Ink::Green, Weight::Regular, false, None);
        add(Kind::Rectangle(R::new(235.0, 80.0, 340.0, 80.0)), Ink::Red, Weight::Regular, false, None);
        add(Kind::Highlight(R::new(240.0, 175.0, 240.0, 22.0)), Ink::Yellow, Weight::Regular, false, None);
        add(Kind::Redact(R::new(260.0, 250.0, 220.0, 70.0)), Ink::Black, Weight::Regular, false, None);
        add(Kind::Redact(R::new(500.0, 330.0, 80.0, 20.0)), Ink::Black, Weight::Regular, true, None);
        add(Kind::Counter(p(215.0, 95.0), 1), Ink::Blue, Weight::Regular, false, None);
        add(Kind::Counter(p(215.0, 265.0), 2), Ink::Blue, Weight::Regular, false, None);
        add(Kind::Text(p(400.0, 260.0), "Wrong colour".into()), Ink::Red, Weight::Regular, true, None);
        add(Kind::Text(p(40.0, 330.0), "Unfilled label".into()), Ink::White, Weight::Heavy, false, None);
        add(Kind::Arrow(p(470.0, 310.0), p(560.0, 380.0)), Ink::Red, Weight::Heavy, false, None);
        add(Kind::Arrow(p(60.0, 250.0), p(180.0, 300.0)), Ink::Violet, Weight::Regular, false, Some(p(80.0, 330.0)));
        add(Kind::Line(p(600.0, 60.0), p(640.0, 200.0)), Ink::Orange, Weight::Light, false, Some(p(680.0, 120.0)));
        let wave: Vec<P> = (0..60).map(|i| p(30.0 + i as f64 * 2.5, 400.0 + (i as f64 / 6.0).sin() * 14.0)).collect();
        add(Kind::Pen(wave), Ink::Violet, Weight::Regular, false, None);
        let last = m.annotations[9].id;
        m.set_selected(Some(last));
        drop(m);
        self.sync();
        self.show_toast("Copied");
    }

    /// Marks for the website's picture of the editor, placed on site/demo.html's screenshot.
    pub fn dev_showcase(&self) {
        let mut m = self.model.borrow_mut();
        let mut add = |kind: Kind, color: Ink, weight: Weight, filled: bool, bend: Option<P>| {
            let mut a = Annotation::new(kind, color, weight, filled);
            a.bend = bend;
            m.annotations.push(a);
            m.annotations.last().map(|a| a.id)
        };
        add(Kind::Highlight(R::new(286.0, 452.0, 436.0, 24.0)), Ink::Yellow, Weight::Regular, false, None);
        add(Kind::Redact(R::new(772.0, 449.0, 178.0, 108.0)), Ink::Black, Weight::Regular, false, None);
        add(Kind::Redact(R::new(890.0, 213.0, 168.0, 22.0)), Ink::Black, Weight::Regular, false, None);
        add(Kind::Rectangle(R::new(626.0, 287.0, 166.0, 56.0)), Ink::Red, Weight::Regular, false, None);
        add(Kind::Counter(p(626.0, 287.0), 1), Ink::Blue, Weight::Regular, false, None);
        add(Kind::Counter(p(272.0, 464.0), 2), Ink::Blue, Weight::Regular, false, None);
        add(Kind::Text(p(640.0, 628.0), "Button is off-centre at 1200px".into()), Ink::Red, Weight::Regular, true, None);
        add(Kind::Text(p(640.0, 684.0), "Card is valid, so why overdue?".into()), Ink::Blue, Weight::Regular, true, None);
        add(Kind::Arrow(p(905.0, 622.0), p(770.0, 352.0)), Ink::Red, Weight::Regular, false, Some(p(885.0, 480.0)));
        let selected = m.annotations.last().map(|a| a.id);
        m.set_selected(selected);
        drop(m);
        self.sync();
    }

    /// Renders the window and the flattened export to PNG files.
    pub fn dev_render(&self, window_png: &std::path::Path, export_png: &std::path::Path) -> bool {
        let Some(renderer) = self.window.native().and_then(|n| n.renderer()) else { return false };
        let (w, h) = (self.window.width(), self.window.height());
        // SHOTPUT_DEV_SCALE renders the window sharper than the screen, for the website.
        let k: f64 = std::env::var("SHOTPUT_DEV_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
        let paintable = gtk::WidgetPaintable::new(Some(&self.window));
        let snapshot = gtk::Snapshot::new();
        snapshot.scale(k as f32, k as f32);
        paintable.snapshot(&snapshot, w as f64, h as f64);
        let Some(node) = snapshot.to_node() else { return false };
        let shot = renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, (w as f64 * k) as f32, (h as f64 * k) as f32)));
        let ok = shot.save_to_png(window_png).is_ok();
        let model = self.model.borrow();
        let flat = render::flatten(&renderer, &self.renderer, &model.base, model.scale, &model.annotations, model.scale);
        ok && flat.is_some_and(|t| t.save_to_png(export_png).is_ok())
    }
}

/// A synthetic screenshot: a window with text lines and a button, for renders and tests.
pub fn dev_sample_image() -> gdk::Texture {
    let (w, h) = (720, 440);
    let surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h).expect("surface");
    {
        let cr = gtk::cairo::Context::new(&surface).expect("context");
        cr.set_source_rgb(0.95, 0.94, 0.93);
        cr.paint().ok();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(200.0, 60.0, 520.0, 380.0);
        cr.fill().ok();
        cr.set_source_rgb(0.4, 0.4, 0.38);
        for i in 0..8 {
            let y = 90.0 + i as f64 * 40.0;
            cr.rectangle(240.0, y, 300.0 - (i % 3) as f64 * 50.0, 7.0);
            cr.fill().ok();
        }
        cr.set_source_rgb(0.66, 0.65, 0.62);
        for i in 0..9 {
            cr.rectangle(30.0, 90.0 + i as f64 * 32.0, 140.0 - (i % 2) as f64 * 30.0, 9.0);
            cr.fill().ok();
        }
        cr.set_source_rgb(0.12, 0.44, 0.9);
        render::rounded_rect(&cr, R::new(560.0, 360.0, 130.0, 40.0), 8.0);
        cr.fill().ok();
    }
    crate::backdrop::surface_to_texture(surface).expect("texture")
}

/// Every open editor, for the app to find the one to raise.
pub fn count() -> usize {
    EDITORS.with(|list| list.borrow().len())
}

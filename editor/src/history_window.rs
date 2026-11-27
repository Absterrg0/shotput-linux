//! The clipboard history in a small window: recent captures and their edited versions, newest
//! first. Click one to copy it again. On GNOME the same list is in the top bar menu; this window
//! is how KDE Plasma, Hyprland and Sway reach it, through `shotput history` or its shortcut.

use crate::history::{self, Entry, Store};
use crate::settings::Settings;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

const CELL: (i32, i32) = (192, 120);

struct Open {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    flow: gtk::FlowBox,
    status: adw::StatusPage,
    clear: gtk::Button,
    _monitor: Option<gio::FileMonitor>,
}

thread_local! {
    static OPEN: RefCell<Option<Rc<Open>>> = const { RefCell::new(None) };
}

fn relative_time(unix: i64) -> String {
    let now = glib::DateTime::now_utc().map(|d| d.to_unix()).unwrap_or(unix);
    let seconds = (now - unix).max(0);
    match seconds {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{} min. ago", s / 60),
        s if s < 86400 => format!("{} hr. ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

/// Puts a history item back on the clipboard, the same way a capture is copied.
pub fn copy_entry(entry: &Entry) -> bool {
    let store = Store::open_default();
    let Ok(data) = std::fs::read(store.path(&entry.id)) else { return false };
    let bytes = glib::Bytes::from_owned(data);
    let Ok(texture) = gdk::Texture::from_bytes(&bytes) else { return false };
    if crate::desktop::captures_natively() {
        let capture = crate::native::Capture {
            id: entry.id.clone(),
            texture,
            path: store.path(&entry.id),
            scale: entry.scale,
            app_name: entry.app_name.clone(),
            date: glib::DateTime::now_local().unwrap_or_else(|_| glib::DateTime::now_utc().expect("time")),
        };
        crate::native::copy_capture(&capture, &Settings::new());
    } else {
        crate::output::copy_png(&bytes, &texture);
    }
    true
}

pub fn open(app: &adw::Application) {
    if let Some(open) = OPEN.with(|o| o.borrow().clone()) {
        rebuild(&open);
        focus_newest(&open);
        open.window.present();
        return;
    }
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Clipboard History")
        .default_width(680)
        .default_height(500)
        .build();
    window.add_css_class("shotput-history");

    let header = adw::HeaderBar::new();
    let clear = gtk::Button::with_label("Clear");
    clear.set_tooltip_text(Some("Clear the history"));
    header.pack_start(&clear);

    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(true)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(6)
        .row_spacing(14)
        .column_spacing(14)
        .valign(gtk::Align::Start)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&flow).vexpand(true).build();
    let footer = gtk::Label::new(Some("Click to copy. Middle-click or Ctrl+E to annotate. Delete removes."));
    footer.add_css_class("dim-label");
    footer.add_css_class("caption");
    footer.set_margin_bottom(10);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.append(&scroll);
    list.append(&footer);

    let status = adw::StatusPage::builder().icon_name("edit-paste-symbolic").build();
    let stack = gtk::Stack::new();
    stack.add_named(&list, Some("list"));
    stack.add_named(&status, Some("empty"));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&stack));
    window.set_content(Some(&view));

    // The extension may change the list on GNOME; follow it.
    let monitor = gio::File::for_path(Store::open_default().index_path()).monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE).ok();
    if let Some(m) = &monitor {
        m.connect_changed(|_, _, _, event| {
            if matches!(event, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created | gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::Renamed | gio::FileMonitorEvent::MovedIn) {
                refresh();
            }
        });
    }

    let open = Rc::new(Open { window: window.clone(), stack, flow: flow.clone(), status, clear: clear.clone(), _monitor: monitor });
    clear.connect_clicked(|_| {
        Store::open_default().clear();
        refresh();
    });
    {
        let window = window.downgrade();
        flow.connect_child_activated(move |_, child| {
            let Some(entry) = entry_of(child) else { return };
            if copy_entry(&entry) {
                if let Some(w) = window.upgrade() {
                    w.close();
                }
            }
        });
    }

    // Esc closes; Ctrl+E annotates and Delete removes the selected item.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let (window, flow, app) = (window.downgrade(), flow.downgrade(), app.downgrade());
        keys.connect_key_pressed(move |_, key, _, state| {
            let selected = flow.upgrade().and_then(|f| f.selected_children().into_iter().next()).and_then(|c| entry_of(&c));
            match key {
                gdk::Key::Escape => {
                    if let Some(w) = window.upgrade() {
                        w.close();
                    }
                }
                gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                    let Some(entry) = selected else { return glib::Propagation::Proceed };
                    if copy_entry(&entry) {
                        if let Some(w) = window.upgrade() {
                            w.close();
                        }
                    }
                }
                gdk::Key::Delete | gdk::Key::KP_Delete => {
                    let Some(entry) = selected else { return glib::Propagation::Proceed };
                    Store::open_default().remove(&entry.id);
                    refresh();
                }
                gdk::Key::e | gdk::Key::E if state.contains(gdk::ModifierType::CONTROL_MASK) => {
                    let (Some(entry), Some(app)) = (selected, app.upgrade()) else { return glib::Propagation::Proceed };
                    annotate(&app, &entry, window.upgrade());
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
    }
    window.add_controller(keys);
    window.connect_destroy(|_| {
        OPEN.with(|o| o.borrow_mut().take());
    });
    OPEN.with(|o| *o.borrow_mut() = Some(open.clone()));
    rebuild(&open);
    window.present();
    focus_newest(&open);
}

/// The newest item is selected, so Return copies it straight away.
fn focus_newest(open: &Open) {
    if let Some(first) = open.flow.child_at_index(0) {
        open.flow.select_child(&first);
        first.grab_focus();
    }
}

/// Rebuilds an open history window, after the list changed.
pub fn refresh() {
    if let Some(open) = OPEN.with(|o| o.borrow().clone()) {
        rebuild(&open);
    }
}

fn entry_of(child: &gtk::FlowBoxChild) -> Option<Entry> {
    let id = child.widget_name();
    Store::open_default().entries().into_iter().find(|e| e.id == id.as_str())
}

fn annotate(app: &adw::Application, entry: &Entry, window: Option<adw::ApplicationWindow>) {
    if let Some(w) = window {
        w.close();
    }
    crate::native::open_history_entry(app, entry);
}

fn rebuild(open: &Rc<Open>) {
    while let Some(child) = open.flow.first_child() {
        open.flow.remove(&child);
    }
    let settings = Settings::new();
    let entries = Store::open_default().entries();
    let on = history::enabled(&settings);
    open.clear.set_visible(!entries.is_empty());
    if entries.is_empty() {
        if on {
            open.status.set_title("Nothing here yet");
            open.status.set_description(Some("Captures and their edits show up here, newest first, so you can copy them again."));
        } else {
            open.status.set_title("Clipboard history is off");
            open.status.set_description(Some("Turn it on in Shotput's settings, under Clipboard and files."));
        }
        open.stack.set_visible_child_name("empty");
        return;
    }
    open.stack.set_visible_child_name("list");
    let store = Store::open_default();
    for entry in entries {
        let child = gtk::FlowBoxChild::new();
        child.set_widget_name(&entry.id);
        child.add_css_class("shotput-history-item");
        let thumb = if store.thumb_path(&entry.id).is_file() { store.thumb_path(&entry.id) } else { store.path(&entry.id) };
        let picture = gtk::Picture::for_filename(&thumb);
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_can_shrink(true);
        picture.set_size_request(CELL.0, CELL.1);
        picture.add_css_class("shotput-history-thumb");
        picture.set_overflow(gtk::Overflow::Hidden);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        buttons.set_halign(gtk::Align::End);
        buttons.set_valign(gtk::Align::Start);
        buttons.set_margin_top(6);
        buttons.set_margin_end(6);
        buttons.set_opacity(0.0);
        let make = |icon: &str, tip: &str| {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(tip));
            b.add_css_class("shotput-glass");
            b.set_focus_on_click(false);
            buttons.append(&b);
            b
        };
        let annotate_button = make("shotput-annotate-symbolic", "Annotate");
        let remove_button = make("window-close-symbolic", "Remove");
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&picture));
        overlay.add_overlay(&buttons);

        let motion = gtk::EventControllerMotion::new();
        {
            let b = buttons.clone();
            motion.connect_enter(move |_, _, _| b.set_opacity(1.0));
            let b = buttons.clone();
            motion.connect_leave(move |_| b.set_opacity(0.0));
        }
        overlay.add_controller(motion);

        let label = gtk::Label::new(Some(&[entry.app_name.clone().unwrap_or_default(), relative_time(entry.date)].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(", ")));
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        column.append(&overlay);
        column.append(&label);
        child.set_child(Some(&column));

        // Middle-click annotates, like the top bar on GNOME.
        let middle = gtk::GestureClick::new();
        middle.set_button(gdk::BUTTON_MIDDLE);
        {
            let (entry, window) = (entry.clone(), open.window.downgrade());
            middle.connect_released(move |_, _, _, _| {
                if let Some(w) = window.upgrade() {
                    if let Some(app) = w.application().and_downcast::<adw::Application>() {
                        annotate(&app, &entry, Some(w));
                    }
                }
            });
        }
        child.add_controller(middle);
        {
            let (entry, window) = (entry.clone(), open.window.downgrade());
            annotate_button.connect_clicked(move |_| {
                if let Some(w) = window.upgrade() {
                    if let Some(app) = w.application().and_downcast::<adw::Application>() {
                        annotate(&app, &entry, Some(w));
                    }
                }
            });
        }
        {
            let id = entry.id.clone();
            remove_button.connect_clicked(move |_| {
                Store::open_default().remove(&id);
                refresh();
            });
        }
        open.flow.append(&child);
    }
}

pub const CSS: &str = r#"
.shotput-history-thumb { border-radius: 10px; box-shadow: inset 0 0 0 1px alpha(currentColor, 0.1); background-color: alpha(currentColor, 0.05); }
flowboxchild.shotput-history-item { padding: 6px; border-radius: 14px; }
"#;

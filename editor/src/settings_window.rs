//! Settings on desktops without GNOME Shell, where the extension's own settings cannot open.
//! The same keys as the extension's settings, grouped the same way.

use crate::settings::Settings;
use adw::prelude::*;
use gtk::{gio, glib};

thread_local! {
    static OPEN: std::cell::RefCell<Option<adw::PreferencesWindow>> = const { std::cell::RefCell::new(None) };
}

fn switch(settings: &gio::Settings, key: &str, title: &str, subtitle: &str) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).subtitle(subtitle).build();
    settings.bind(key, &row, "active").build();
    row
}

/// A choice between string values of one key.
fn choice(settings: &gio::Settings, key: &str, title: &str, options: &[(&str, &str)]) -> adw::ComboRow {
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    let row = adw::ComboRow::builder().title(title).model(&gtk::StringList::new(&labels)).build();
    let values: std::rc::Rc<Vec<String>> = std::rc::Rc::new(options.iter().map(|(v, _)| v.to_string()).collect());
    let key = key.to_string();
    let sync = {
        let (row, values, key) = (row.clone(), values.clone(), key.clone());
        move |s: &gio::Settings| {
            let current = s.string(&key);
            if let Some(i) = values.iter().position(|v| *v == current.as_str()) {
                row.set_selected(i as u32);
            }
        }
    };
    sync(settings);
    settings.connect_changed(Some(&key), move |s, _| sync(s));
    let s = settings.clone();
    row.connect_selected_notify(move |r| {
        if let Some(v) = values.get(r.selected() as usize) {
            if s.string(&key) != v.as_str() {
                let _ = s.set_string(&key, v);
            }
        }
    });
    row
}

fn number(settings: &gio::Settings, key: &str, title: &str, subtitle: &str, min: f64, max: f64, step: f64, digits: u32) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(min, max, step);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_digits(digits);
    settings.bind(key, &row, "value").build();
    row
}

pub fn open(app: &adw::Application) {
    if let Some(w) = OPEN.with(|o| o.borrow().clone()) {
        w.present();
        return;
    }
    let store = Settings::new();
    let Some(settings) = store.gio().cloned() else {
        crate::notify(app, "Shotput's settings are missing", "Run the installer again.");
        return;
    };
    let window = adw::PreferencesWindow::builder().application(app).title("Shotput").default_width(620).default_height(720).build();
    window.set_search_enabled(true);

    // Capture
    let page = adw::PreferencesPage::builder().title("Capture").icon_name("camera-photo-symbolic").build();
    let group = adw::PreferencesGroup::builder().title("After a capture").build();
    group.add(&choice(&settings, "after-capture", "Then", &[("preview", "Show a preview"), ("editor", "Open the editor"), ("clipboard", "Just copy")]));
    group.add(&choice(
        &settings,
        "preview-corner",
        "Preview corner",
        &[("automatic", "Bottom right"), ("bottom-left", "Bottom left"), ("top-right", "Top right"), ("top-left", "Top left")],
    ));
    group.add(&number(&settings, "preview-duration", "Preview stays", "Seconds. 0 keeps it until you dismiss it.", 0.0, 60.0, 1.0, 0));
    group.add(&switch(&settings, "plays-sound", "Play a sound", "Needs canberra-gtk-play."));
    page.add(&group);
    let group = adw::PreferencesGroup::builder().title("Selecting").build();
    group.add(&switch(&settings, "shows-magnifier", "Show the magnifier", "A close-up of the pixels under the pointer."));
    group.add(&switch(&settings, "snaps-to-edges", "Snap to edges", "Window and screen edges. Hold Ctrl to place freely."));
    group.add(&choice(&settings, "selection-hints", "Hints", &[("first-few", "The first few times"), ("always", "Always"), ("never", "Never")]));
    page.add(&group);
    window.add(&page);

    // Output
    let page = adw::PreferencesPage::builder().title("Output").icon_name("folder-symbolic").build();
    let group = adw::PreferencesGroup::builder().title("Saving").build();
    group.add(&switch(&settings, "saves-every-capture", "Save every capture", "As well as copying it."));
    let folder = adw::ActionRow::builder().title("Save folder").build();
    let sync_folder = {
        let folder = folder.clone();
        move |s: &gio::Settings| {
            let f = s.string("save-folder");
            folder.set_subtitle(if f.is_empty() { "Pictures/Screenshots" } else { f.as_str() });
        }
    };
    sync_folder(&settings);
    settings.connect_changed(Some("save-folder"), move |s, _| sync_folder(s));
    let choose = gtk::Button::builder().label("Choose…").valign(gtk::Align::Center).build();
    {
        let settings = settings.clone();
        let window = window.clone();
        choose.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder().title("Save folder").build();
            let settings = settings.clone();
            dialog.select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
                if let Some(path) = result.ok().and_then(|f| f.path()) {
                    let _ = settings.set_string("save-folder", &path.to_string_lossy());
                }
            });
        });
    }
    folder.add_suffix(&choose);
    group.add(&folder);
    group.add(&choice(&settings, "save-format", "Format", &[("png", "PNG"), ("jpeg", "JPEG")]));
    group.add(&number(&settings, "jpeg-quality", "JPEG quality", "", 0.1, 1.0, 0.05, 2));
    let template = adw::EntryRow::builder().title("File name").build();
    settings.bind("filename-template", &template, "text").build();
    group.add(&template);
    page.add(&group);
    let group = adw::PreferencesGroup::new();
    group.add(&switch(&settings, "copy-at-point-size", "Standard resolution", "Copy and save HiDPI captures at their on-screen size."));
    page.add(&group);
    window.add(&page);

    // Editor
    let page = adw::PreferencesPage::builder().title("Editor").icon_name("document-edit-symbolic").build();
    let group = adw::PreferencesGroup::new();
    group.add(&choice(&settings, "escape-in-editor", "Esc in the editor", &[("discard", "Closes without copying"), ("copy", "Copies and closes")]));
    group.add(&switch(&settings, "editor-closes-on-copy", "Ctrl+C closes the editor", ""));
    group.add(&switch(&settings, "recognizes-shapes", "Turn sketches into shapes", "Pen strokes that look like a line, arrow, box or circle become one."));
    group.add(&switch(&settings, "ocr-keeps-line-breaks", "Copy text keeps line breaks", ""));
    page.add(&group);
    window.add(&page);

    // Shortcuts
    let page = adw::PreferencesPage::builder().title("Shortcuts").icon_name("preferences-desktop-keyboard-shortcuts-symbolic").build();
    let group = adw::PreferencesGroup::builder()
        .description(match crate::desktop::current() {
            crate::desktop::Desktop::Kde => "Shotput's shortcuts are KDE Plasma shortcuts: change them in System Settings → Keyboard → Shortcuts → Shotput.",
            crate::desktop::Desktop::Sway => "Sway has no shortcut service for apps. Bind the commands in your config, for example:\nbindsym Print exec shotput area",
            _ => "Your desktop keeps Shotput's shortcuts. You can also bind the commands below to any key.",
        })
        .build();
    let change = adw::ActionRow::builder().title("Change shortcuts").activatable(true).build();
    change.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    change.connect_activated(|_| {
        if crate::desktop::current() == crate::desktop::Desktop::Kde && glib::find_program_in_path("systemsettings").is_some() {
            let _ = gio::Subprocess::newv(&["systemsettings".as_ref(), "kcm_keys".as_ref()], gio::SubprocessFlags::NONE);
        } else {
            crate::shortcuts::configure();
        }
    });
    group.add(&change);
    page.add(&group);
    let commands = adw::PreferencesGroup::builder().title("Commands").build();
    for (cmd, what) in [
        ("shotput area", "Capture area"),
        ("shotput annotate", "Capture area and annotate"),
        ("shotput window", "Capture window"),
        ("shotput screen", "Capture screen"),
        ("shotput text", "Copy text from area"),
        ("shotput last", "Annotate last capture"),
    ] {
        let row = adw::ActionRow::builder().title(what).subtitle(cmd).build();
        row.add_css_class("property");
        commands.add(&row);
    }
    page.add(&commands);
    window.add(&page);

    // About
    let page = adw::PreferencesPage::builder().title("About").icon_name("help-about-symbolic").build();
    let group = adw::PreferencesGroup::new();
    let status = adw::StatusPage::builder()
        .icon_name(crate::APP_ID)
        .title("Shotput for Linux")
        .description(format!(
            "Version {}\n\nScreenshots for pasting. Free and open source under the MIT licence. Nothing you capture leaves this machine.\n\nShotput was designed and built for macOS by keshav-exe. This is a port of it to Linux.",
            env!("CARGO_PKG_VERSION")
        ))
        .build();
    group.add(&status);
    let links = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    links.set_halign(gtk::Align::Center);
    for (label, url) in [
        ("Website", "https://shotput.abstergo.fyi"),
        ("Original Shotput", "https://github.com/keshav-exe/shotput"),
        ("Report an Issue", "https://github.com/Absterrg0/shotput-linux/issues/new"),
    ] {
        let b = gtk::Button::with_label(label);
        let url = url.to_string();
        b.connect_clicked(move |b| {
            gtk::UriLauncher::new(&url).launch(b.root().and_downcast_ref::<gtk::Window>(), gio::Cancellable::NONE, |_| {});
        });
        links.append(&b);
    }
    group.add(&links);
    page.add(&group);
    window.add(&page);

    window.connect_destroy(|_| {
        OPEN.with(|o| o.borrow_mut().take());
    });
    OPEN.with(|o| *o.borrow_mut() = Some(window.clone()));
    window.present();
}

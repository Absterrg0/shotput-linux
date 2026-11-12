//! Preferences shared with the GNOME Shell extension, which owns the GSettings schema.

use gtk::gio;
use gtk::prelude::*;
use std::path::PathBuf;

pub const SCHEMA: &str = "org.gnome.shell.extensions.shotput";
pub const EXTENSION_UUID: &str = "shotput@shotput.github.io";

pub struct Settings(Option<gio::Settings>);

fn schema_dirs() -> Vec<PathBuf> {
    let data = gtk::glib::user_data_dir();
    vec![
        data.join("gnome-shell/extensions").join(EXTENSION_UUID).join("schemas"),
        PathBuf::from("/usr/share/gnome-shell/extensions").join(EXTENSION_UUID).join("schemas"),
    ]
}

impl Settings {
    pub fn new() -> Settings {
        let default = gio::SettingsSchemaSource::default();
        if let Some(schema) = default.as_ref().and_then(|s| s.lookup(SCHEMA, true)) {
            return Settings(Some(gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)));
        }
        for dir in schema_dirs() {
            if !dir.join("gschemas.compiled").exists() {
                continue;
            }
            if let Ok(source) = gio::SettingsSchemaSource::from_directory(&dir, default.as_ref(), false) {
                if let Some(schema) = source.lookup(SCHEMA, false) {
                    return Settings(Some(gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)));
                }
            }
        }
        Settings(None)
    }

    fn has(&self, key: &str) -> Option<&gio::Settings> {
        self.0.as_ref().filter(|s| s.settings_schema().is_some_and(|schema| schema.has_key(key)))
    }

    pub fn string(&self, key: &str, default: &str) -> String {
        self.has(key).map(|s| s.string(key).to_string()).unwrap_or_else(|| default.to_string())
    }
    pub fn boolean(&self, key: &str, default: bool) -> bool {
        self.has(key).map(|s| s.boolean(key)).unwrap_or(default)
    }
    pub fn double(&self, key: &str, default: f64) -> f64 {
        self.has(key).map(|s| s.double(key)).unwrap_or(default)
    }
    pub fn int(&self, key: &str, default: i32) -> i32 {
        self.has(key).map(|s| s.int(key)).unwrap_or(default)
    }
    pub fn set_int(&self, key: &str, value: i32) {
        if let Some(s) = self.has(key) {
            let _ = s.set_int(key, value);
        }
    }

    pub fn escape_copies(&self) -> bool {
        self.string("escape-in-editor", "discard") == "copy"
    }
    pub fn closes_on_copy(&self) -> bool {
        self.boolean("editor-closes-on-copy", false)
    }
    pub fn recognizes_shapes(&self) -> bool {
        self.boolean("recognizes-shapes", true)
    }
    pub fn copy_at_point_size(&self) -> bool {
        self.boolean("copy-at-point-size", false)
    }
}

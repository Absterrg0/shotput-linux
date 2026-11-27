//! Clipboard history: recent captures and their edited versions, newest first, kept in
//! `~/.local/share/shotput/history` so they survive a restart. On GNOME the Shell extension
//! writes it (it owns captures and the clipboard); elsewhere this app does. Both read and write
//! the same `index.json`, and every change rereads it first, so either side can clear it.

use gtk::prelude::*;
use gtk::{gdk_pixbuf, glib};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Items kept when Settings has no say.
pub const DEFAULT_LIMIT: usize = 15;
/// The limits Settings allows.
pub const LIMITS: (i32, i32) = (5, 50);
/// Disk use is capped as well, so a run of 4K captures cannot fill it. The oldest go first.
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// Thumbnails cover twice the top bar cell (136 × 86), for sharp HiDPI menus.
const THUMB: (f64, f64) = (272.0, 172.0);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: String,
    /// Seconds since 1970 when it last changed.
    pub date: i64,
    #[serde(default)]
    pub app_name: Option<String>,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub width: i32,
    #[serde(default)]
    pub height: i32,
    /// Bytes on disk, image and thumbnail.
    #[serde(default)]
    pub size: u64,
}

fn one() -> f64 {
    1.0
}

/// Ids name files, so only plain ones are accepted from the index.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Drops entries past the count, then the oldest until the rest fit in `max_bytes`. The newest
/// always stays. Returns what was dropped, for its files to be deleted.
pub fn prune(entries: &mut Vec<Entry>, limit: usize, max_bytes: u64) -> Vec<Entry> {
    let limit = limit.max(1);
    let mut dropped = if entries.len() > limit { entries.split_off(limit) } else { Vec::new() };
    let mut total: u64 = entries.iter().map(|e| e.size).sum();
    while entries.len() > 1 && total > max_bytes {
        let last = entries.pop().expect("more than one");
        total -= last.size;
        dropped.insert(0, last);
    }
    dropped
}

/// Serialises writers in this process; the index itself is replaced atomically.
static LOCK: Mutex<()> = Mutex::new(());

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into() }
    }

    pub fn default_dir() -> PathBuf {
        glib::user_data_dir().join("shotput/history")
    }

    pub fn open_default() -> Store {
        Store::new(Self::default_dir())
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.png"))
    }

    pub fn thumb_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}-thumb.png"))
    }

    /// The items whose images are still there, newest first.
    pub fn entries(&self) -> Vec<Entry> {
        let Ok(data) = std::fs::read(self.index_path()) else { return Vec::new() };
        let list: Vec<Entry> = serde_json::from_slice(&data).unwrap_or_default();
        list.into_iter().filter(|e| valid_id(&e.id) && self.path(&e.id).is_file()).collect()
    }

    fn save(&self, entries: &[Entry]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_vec(entries).map_err(std::io::Error::other)?;
        write_atomic(&self.index_path(), &json)
    }

    fn delete_files(&self, id: &str) {
        let _ = std::fs::remove_file(self.path(id));
        let _ = std::fs::remove_file(self.thumb_path(id));
    }

    /// Adds an image at the front, or replaces the item with the same id and moves it there,
    /// so an edited capture updates its entry instead of adding another.
    pub fn record(&self, mut entry: Entry, png: &[u8], thumb: Option<&[u8]>, limit: usize, max_bytes: u64) -> std::io::Result<()> {
        if !valid_id(&entry.id) {
            return Err(std::io::Error::other("bad id"));
        }
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(&self.dir)?;
        set_private(&self.dir);
        let mut entries = self.entries();
        entries.retain(|e| e.id != entry.id);
        write_atomic(&self.path(&entry.id), png)?;
        entry.size = png.len() as u64;
        match thumb {
            Some(t) if write_atomic(&self.thumb_path(&entry.id), t).is_ok() => entry.size += t.len() as u64,
            _ => {
                let _ = std::fs::remove_file(self.thumb_path(&entry.id));
            }
        }
        entries.insert(0, entry);
        for gone in prune(&mut entries, limit, max_bytes) {
            self.delete_files(&gone.id);
        }
        self.save(&entries)
    }

    pub fn remove(&self, id: &str) {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = self.entries();
        entries.retain(|e| e.id != id);
        if valid_id(id) {
            self.delete_files(id);
        }
        let _ = self.save(&entries);
    }

    /// Empties the history, files the index lost track of included.
    pub fn clear(&self) {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = self.save(&[]);
        if let Ok(dir) = std::fs::read_dir(&self.dir) {
            for f in dir.flatten() {
                if f.path().extension().is_some_and(|e| e == "png") {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

fn set_private(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

/// The size of a PNG and a small version of it for the history, decoded off the main thread.
pub fn thumbnail(png: &[u8]) -> Option<(i32, i32, Vec<u8>)> {
    let loader = gdk_pixbuf::PixbufLoader::with_type("png").ok()?;
    loader.write(png).ok()?;
    loader.close().ok()?;
    let pixbuf = loader.pixbuf()?;
    let (w, h) = (pixbuf.width(), pixbuf.height());
    let k = (THUMB.0 / w.max(1) as f64).max(THUMB.1 / h.max(1) as f64).min(1.0);
    let (tw, th) = (((w as f64 * k).round() as i32).max(1), ((h as f64 * k).round() as i32).max(1));
    let small = if k < 1.0 { pixbuf.scale_simple(tw, th, gdk_pixbuf::InterpType::Bilinear)? } else { pixbuf };
    let bytes = small.save_to_bufferv("png", &[]).ok()?;
    Some((w, h, bytes))
}

/// Settings: whether history is kept, and how many items.
pub fn enabled(settings: &crate::settings::Settings) -> bool {
    settings.boolean("keeps-history", true)
}

pub fn limit(settings: &crate::settings::Settings) -> usize {
    settings.int("history-size", DEFAULT_LIMIT as i32).clamp(LIMITS.0, LIMITS.1) as usize
}

/// Records a PNG in the background (on desktops where this app owns the clipboard), then
/// refreshes an open history window. Does nothing with history turned off.
pub fn add(id: &str, png: glib::Bytes, app_name: Option<String>, scale: f64) {
    let settings = crate::settings::Settings::new();
    if !enabled(&settings) {
        return;
    }
    let limit = limit(&settings);
    let id = id.to_string();
    glib::MainContext::default().spawn_local(async move {
        let done = gtk::gio::spawn_blocking(move || {
            let (width, height, thumb) = match thumbnail(&png) {
                Some((w, h, t)) => (w, h, Some(t)),
                None => (0, 0, None),
            };
            let date = glib::DateTime::now_utc().map(|d| d.to_unix()).unwrap_or(0);
            let entry = Entry { id, date, app_name, scale, width, height, size: 0 };
            Store::open_default().record(entry, &png, thumb.as_deref(), limit, MAX_BYTES)
        })
        .await;
        if !matches!(done, Ok(Ok(()))) {
            eprintln!("shotput: could not add to the clipboard history");
        }
        crate::history_window::refresh();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, size: u64) -> Entry {
        Entry { id: id.into(), date: 0, app_name: None, scale: 1.0, width: 1, height: 1, size }
    }

    fn temp_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("shotput-history-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn prune_keeps_count_then_bytes() {
        let mut list: Vec<Entry> = (0..6).map(|i| entry(&format!("e{i}"), 10)).collect();
        let dropped = prune(&mut list, 4, u64::MAX);
        assert_eq!(list.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["e0", "e1", "e2", "e3"]);
        assert_eq!(dropped.len(), 2);

        let dropped = prune(&mut list, 10, 25);
        assert_eq!(list.len(), 2);
        assert_eq!(dropped.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["e2", "e3"]);

        // The newest stays even when it alone is over the cap.
        let mut big = vec![entry("a", 100), entry("b", 100)];
        prune(&mut big, 10, 50);
        assert_eq!(big.len(), 1);
        assert_eq!(big[0].id, "a");
    }

    #[test]
    fn ids_cannot_name_other_files() {
        assert!(valid_id("2f1c-uuid-ABC"));
        assert!(!valid_id("../index"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id(""));
    }

    #[test]
    fn record_updates_in_place_and_prunes_files() {
        let store = temp_store("record");
        for i in 0..4 {
            store.record(entry(&format!("c{i}"), 0), &[i as u8; 8], Some(&[1, 2]), 3, u64::MAX).unwrap();
        }
        let ids: Vec<String> = store.entries().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["c3", "c2", "c1"]);
        assert!(!store.path("c0").exists(), "pruned image is deleted");
        assert!(!store.thumb_path("c0").exists(), "pruned thumbnail is deleted");

        // An edit of c1 replaces its image and moves it to the front, without a duplicate.
        store.record(entry("c1", 0), b"edited", None, 3, u64::MAX).unwrap();
        let list = store.entries();
        assert_eq!(list.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["c1", "c3", "c2"]);
        assert_eq!(std::fs::read(store.path("c1")).unwrap(), b"edited");
        assert_eq!(list[0].size, 6);
        assert!(!store.thumb_path("c1").exists());

        store.remove("c3");
        assert_eq!(store.entries().len(), 2);
        assert!(!store.path("c3").exists());

        store.clear();
        assert!(store.entries().is_empty());
        assert!(!store.path("c1").exists());
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn disk_cap_drops_oldest() {
        let store = temp_store("bytes");
        for i in 0..5 {
            store.record(entry(&format!("b{i}"), 0), &[0u8; 100], None, 50, 250).unwrap();
        }
        let ids: Vec<String> = store.entries().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["b4", "b3"]);
        assert!(!store.path("b0").exists());
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn index_from_the_extension_reads() {
        // The GNOME extension writes the same file from JavaScript.
        let store = temp_store("js");
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(store.path("x1"), b"png").unwrap();
        std::fs::write(
            store.index_path(),
            br#"[{"id":"x1","date":1700000000,"appName":"Files","scale":2,"width":10,"height":5,"size":3},{"id":"gone","date":1}]"#,
        )
        .unwrap();
        let list = store.entries();
        assert_eq!(list.len(), 1, "entries without an image are skipped");
        assert_eq!(list[0].app_name.as_deref(), Some("Files"));
        assert_eq!(list[0].scale, 2.0);
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn thumbnail_covers_the_cell() {
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 1600, 400).unwrap();
        pixbuf.fill(0xff0000ff);
        let png = pixbuf.save_to_bufferv("png", &[]).unwrap();
        let (w, h, thumb) = thumbnail(&png).unwrap();
        assert_eq!((w, h), (1600, 400));
        let loader = gdk_pixbuf::PixbufLoader::new();
        loader.write(&thumb).unwrap();
        loader.close().unwrap();
        let t = loader.pixbuf().unwrap();
        assert_eq!(t.height(), 172);
        assert_eq!(t.width(), 688);
    }
}

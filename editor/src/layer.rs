//! Overlay windows on Wayland desktops other than GNOME, through gtk4-layer-shell when the system
//! has it. KDE Plasma, Hyprland, Sway and other wlroots compositors support the layer shell, which
//! puts a surface above every window and panel without a taskbar entry or an opening animation.
//!
//! gtk4-layer-shell has to be loaded before libwayland-client, so the app re-executes itself with
//! the library preloaded (see `preload`). Without it, overlays fall back to fullscreen windows.

use gtk::{gdk, glib};
use std::ffi::{CString, c_char, c_int, c_void};

const LIBRARY: &str = "libgtk4-layer-shell.so.0";
const SEARCH: [&str; 6] = [
    "/usr/lib64",
    "/usr/lib",
    "/usr/lib/x86_64-linux-gnu",
    "/usr/lib/aarch64-linux-gnu",
    "/usr/local/lib",
    "/usr/local/lib64",
];

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

pub fn library_path() -> Option<std::path::PathBuf> {
    SEARCH.iter().map(|dir| std::path::Path::new(dir).join(LIBRARY)).find(|p| p.exists())
}

/// Re-executes with gtk4-layer-shell preloaded, on Wayland desktops that are not GNOME. Runs
/// before GTK starts; returns normally when nothing needs doing.
pub fn preload(args: &[String]) {
    if std::env::var_os("SHOTPUT_LAYER_SHELL").is_some() || crate::desktop::current() == crate::desktop::Desktop::Gnome {
        return;
    }
    if std::env::var("XDG_SESSION_TYPE").as_deref() != Ok("wayland") && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }
    let Some(lib) = library_path() else { return };
    let mut preload = lib.to_string_lossy().to_string();
    if let Ok(existing) = std::env::var("LD_PRELOAD") {
        if !existing.is_empty() {
            preload = format!("{preload}:{existing}");
        }
    }
    let Ok(exe) = std::env::current_exe() else { return };
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(exe)
        .args(&args[1..])
        .env("LD_PRELOAD", preload)
        .env("SHOTPUT_LAYER_SHELL", "1")
        .exec();
    eprintln!("shotput: could not load the layer shell: {err}");
}

fn symbol<T>(name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    // RTLD_DEFAULT: the preloaded library is already in the process.
    let ptr = unsafe { dlsym(std::ptr::null_mut(), c.as_ptr()) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
    }
}

type InitFn = unsafe extern "C" fn(*mut c_void);
type SupportedFn = unsafe extern "C" fn() -> c_int;
type SetIntFn = unsafe extern "C" fn(*mut c_void, c_int);
type SetEdgeFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
type SetMonitorFn = unsafe extern "C" fn(*mut c_void, *mut c_void);
type SetNamespaceFn = unsafe extern "C" fn(*mut c_void, *const c_char);

pub enum Layer {
    Overlay = 3,
}
pub enum Edge {
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}
pub enum Keyboard {
    None = 0,
    Exclusive = 1,
}

/// True when windows can be layer surfaces in this session.
pub fn available() -> bool {
    thread_local!(static AVAILABLE: bool = {
        let supported: Option<SupportedFn> = symbol("gtk_layer_is_supported");
        match supported {
            Some(f) => unsafe { f() != 0 },
            None => false,
        }
    });
    AVAILABLE.with(|a| *a)
}

fn raw(window: &gtk::Window) -> *mut c_void {
    use glib::translate::ToGlibPtr;
    let ptr: *mut gtk::ffi::GtkWindow = window.to_glib_none().0;
    ptr as *mut c_void
}

/// Makes `window` a layer surface. Call before the window is shown.
pub struct Surface<'a>(&'a gtk::Window);

impl<'a> Surface<'a> {
    pub fn init(window: &'a gtk::Window, namespace: &str) -> Option<Surface<'a>> {
        if !available() {
            return None;
        }
        let init: InitFn = symbol("gtk_layer_init_for_window")?;
        unsafe { init(raw(window)) };
        if let (Some(set), Ok(ns)) = (symbol::<SetNamespaceFn>("gtk_layer_set_namespace"), CString::new(namespace)) {
            unsafe { set(raw(window), ns.as_ptr()) };
        }
        Some(Surface(window))
    }

    pub fn layer(&self, layer: Layer) -> &Self {
        if let Some(f) = symbol::<SetIntFn>("gtk_layer_set_layer") {
            unsafe { f(raw(self.0), layer as c_int) };
        }
        self
    }

    pub fn anchor(&self, edge: Edge, on: bool) -> &Self {
        if let Some(f) = symbol::<SetEdgeFn>("gtk_layer_set_anchor") {
            unsafe { f(raw(self.0), edge as c_int, on as c_int) };
        }
        self
    }

    pub fn margin(&self, edge: Edge, margin: i32) -> &Self {
        if let Some(f) = symbol::<SetEdgeFn>("gtk_layer_set_margin") {
            unsafe { f(raw(self.0), edge as c_int, margin) };
        }
        self
    }

    pub fn keyboard(&self, mode: Keyboard) -> &Self {
        if let Some(f) = symbol::<SetIntFn>("gtk_layer_set_keyboard_mode") {
            unsafe { f(raw(self.0), mode as c_int) };
        }
        self
    }

    /// -1 covers panels and docks instead of making room for them.
    pub fn exclusive_zone(&self, zone: i32) -> &Self {
        if let Some(f) = symbol::<SetIntFn>("gtk_layer_set_exclusive_zone") {
            unsafe { f(raw(self.0), zone) };
        }
        self
    }

    pub fn monitor(&self, monitor: &gdk::Monitor) -> &Self {
        if let Some(f) = symbol::<SetMonitorFn>("gtk_layer_set_monitor") {
            use glib::translate::ToGlibPtr;
            let m: *mut gdk::ffi::GdkMonitor = monitor.to_glib_none().0;
            unsafe { f(raw(self.0), m as *mut c_void) };
        }
        self
    }
}

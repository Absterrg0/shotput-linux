// Helpers installed into the nested shell's global scope for the test script.
const Clutter = imports.gi.Clutter;
const seat = Clutter.get_default_backend().get_default_seat();
globalThis.tp = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
globalThis.tk = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
globalThis.now = () => Math.floor(imports.gi.GLib.get_monotonic_time());
globalThis.move = (x, y) => tp.notify_absolute_motion(now(), x, y);
globalThis.press = () => tp.notify_button(now(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.PRESSED);
globalThis.release = () => tp.notify_button(now(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED);
globalThis.key = (sym, down = true) => tk.notify_keyval(now(), sym, down ? Clutter.KeyState.PRESSED : Clutter.KeyState.RELEASED);
globalThis.sp = () => Main.extensionManager.lookup('shotput@shotput.github.io').stateObj;
globalThis.shot = name => { const f = imports.gi.Gio.File.new_for_path('/tmp/claude-1000/shotput-shots/' + name); try { f.delete(null); } catch {} new imports.gi.Shell.Screenshot().screenshot(false, f.create(0, null), () => {}); };
'ok';

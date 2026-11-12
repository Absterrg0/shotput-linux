// Shotput for GNOME Shell: screenshots for pasting. Press a shortcut, drag an area, and it is on
// the clipboard when you let go. A port of Shotput for macOS (MIT, keshav-exe/shotput).
//
// This extension runs inside the compositor, which is the only place on GNOME Wayland that can
// freeze the screen, snap to windows, float a preview without taking focus, own the clipboard,
// and keep a top bar icon. The annotation editor is a separate GTK 4 app (`shotput`), opened
// over D-Bus and kept resident so it appears instantly.

import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import GdkPixbuf from 'gi://GdkPixbuf';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import {SelectionOverlay} from './overlay.js';
import {PreviewController, Shelf, Hud, flash} from './preview.js';
import {PinStore, Indicator, acceleratorLabel} from './pins.js';

Gio._promisify(Shell.Screenshot.prototype, 'screenshot_stage_to_content');
Gio._promisify(Shell.Screenshot, 'composite_to_stream');
Gio._promisify(Gio.Subprocess.prototype, 'communicate_utf8_async');
Gio._promisify(Gio.Subprocess.prototype, 'wait_check_async');

const EDITOR_ID = 'io.github.shotput.Shotput';
const EDITOR_PATH = '/io/github/shotput/Shotput';

const BUS_IFACE = `<node>
  <interface name="io.github.shotput.Shell">
    <method name="Run"><arg type="s" direction="in" name="action"/></method>
    <method name="SetClipboardImage"><arg type="s" direction="in" name="path"/></method>
    <method name="SetClipboardText"><arg type="s" direction="in" name="text"/></method>
    <method name="CaptureEdited"><arg type="s" direction="in" name="id"/><arg type="s" direction="in" name="path"/></method>
    <method name="Hud"><arg type="s" direction="in" name="message"/></method>
  </interface>
</node>`;

const SHORTCUTS = {
    'shortcut-area': 'area',
    'shortcut-area-annotate': 'annotate',
    'shortcut-window': 'window',
    'shortcut-screen': 'screen',
    'shortcut-annotate-last': 'last',
    'shortcut-repeat-area': 'repeat',
    'shortcut-text': 'text',
    'shortcut-annotate-clipboard': 'clipboard',
};

// Other processes may only start captures that put a selection on screen. A silent full-screen
// capture, or repeating an area without asking, is shortcut only, so nothing can use Shotput to
// read the screen.
const BUS_ACTIONS = new Set(['area', 'annotate', 'window', 'last', 'text', 'clipboard', 'settings']);

function cacheDir() {
    const dir = GLib.build_filenamev([GLib.get_user_cache_dir(), 'shotput', 'captures']);
    GLib.mkdir_with_parents(dir, 0o700);
    return dir;
}

function runtimeDir() {
    const dir = GLib.build_filenamev([GLib.get_user_runtime_dir(), 'shotput']);
    GLib.mkdir_with_parents(dir, 0o700);
    return dir;
}

function pngBytes(pixbuf) {
    const [, buffer] = pixbuf.save_to_bufferv('png', [], []);
    return new GLib.Bytes(buffer);
}

/** One screenshot and where on screen it came from. */
class Capture {
    constructor({pixbuf, png, scale, screenRect = null, monitorIndex = -1, appName = null}) {
        this.id = GLib.uuid_string_random();
        this.date = GLib.DateTime.new_now_local();
        this.pixbuf = pixbuf;
        this.png = png ?? pngBytes(pixbuf);
        this.scale = scale;
        this.screenRect = screenRect;
        this.monitorIndex = monitorIndex;
        this.appName = appName;
        /** Set when the capture was put on a backdrop as it was taken: {plain, fill, padding}. */
        this.framed = null;
        this.path = GLib.build_filenamev([cacheDir(), `${this.id}.png`]);
        this._written = null;
    }

    get pointSize() {
        return {width: this.pixbuf.get_width() / this.scale, height: this.pixbuf.get_height() / this.scale};
    }

    /** Written once, for the editor, pins and "annotate last". */
    ensureFile() {
        if (!this._written) {
            this._written = new Promise(resolve => {
                const file = Gio.File.new_for_path(this.path);
                file.replace_contents_bytes_async(this.png, null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null,
                    (f, res) => {
                        try {
                            f.replace_contents_finish(res);
                        } catch (e) {
                            logError(e, 'Shotput: writing capture');
                        }
                        resolve(this.path);
                    });
            });
        }
        return this._written;
    }

    /** Replaces the pixels with the annotated result from the editor. */
    replaceFromFile(path) {
        const pixbuf = GdkPixbuf.Pixbuf.new_from_file(path);
        const [, data] = GLib.file_get_contents(path);
        this.pixbuf = pixbuf;
        this.png = new GLib.Bytes(data);
        // The marks are now part of the pixels, so the unframed original no longer matches.
        this.framed = null;
        if (path !== this.path) {
            this._written = null;
            this.ensureFile();
        } else {
            this._written = Promise.resolve(this.path);
        }
    }
}

/** Expands a template such as `Shotput %Y-%m-%d` into a file name without an extension. */
export function fileName(template, date, appName) {
    let result = '';
    for (let i = 0; i < template.length; i++) {
        const c = template[i];
        if (c !== '%') {
            result += c;
            continue;
        }
        const token = template[++i];
        if (token === undefined) {
            result += '%';
            break;
        }
        if ('YymbBdjAaHIpMS'.includes(token))
            result += date.format(`%${token}`) ?? '';
        else if (token === 'n')
            result += appName ?? 'Screen';
        else if (token === 'r')
            result += Array.from({length: 4}, () => 'abcdefghijkmnpqrstuvwxyz23456789'[GLib.random_int_range(0, 32)]).join('');
        else if (token === '%')
            result += '%';
        else
            result += `%${token}`;
    }
    const cleaned = result.replaceAll('/', '-').replaceAll(':', '.').trim();
    return cleaned && !cleaned.startsWith('.') ? cleaned : 'Shotput';
}

function uniquePath(folder, name) {
    const dot = name.lastIndexOf('.');
    const base = dot > 0 ? name.slice(0, dot) : name;
    const ext = dot > 0 ? name.slice(dot) : '';
    let candidate = GLib.build_filenamev([folder, name]);
    for (let n = 2; GLib.file_test(candidate, GLib.FileTest.EXISTS); n++)
        candidate = GLib.build_filenamev([folder, `${base} ${n}${ext}`]);
    return candidate;
}

export default class ShotputExtension extends Extension {
    enable() {
        this.settings = this.getSettings();
        this.pins = new PinStore();
        this.hud = new Hud(this.path);
        this.preview = new PreviewController(this);
        this.shelf = new Shelf(this);
        this.lastCapture = null;
        this._lastArea = null;
        this._overlay = null;
        this._starting = false;
        this._placement = null;
        this._signals = [];

        // The installer pointed a GNOME custom shortcut at `shotput` until this loaded; retire it
        // first, so the extension's own shortcut takes over the key.
        this._migrateCustomKeybindings();
        for (const [key, action] of Object.entries(SHORTCUTS)) {
            Main.wm.addKeybinding(key, this.settings, Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
                Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW | Shell.ActionMode.POPUP,
                () => this.run(action));
        }

        this._syncIndicator();
        this._signals.push([this.settings, this.settings.connect('changed::shows-panel-icon', () => this._syncIndicator())]);
        this._signals.push([global.display, global.display.connect('window-created', (_d, w) => this._onWindowCreated(w))]);

        this._dbus = Gio.DBusExportedObject.wrapJSObject(BUS_IFACE, {
            Run: action => {
                if (BUS_ACTIONS.has(action))
                    this.run(action);
            },
            SetClipboardImage: path => {
                const [, data] = GLib.file_get_contents(path);
                this.setClipboardBytes(new GLib.Bytes(data));
            },
            SetClipboardText: text => {
                St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, text);
            },
            CaptureEdited: (id, path) => {
                if (this.lastCapture?.id === id) {
                    try {
                        this.lastCapture.replaceFromFile(path);
                    } catch (e) {
                        logError(e, 'Shotput: reading edited capture');
                    }
                }
            },
            Hud: message => this.hud.show(message),
        });
        this._dbus.export(Gio.DBus.session, '/io/github/shotput/Shell');

        this._purgeOldCaptures();
        this._idle = GLib.timeout_add_seconds(GLib.PRIORITY_LOW, 4, () => {
            this._idle = 0;
            if (this.settings.get_boolean('keeps-editor-ready'))
                this._warmEditor();
            if (!this.settings.get_boolean('has-launched')) {
                this.settings.set_boolean('has-launched', true);
                const key = acceleratorLabel(this.settings.get_strv('shortcut-area')[0] ?? '');
                Main.notify('Shotput is ready',
                    key ? `Press ${key} and drag an area. It is on the clipboard when you let go.`
                        : 'Use the camera icon in the top bar to capture an area.');
            }
            return GLib.SOURCE_REMOVE;
        });
    }

    disable() {
        if (this._idle) {
            GLib.source_remove(this._idle);
            this._idle = 0;
        }
        this._overlay?.cancel();
        this._overlay = null;
        for (const key of Object.keys(SHORTCUTS))
            Main.wm.removeKeybinding(key);
        for (const [obj, id] of this._signals)
            obj.disconnect(id);
        this._signals = [];
        this._dbus?.unexport();
        this._dbus = null;
        this.preview?.destroy();
        this.shelf?.destroy();
        this.hud?.destroy();
        this._indicator?.destroy();
        this._indicator = null;
        this.preview = this.shelf = this.hud = this.pins = null;
        this.lastCapture = null;
        this.settings = null;
    }

    // MARK: Actions

    run(action) {
        const done = result => result?.catch?.(e => logError(e, `Shotput: ${action}`));
        switch (action) {
        case 'area': done(this.capture('area')); break;
        case 'annotate': done(this.capture('area', {annotate: true})); break;
        case 'window': done(this.capture('window')); break;
        case 'screen': done(this.captureScreen()); break;
        case 'text': done(this.capture('area', {readText: true})); break;
        case 'repeat': done(this.repeatLastArea()); break;
        case 'last': this.annotateLast(); break;
        case 'clipboard': this.annotateClipboard(); break;
        case 'settings': this.openPreferences(); break;
        }
    }

    async _shootStage() {
        const shooter = new Shell.Screenshot();
        const [content, scale, cursorContent, cursorPoint, cursorScale] = await shooter.screenshot_stage_to_content();
        return {
            content, scale,
            cursor: cursorContent ? {texture: cursorContent.get_texture(), x: cursorPoint.x, y: cursorPoint.y, scale: cursorScale} : null,
        };
    }

    /** Crops the still to a rect in stage coordinates and encodes it. */
    async _crop(shot, rect, withCursor = false) {
        const s = shot.scale;
        const stream = Gio.MemoryOutputStream.new_resizable();
        const cursor = withCursor ? shot.cursor : null;
        const pixbuf = await Shell.Screenshot.composite_to_stream(
            shot.content.get_texture(),
            Math.round(rect.x * s), Math.round(rect.y * s), Math.round(rect.width * s), Math.round(rect.height * s),
            s,
            cursor?.texture ?? null, (cursor?.x ?? 0) * s, (cursor?.y ?? 0) * s, cursor?.scale ?? 1,
            stream);
        stream.close(null);
        return {pixbuf, png: stream.steal_as_bytes()};
    }

    _focusAppName() {
        return Shell.WindowTracker.get_default().focus_app?.get_name() ?? null;
    }

    _monitorIndexFor(rect) {
        const cx = rect.x + rect.width / 2, cy = rect.y + rect.height / 2;
        const m = Main.layoutManager.monitors.find(mon =>
            cx >= mon.x && cx < mon.x + mon.width && cy >= mon.y && cy < mon.y + mon.height);
        return (m ?? Main.layoutManager.primaryMonitor).index;
    }

    _consumeHint() {
        switch (this.settings.get_string('selection-hints')) {
        case 'always': return true;
        case 'never': return false;
        default: {
            const shown = this.settings.get_int('hints-shown');
            if (shown >= 4)
                return false;
            this.settings.set_int('hints-shown', shown + 1);
            return true;
        }
        }
    }

    /**
     * Runs a capture from trigger to delivery: still, selection, clipboard, then preview or editor.
     * Pressing a capture shortcut again while selecting backs out, so one key both starts and cancels.
     */
    async capture(mode, {annotate = false, readText = false} = {}) {
        if (this._overlay) {
            this._overlay.cancel();
            return;
        }
        if (this._starting)
            return;
        this._starting = true;
        const appName = this._focusAppName();
        let shot;
        try {
            this.preview.dismiss(false);
            shot = await this._shootStage();
        } catch (e) {
            logError(e, 'Shotput: capturing the screen');
            this._starting = false;
            return;
        }
        let outcome;
        try {
            const overlay = new SelectionOverlay({mode, shot, settings: this.settings, showHint: mode === 'area' && this._consumeHint()});
            this._overlay = overlay;
            this._starting = false;
            outcome = await overlay.run();
        } catch (e) {
            logError(e, 'Shotput: selecting');
            outcome = {kind: 'cancelled'};
        } finally {
            this._overlay = null;
            this._starting = false;
        }

        try {
            if (outcome.kind === 'area') {
                const rect = outcome.rect;
                this._lastArea = rect;
                const {pixbuf, png} = await this._crop(shot, rect);
                if (readText) {
                    this._copyText(pixbuf);
                    return;
                }
                this._deliver(new Capture({pixbuf, png, scale: shot.scale, screenRect: rect, monitorIndex: this._monitorIndexFor(rect), appName}), annotate);
            } else if (outcome.kind === 'window') {
                const capture = await this._captureWindow(outcome.info, shot, appName);
                if (!capture)
                    return;
                if (readText) {
                    this._copyText(capture.pixbuf);
                    return;
                }
                this._deliver(capture, annotate);
            }
        } catch (e) {
            logError(e, 'Shotput: finishing the capture');
            this.hud.show('Capture failed', 'dialog-warning-symbolic');
        }
    }

    /** One window on its own, with clean corners and nothing overlapping it. */
    async _captureWindow(info, shot, appName) {
        const window = info.window;
        const frame = window.get_frame_rect();
        const rect = {x: frame.x, y: frame.y, width: frame.width, height: frame.height};
        const keepShadow = this.settings.get_boolean('window-shadow');
        let result = null;
        let scale = shot.scale;
        try {
            const actor = window.get_compositor_private();
            const content = actor?.paint_to_content(null);
            if (content) {
                const buffer = window.get_buffer_rect();
                scale = actor.get_resource_scale();
                const geometry = keepShadow ? [-1, -1, -1, -1] : [
                    Math.round((frame.x - buffer.x) * scale), Math.round((frame.y - buffer.y) * scale),
                    Math.round(frame.width * scale), Math.round(frame.height * scale),
                ];
                const stream = Gio.MemoryOutputStream.new_resizable();
                const pixbuf = await Shell.Screenshot.composite_to_stream(
                    content.get_texture(), ...geometry, scale, null, 0, 0, 1, stream);
                stream.close(null);
                result = {pixbuf, png: stream.steal_as_bytes()};
            }
        } catch (e) {
            logError(e, 'Shotput: capturing the window on its own');
        }
        if (!result) {
            // Fall back to the still if the window has gone away.
            scale = shot.scale;
            result = await this._crop(shot, rect);
        }
        const capture = new Capture({
            ...result, scale, screenRect: rect, monitorIndex: this._monitorIndexFor(rect), appName: info.ownerName ?? appName,
        });

        const background = this.settings.get_string('window-background');
        if (background !== 'transparent') {
            const fill = background === 'wallpaper' ? 'wallpaper' : this.settings.get_string('window-background-color');
            const padding = this.settings.get_double('window-padding');
            const framed = await this._compose(capture, fill, padding, keepShadow);
            if (framed) {
                const plain = capture.path.replace(/\.png$/, '-plain.png');
                GLib.file_set_contents(plain, capture.png.toArray());
                const composed = new Capture({...framed, scale, screenRect: rect, monitorIndex: capture.monitorIndex, appName: capture.appName});
                composed.framed = {plain, fill, padding};
                return composed;
            }
        }
        return capture;
    }

    /** Frames a capture on its backdrop with the editor binary, which shares the editor's renderer. */
    async _compose(capture, fill, padding, keepShadow) {
        const bin = this._shotputBinary();
        if (!bin)
            return null;
        const dir = runtimeDir();
        const input = GLib.build_filenamev([dir, `${capture.id}-in.png`]);
        const output = GLib.build_filenamev([dir, `${capture.id}-out.png`]);
        try {
            GLib.file_set_contents(input, capture.png.toArray());
            const args = [bin, 'compose', input, output, '--scale', `${capture.scale}`, '--fill', fill, '--padding', `${padding}`];
            if (keepShadow)
                args.push('--no-shadow');
            const proc = Gio.Subprocess.new(args, Gio.SubprocessFlags.STDERR_SILENCE);
            await proc.wait_check_async(null);
            const pixbuf = GdkPixbuf.Pixbuf.new_from_file(output);
            const [, data] = GLib.file_get_contents(output);
            return {pixbuf, png: new GLib.Bytes(data)};
        } catch (e) {
            logError(e, 'Shotput: framing the window');
            return null;
        } finally {
            for (const f of [input, output])
                GLib.unlink(f);
        }
    }

    /** The display under the pointer. */
    async captureScreen() {
        if (this._overlay || this._starting)
            return;
        this._starting = true;
        try {
            const appName = this._focusAppName();
            this.preview.dismiss(false);
            const shot = await this._shootStage();
            const [px, py] = global.get_pointer();
            const monitor = Main.layoutManager.monitors.find(m =>
                px >= m.x && px < m.x + m.width && py >= m.y && py < m.y + m.height) ?? Main.layoutManager.primaryMonitor;
            const rect = {x: monitor.x, y: monitor.y, width: monitor.width, height: monitor.height};
            const {pixbuf, png} = await this._crop(shot, rect, this.settings.get_boolean('shows-pointer'));
            this._deliver(new Capture({pixbuf, png, scale: shot.scale, screenRect: rect, monitorIndex: monitor.index, appName}), false);
        } catch (e) {
            logError(e, 'Shotput: capturing the screen');
        } finally {
            this._starting = false;
        }
    }

    /** Captures the area chosen last time again, straight away. */
    async repeatLastArea() {
        if (this._overlay || this._starting)
            return;
        if (!this._lastArea) {
            this.capture('area');
            return;
        }
        this._starting = true;
        try {
            const appName = this._focusAppName();
            this.preview.dismiss(false);
            const shot = await this._shootStage();
            const rect = this._lastArea;
            const {pixbuf, png} = await this._crop(shot, rect);
            this._deliver(new Capture({pixbuf, png, scale: shot.scale, screenRect: rect, monitorIndex: this._monitorIndexFor(rect), appName}), false);
        } catch (e) {
            logError(e, 'Shotput: repeating the last area');
        } finally {
            this._starting = false;
        }
    }

    annotateLast() {
        if (!this.lastCapture) {
            this.annotateClipboard();
            return;
        }
        this.preview.dismiss();
        this.openEditor(this.lastCapture);
    }

    /** Opens whatever image is on the clipboard, so screenshots from other tools can be marked up too. */
    annotateClipboard() {
        const clipboard = St.Clipboard.get_default();
        {
            const types = clipboard.get_mimetypes(St.ClipboardType.CLIPBOARD) ?? [];
            const type = types.find(t => t === 'image/png') ?? types.find(t => t.startsWith('image/'));
            if (!type) {
                this.hud.show('No image on the clipboard', 'dialog-information-symbolic');
                return;
            }
            clipboard.get_content(St.ClipboardType.CLIPBOARD, type, (_c2, bytes) => {
                try {
                    if (!bytes || bytes.get_size() === 0)
                        throw new Error('empty clipboard');
                    const stream = Gio.MemoryInputStream.new_from_bytes(bytes);
                    const pixbuf = GdkPixbuf.Pixbuf.new_from_stream(stream, null);
                    // Images from elsewhere carry no reliable density, so treat them as this screen's.
                    const scale = St.ThemeContext.get_for_stage(global.stage).scale_factor;
                    const capture = new Capture({pixbuf, png: type === 'image/png' ? bytes : null, scale});
                    this.lastCapture = capture;
                    this.openEditor(capture);
                } catch (e) {
                    this.hud.show('No image on the clipboard', 'dialog-information-symbolic');
                }
            });
        }
    }

    // MARK: Delivery

    _deliver(capture, annotate) {
        this.lastCapture = capture;
        const after = annotate ? 'editor' : this.settings.get_string('after-capture');
        if (this.settings.get_boolean('plays-sound'))
            global.display.get_sound_player().play_from_theme('screen-capture', 'Screenshot taken', null);
        flash(capture.screenRect);
        this.pulseIndicator();

        // The clipboard is filled straight away in every mode, so pasting never waits on a choice.
        this.copyCapture(capture);
        capture.ensureFile();
        if (this.settings.get_boolean('saves-every-capture') && this.settings.get_string('save-folder'))
            this.saveCapture(capture, {quiet: true});

        if (after === 'editor')
            this.openEditor(capture);
        else if (after === 'preview')
            this.preview.show(capture);
    }

    setClipboardBytes(bytes) {
        St.Clipboard.get_default().set_content(St.ClipboardType.CLIPBOARD, 'image/png', bytes);
    }

    /** Applies the standard-resolution preference, then puts the PNG on the clipboard. */
    copyCapture(capture) {
        let bytes = capture.png;
        if (this.settings.get_boolean('copy-at-point-size') && capture.scale > 1) {
            const w = Math.round(capture.pixbuf.get_width() / capture.scale);
            const h = Math.round(capture.pixbuf.get_height() / capture.scale);
            bytes = pngBytes(capture.pixbuf.scale_simple(w, h, GdkPixbuf.InterpType.HYPER));
        }
        this.setClipboardBytes(bytes);
    }

    _saveFolder() {
        let folder = this.settings.get_string('save-folder').trim();
        if (folder.startsWith('~'))
            folder = GLib.get_home_dir() + folder.slice(1);
        if (folder && GLib.file_test(folder, GLib.FileTest.IS_DIR))
            return folder;
        const pictures = GLib.get_user_special_dir(GLib.UserDirectory.DIRECTORY_PICTURES) ?? GLib.get_home_dir();
        const screenshots = GLib.build_filenamev([pictures, 'Screenshots']);
        GLib.mkdir_with_parents(screenshots, 0o755);
        return screenshots;
    }

    /** Writes into the chosen folder, or Pictures/Screenshots when none is set. */
    saveCapture(capture, {quiet = false} = {}) {
        try {
            const format = this.settings.get_string('save-format') === 'jpeg' ? 'jpeg' : 'png';
            const template = this.settings.get_string('filename-template');
            const name = `${fileName(template, capture.date, capture.appName)}.${format === 'jpeg' ? 'jpg' : 'png'}`;
            const folder = this._saveFolder();
            const path = uniquePath(folder, name);
            let data;
            if (format === 'png') {
                data = capture.png.toArray();
            } else {
                // JPEG has no alpha. Flatten onto white so transparent window corners do not turn black.
                const src = capture.pixbuf;
                const flat = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, false, 8, src.get_width(), src.get_height());
                flat.fill(0xffffffff);
                src.composite(flat, 0, 0, src.get_width(), src.get_height(), 0, 0, 1, 1, GdkPixbuf.InterpType.NEAREST, 255);
                const quality = Math.round(Math.min(Math.max(this.settings.get_double('jpeg-quality'), 0.1), 1) * 100);
                [, data] = flat.save_to_bufferv('jpeg', ['quality'], [`${quality}`]);
            }
            GLib.file_set_contents(path, data);
            if (!quiet)
                this.hud.show(`Saved to ${GLib.path_get_basename(folder)}`);
        } catch (e) {
            logError(e, 'Shotput: saving');
            this.hud.show('Could not save the screenshot', 'dialog-warning-symbolic');
        }
    }

    // MARK: Text recognition

    _tesseract() {
        const bundled = GLib.build_filenamev([GLib.get_user_data_dir(), 'shotput', 'bin', 'tesseract']);
        if (GLib.file_test(bundled, GLib.FileTest.IS_EXECUTABLE))
            return bundled;
        return GLib.find_program_in_path('tesseract');
    }

    /** Reads the text in the image on this machine and copies it. */
    async _copyText(pixbuf) {
        const bin = this._tesseract();
        if (!bin) {
            this.hud.show('Text recognition needs Tesseract', 'dialog-warning-symbolic');
            return;
        }
        const path = GLib.build_filenamev([runtimeDir(), `ocr-${GLib.uuid_string_random()}.png`]);
        try {
            // Screen text is small for Tesseract; doubling it reads far more reliably.
            const w = pixbuf.get_width(), h = pixbuf.get_height();
            const input = w * h < 8e6 ? pixbuf.scale_simple(w * 2, h * 2, GdkPixbuf.InterpType.BILINEAR) : pixbuf;
            input.savev(path, 'png', [], []);
            const lang = this.settings.get_string('ocr-language') || 'eng';
            const proc = Gio.Subprocess.new([bin, path, 'stdout', '-l', lang],
                Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_SILENCE);
            const [out] = await proc.communicate_utf8_async(null, null);
            const lines = (out ?? '').split('\n').map(l => l.trim()).filter(Boolean);
            if (lines.length === 0) {
                this.hud.show('No text found', 'shotput-read-text-symbolic');
                return;
            }
            const joined = lines.join(this.settings.get_boolean('ocr-keeps-line-breaks') ? '\n' : ' ');
            St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, joined);
            this.hud.show(lines.length === 1 ? 'Copied 1 line of text' : `Copied ${lines.length} lines of text`);
        } catch (e) {
            logError(e, 'Shotput: reading text');
            this.hud.show('Could not read the text', 'dialog-warning-symbolic');
        } finally {
            GLib.unlink(path);
        }
    }

    // MARK: Editor

    _shotputBinary() {
        const local = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'shotput']);
        if (GLib.file_test(local, GLib.FileTest.IS_EXECUTABLE))
            return local;
        return GLib.find_program_in_path('shotput');
    }

    _warmEditor() {
        Gio.DBus.session.call(EDITOR_ID, EDITOR_PATH, 'org.freedesktop.Application', 'Activate',
            new GLib.Variant('(a{sv})', [{}]), null, Gio.DBusCallFlags.NONE, 10000, null, (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch {}
            });
    }

    async openEditor(capture) {
        await capture.ensureFile();
        const monitor = Main.layoutManager.monitors[capture.monitorIndex] ?? Main.layoutManager.primaryMonitor;
        const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
        const request = {
            path: capture.path, id: capture.id, scale: capture.scale,
            monitor: [work.width, work.height], app: capture.appName, writes_back: true,
            plain: capture.framed?.plain ?? null, fill: capture.framed?.fill ?? null, padding: capture.framed?.padding ?? null,
        };
        this._placement = {rect: capture.screenRect, work, until: GLib.get_monotonic_time() + 6 * 1e6};
        const json = JSON.stringify(request);
        Gio.DBus.session.call(EDITOR_ID, EDITOR_PATH, 'org.gtk.Actions', 'Activate',
            new GLib.Variant('(sava{sv})', ['open', [new GLib.Variant('s', json)], {}]),
            null, Gio.DBusCallFlags.NONE, 10000, null, (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch (e) {
                    // No D-Bus activation file: start the binary directly.
                    const bin = this._shotputBinary();
                    if (bin)
                        Gio.Subprocess.new([bin, '--open-json', json], Gio.SubprocessFlags.NONE);
                    else
                        this.hud.show('The Shotput editor is not installed', 'dialog-warning-symbolic');
                }
            });
    }

    openPinned(pin) {
        try {
            const path = this.pins.path(pin);
            const pixbuf = GdkPixbuf.Pixbuf.new_from_file(path);
            const capture = new Capture({pixbuf, png: this.pins.bytes(pin), scale: pin.scale ?? 1});
            this.openEditor(capture);
        } catch (e) {
            logError(e, 'Shotput: opening a pin');
        }
    }

    /**
     * The editor opens where the capture was taken, so the screenshot appears to lift into it,
     * and it is focused even though it was started over D-Bus.
     */
    _onWindowCreated(window) {
        // On Wayland the app id arrives after the window is created, so look at its first frame.
        const isEditor = () => window.get_gtk_application_id?.() === EDITOR_ID || window.get_wm_class() === EDITOR_ID;
        const place = () => {
            if (!isEditor())
                return;
            const placement = this._placement && GLib.get_monotonic_time() < this._placement.until ? this._placement : null;
            this._placement = null;
            if (placement) {
                const f = window.get_frame_rect();
                const work = placement.work;
                const header = 47, reserve = 68;
                const cx = placement.rect ? placement.rect.x + placement.rect.width / 2 : work.x + work.width / 2;
                const cy = placement.rect ? placement.rect.y + placement.rect.height / 2 : work.y + work.height / 2;
                let x = cx - f.width / 2;
                let y = cy - header - (f.height - header - reserve) / 2;
                x = Math.min(Math.max(x, work.x), work.x + work.width - f.width);
                y = Math.min(Math.max(y, work.y), work.y + work.height - f.height);
                window.move_frame(true, Math.round(x), Math.round(y));
            }
            Main.activateWindow(window);
        };
        const handler = window.connect('shown', () => {
            window.disconnect(handler);
            place();
        });
    }

    // MARK: Top bar

    _syncIndicator() {
        const shows = this.settings.get_boolean('shows-panel-icon');
        if (shows && !this._indicator) {
            this._indicator = new Indicator(this);
            Main.panel.addToStatusArea(this.uuid, this._indicator);
        } else if (!shows && this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }
    }

    pulseIndicator() {
        this._indicator?.pulse();
    }

    /** Where pins fly to: the top bar icon, or the top centre of the screen when it is hidden. */
    indicatorTarget() {
        if (this._indicator?.visible && this._indicator.get_stage()) {
            const [x, y] = this._indicator.get_transformed_position();
            const [w, h] = this._indicator.get_transformed_size();
            return {x, y, width: w, height: h};
        }
        const m = Main.layoutManager.primaryMonitor;
        return {x: m.x + m.width / 2 - 11, y: m.y, width: 22, height: 22};
    }

    // MARK: Housekeeping

    /** Before this extension loaded, the installer pointed a GNOME custom shortcut at `shotput`. */
    _migrateCustomKeybindings() {
        try {
            const media = new Gio.Settings({schema_id: 'org.gnome.settings-daemon.plugins.media-keys'});
            const paths = media.get_strv('custom-keybindings');
            const keep = paths.filter(path => {
                const binding = new Gio.Settings({
                    schema_id: 'org.gnome.settings-daemon.plugins.media-keys.custom-keybinding', path,
                });
                const ours = /(^|\/)shotput(\s|$)/.test(binding.get_string('command'));
                if (ours) {
                    binding.reset('name');
                    binding.reset('command');
                    binding.reset('binding');
                }
                return !ours;
            });
            if (keep.length !== paths.length)
                media.set_strv('custom-keybindings', keep);
        } catch (e) {
            logError(e, 'Shotput: migrating shortcuts');
        }
    }

    /** Captures are kept for a day, for the editor and "annotate last". */
    _purgeOldCaptures() {
        try {
            const dir = Gio.File.new_for_path(cacheDir());
            const children = dir.enumerate_children('standard::name,time::modified', Gio.FileQueryInfoFlags.NONE, null);
            const cutoff = GLib.get_real_time() / 1e6 - 86400;
            let info;
            while ((info = children.next_file(null))) {
                if (info.get_attribute_uint64('time::modified') < cutoff)
                    dir.get_child(info.get_name()).delete(null);
            }
        } catch {}
    }
}

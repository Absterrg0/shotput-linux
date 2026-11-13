// Captures kept in the top bar: the 15 most recent pins, newest first, saved in Shotput's own
// data folder so they survive a restart. Left click on the icon opens them in a strip that
// scrolls sideways; right click opens the capture menu. Ported from PinStore.swift,
// PinTray.swift, PinCell.swift and the menu bar item in AppDelegate.swift.

import Clutter from 'gi://Clutter';
import GdkPixbuf from 'gi://GdkPixbuf';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {showScreenRecordingUI} from 'resource:///org/gnome/shell/ui/screenshot.js';

import {RoundedMask} from './effects.js';
import {coverContent} from './preview.js';

const LIMIT = 15;
const CELL = {width: 136, height: 86};

export class PinStore {
    constructor() {
        this.limit = LIMIT;
        this._dir = GLib.build_filenamev([GLib.get_user_data_dir(), 'shotput', 'pins']);
        GLib.mkdir_with_parents(this._dir, 0o700);
        this._listeners = new Set();
        this.pins = [];
        try {
            const [, data] = GLib.file_get_contents(this._index);
            const saved = JSON.parse(new TextDecoder().decode(data));
            this.pins = saved.filter(p => GLib.file_test(this.path(p), GLib.FileTest.EXISTS));
        } catch {
            this.pins = [];
        }
    }

    get _index() {
        return GLib.build_filenamev([this._dir, 'index.json']);
    }

    get count() {
        return this.pins.length;
    }

    path(pin) {
        return GLib.build_filenamev([this._dir, `${pin.id}.png`]);
    }

    contains(capture) {
        return this.pins.some(p => p.id === capture.id);
    }

    onChange(listener) {
        this._listeners.add(listener);
        return () => this._listeners.delete(listener);
    }

    /** Adds the capture at the front. Past the limit, the oldest pin goes. */
    pin(capture) {
        if (this.contains(capture))
            return false;
        const pin = {id: capture.id, date: capture.date.to_unix(), appName: capture.appName, scale: capture.scale};
        try {
            GLib.file_set_contents(this.path(pin), capture.png.toArray());
        } catch {
            return false;
        }
        this.pins.unshift(pin);
        while (this.pins.length > LIMIT)
            this.remove(this.pins[this.pins.length - 1], false);
        this._save();
        return true;
    }

    remove(pin, notify = true) {
        try {
            Gio.File.new_for_path(this.path(pin)).delete(null);
        } catch {}
        this.pins = this.pins.filter(p => p.id !== pin.id);
        if (notify)
            this._save();
    }

    removeAll() {
        for (const pin of [...this.pins])
            this.remove(pin, false);
        this._save();
    }

    bytes(pin) {
        const [, data] = GLib.file_get_contents(this.path(pin));
        return new GLib.Bytes(data);
    }

    _save() {
        try {
            GLib.file_set_contents(this._index, JSON.stringify(this.pins));
        } catch (e) {
            logError(e, 'Shotput: saving pins');
        }
        for (const listener of this._listeners)
            listener();
    }
}

function relativeTime(unix) {
    const seconds = Math.max(0, GLib.get_real_time() / 1e6 - unix);
    if (seconds < 60)
        return 'just now';
    if (seconds < 3600)
        return `${Math.floor(seconds / 60)} min. ago`;
    if (seconds < 86400)
        return `${Math.floor(seconds / 3600)} hr. ago`;
    return `${Math.floor(seconds / 86400)} days ago`;
}

/** '<Control><Alt>a' as 'Ctrl+Alt+A', without GTK, which cannot load in the shell. */
export function acceleratorLabel(accel) {
    if (!accel)
        return '';
    const names = {control: 'Ctrl', primary: 'Ctrl', ctrl: 'Ctrl', alt: 'Alt', shift: 'Shift', super: 'Super', meta: 'Meta', hyper: 'Hyper'};
    const mods = [...accel.matchAll(/<([^>]+)>/g)].map(m => names[m[1].toLowerCase()] ?? m[1]);
    let key = accel.replace(/<[^>]+>/g, '');
    key = key.length === 1 ? key.toUpperCase() : key.replace(/^KP_/, 'Keypad ');
    return [...mods, key].join('+');
}

function shortcutLabel(settings, key) {
    return acceleratorLabel(settings.get_strv(key)[0] ?? '');
}

export const Indicator = GObject.registerClass(
class ShotputIndicator extends PanelMenu.Button {
    _init(shotput) {
        super._init(0.5, 'Shotput', false);
        this._shotput = shotput;
        this.add_child(new St.Icon({
            gicon: Gio.icon_new_for_string(GLib.build_filenamev([shotput.path, 'icons', 'shotput-capture-symbolic.svg'])),
            style_class: 'system-status-icon',
        }));

        // One menu: the pinned strip, then captures and settings. (GNOME's top bar keeps
        // right clicks from its items, so the Mac's separate right-click menu lives here too.)
        this._trayItem = new PopupMenu.PopupBaseMenuItem({reactive: false, can_focus: false, style_class: 'shotput-tray-item'});
        this.menu.addMenuItem(this._trayItem);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        this._actionsSection = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._actionsSection);
        this.menu.connect('open-state-changed', (_m, open) => {
            if (open) {
                this._buildTray();
                this._buildActions();
            }
        });
        this._unwatch = shotput.pins.onChange(() => {
            if (this.menu.isOpen)
                this._buildTray();
        });
    }

    pulse() {
        if (!St.Settings.get().enable_animations)
            return;
        this.opacity = 64;
        this.ease({opacity: 255, duration: 350, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
    }

    _buildActions() {
        const menu = this._actionsSection;
        menu.removeAll();
        const s = this._shotput;
        const add = (label, key, action, icon) => {
            const item = new PopupMenu.PopupImageMenuItem(label, icon);
            const accel = shortcutLabel(s.settings, key);
            if (accel)
                item.add_child(new St.Label({text: accel, style_class: 'shotput-accel', x_expand: true, x_align: Clutter.ActorAlign.END, y_align: Clutter.ActorAlign.CENTER}));
            item.connect('activate', () => {
                // Let the menu leave first, so it is not in the frozen still.
                GLib.timeout_add(GLib.PRIORITY_DEFAULT, 160, () => {
                    s.run(action);
                    return GLib.SOURCE_REMOVE;
                });
            });
            menu.addMenuItem(item);
            return item;
        };
        const icon = name => Gio.icon_new_for_string(GLib.build_filenamev([s.path, 'icons', `${name}.svg`]));
        add('Capture Area', 'shortcut-area', 'area', icon('shotput-capture-symbolic'));
        add('Capture Area and Annotate', 'shortcut-area-annotate', 'annotate', icon('shotput-annotate-symbolic'));
        add('Capture Window', 'shortcut-window', 'window', 'focus-windows-symbolic');
        add('Capture Screen', 'shortcut-screen', 'screen', 'video-display-symbolic');
        add('Repeat Last Area', 'shortcut-repeat-area', 'repeat', 'view-refresh-symbolic');
        add('Copy Text from Area', 'shortcut-text', 'text', icon('shotput-read-text-symbolic'));
        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        add('Annotate Last Capture', 'shortcut-annotate-last', 'last', 'document-open-recent-symbolic')
            .setSensitive(Boolean(s.lastCapture));
        add('Annotate Clipboard Image', 'shortcut-annotate-clipboard', 'clipboard', 'edit-paste-symbolic');
        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        // Shotput takes stills; GNOME's own tool records the screen.
        const record = new PopupMenu.PopupImageMenuItem('Record Screen…', 'media-record-symbolic');
        record.connect('activate', () => {
            GLib.timeout_add(GLib.PRIORITY_DEFAULT, 160, () => {
                showScreenRecordingUI();
                return GLib.SOURCE_REMOVE;
            });
        });
        menu.addMenuItem(record);
        const settings = new PopupMenu.PopupImageMenuItem('Settings…', 'emblem-system-symbolic');
        settings.connect('activate', () => s.run('settings'));
        menu.addMenuItem(settings);
    }

    _buildTray() {
        const s = this._shotput;
        const store = s.pins;
        this._trayItem.destroy_all_children();
        const box = new St.BoxLayout({vertical: true, style_class: 'shotput-tray', x_expand: true});

        const header = new St.BoxLayout({style_class: 'shotput-tray-header'});
        header.add_child(new St.Label({text: 'Pinned', style_class: 'shotput-tray-title', y_align: Clutter.ActorAlign.CENTER}));
        if (store.count > 0) {
            header.add_child(new St.Label({text: `${store.count} of ${store.limit}`, style_class: 'shotput-tray-count', y_align: Clutter.ActorAlign.CENTER}));
            header.add_child(new St.Widget({x_expand: true}));
            const clear = new St.Button({label: 'Clear', style_class: 'shotput-link-button', can_focus: true});
            clear.connect('clicked', () => store.removeAll());
            header.add_child(clear);
        }
        box.add_child(header);

        if (store.count === 0) {
            const empty = new St.BoxLayout({style_class: 'shotput-tray-empty', x_expand: true});
            const text = new St.BoxLayout({vertical: true, x_expand: true, y_align: Clutter.ActorAlign.CENTER});
            text.add_child(new St.Label({text: 'Nothing pinned yet', style_class: 'shotput-tray-empty-title'}));
            text.add_child(new St.Label({text: 'Drag a preview up to the top bar, or use its pin.', style_class: 'shotput-tray-empty-detail'}));
            empty.add_child(text);
            const capture = new St.Button({label: 'Capture Area', style_class: 'button shotput-tray-capture', y_align: Clutter.ActorAlign.CENTER});
            capture.connect('clicked', () => {
                this.menu.close();
                GLib.timeout_add(GLib.PRIORITY_DEFAULT, 150, () => {
                    s.run('area');
                    return GLib.SOURCE_REMOVE;
                });
            });
            empty.add_child(capture);
            box.add_child(empty);
        } else {
            const strip = new St.BoxLayout({style_class: 'shotput-tray-strip'});
            for (const pin of store.pins)
                strip.add_child(this._cell(pin));
            const scroll = new St.ScrollView({
                style_class: 'shotput-tray-scroll', hscrollbar_policy: St.PolicyType.EXTERNAL,
                vscrollbar_policy: St.PolicyType.NEVER, overlay_scrollbars: true, enable_mouse_scrolling: true,
                x_expand: true, child: strip,
            });
            // A plain mouse wheel scrolls the strip sideways too.
            scroll.connect('scroll-event', (_a, event) => {
                const adj = scroll.hadjustment;
                const dir = event.get_scroll_direction();
                let delta = 0;
                if (dir === Clutter.ScrollDirection.UP || dir === Clutter.ScrollDirection.LEFT)
                    delta = -60;
                else if (dir === Clutter.ScrollDirection.DOWN || dir === Clutter.ScrollDirection.RIGHT)
                    delta = 60;
                else if (dir === Clutter.ScrollDirection.SMOOTH) {
                    const [dx, dy] = event.get_scroll_delta();
                    delta = (Math.abs(dy) > Math.abs(dx) ? dy : dx) * 30;
                }
                adj.value = Math.min(Math.max(adj.value + delta, adj.lower), adj.upper - adj.page_size);
                return Clutter.EVENT_STOP;
            });
            box.add_child(scroll);
            box.add_child(new St.Label({
                text: 'Click to copy. Alt-click or middle-click to annotate.',
                style_class: 'shotput-tray-footer',
            }));
        }
        this._trayItem.add_child(box);
    }

    /** One pin: click copies, Alt-click annotates, the corner button unpins. */
    _cell(pin) {
        const s = this._shotput;
        const cell = new St.Button({
            style_class: 'shotput-pin-cell', width: CELL.width, height: CELL.height, can_focus: true,
            track_hover: true, button_mask: St.ButtonMask.ONE | St.ButtonMask.TWO,
            accessible_name: `Pinned screenshot, ${relativeTime(pin.date)}`,
        });
        const layout = new St.Widget({layout_manager: new Clutter.BinLayout(), width: CELL.width, height: CELL.height});
        const image = new St.Widget({x_expand: true, y_expand: true});
        try {
            const pixbuf = GdkPixbuf.Pixbuf.new_from_file(s.pins.path(pin));
            image.set_content(coverContent(pixbuf, CELL.width, CELL.height));
        } catch {}
        image.add_effect(new RoundedMask(10));
        layout.add_child(image);
        layout.add_child(new St.Widget({style_class: 'shotput-pin-ring', x_expand: true, y_expand: true}));
        const meta = new St.Label({
            text: [pin.appName, relativeTime(pin.date)].filter(Boolean).join(', '),
            style_class: 'shotput-pin-meta', opacity: 0,
        });
        // Kept to its own size in the bottom-left corner.
        const metaLayer = new St.BoxLayout({vertical: true, x_expand: true, y_expand: true});
        metaLayer.add_child(new St.Widget({y_expand: true}));
        const metaRow = new St.BoxLayout();
        metaRow.add_child(meta);
        metaLayer.add_child(metaRow);
        layout.add_child(metaLayer);
        const remove = new St.Button({
            style_class: 'shotput-glass-button shotput-glass-small', can_focus: false, opacity: 0,
            child: new St.Icon({icon_name: 'window-close-symbolic', icon_size: 12}), accessible_name: 'Unpin',
        });
        remove.connect('clicked', () => s.pins.remove(pin));
        const corner = new St.Widget({x_expand: true, y_expand: true, layout_manager: new Clutter.FixedLayout()});
        corner.add_child(remove);
        remove.set_position(CELL.width - 26, 4);
        layout.add_child(corner);
        cell.set_child(layout);
        cell.connect('notify::hover', () => {
            remove.opacity = cell.hover ? 255 : 0;
            meta.opacity = cell.hover ? 255 : 0;
        });
        cell.connect('clicked', (_b, button) => {
            const event = Clutter.get_current_event();
            const alt = event && (event.get_state() & (Clutter.ModifierType.MOD1_MASK | Clutter.ModifierType.CONTROL_MASK));
            this.menu.close();
            if (alt || button === 2) {
                s.openPinned(pin);
            } else {
                s.setClipboardBytes(s.pins.bytes(pin));
                s.hud.show('Copied pinned screenshot');
            }
        });
        return cell;
    }

    destroy() {
        this._unwatch?.();
        super.destroy();
    }
});

// The floating thumbnail shown after a capture, the shelf a drag pins to, the capture flash
// and the HUD. Ported from Shotput's PreviewPanel.swift, PinShelf.swift and the HUD.
// Everything here is shell chrome, so it never takes focus from the app you paste into.

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import GdkPixbuf from 'gi://GdkPixbuf';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {RoundedMask} from './effects.js';

const MARGIN = 20;
const CHIP_RADIUS = 14;

/** An St.ImageContent of the pixbuf, cropped to fill `w` × `h` (cover), at the stage's scale. */
export function coverContent(pixbuf, w, h) {
    const scale = St.ThemeContext.get_for_stage(global.stage).scale_factor;
    const tw = Math.max(1, Math.round(w * scale)), th = Math.max(1, Math.round(h * scale));
    const pw = pixbuf.get_width(), ph = pixbuf.get_height();
    const fill = Math.max(tw / pw, th / ph);
    const cw = Math.min(pw, Math.round(tw / fill)), ch = Math.min(ph, Math.round(th / fill));
    const sub = pixbuf.new_subpixbuf(Math.floor((pw - cw) / 2), Math.floor((ph - ch) / 2), cw, ch);
    const scaled = fill < 1 ? sub.scale_simple(tw, th, GdkPixbuf.InterpType.BILINEAR) : sub.copy();
    return imageContent(scaled);
}

export function imageContent(pixbuf) {
    const coglContext = global.stage.context.get_backend().get_cogl_context();
    const content = St.ImageContent.new_with_preferred_size(pixbuf.get_width(), pixbuf.get_height());
    content.set_bytes(
        coglContext, pixbuf.read_pixel_bytes(),
        pixbuf.get_has_alpha() ? Cogl.PixelFormat.RGBA_8888 : Cogl.PixelFormat.RGB_888,
        pixbuf.get_width(), pixbuf.get_height(), pixbuf.get_rowstride());
    return content;
}

function iconFor(dir, name) {
    const file = Gio.File.new_for_path(GLib.build_filenamev([dir, 'icons', `${name}.svg`]));
    return file.query_exists(null) ? new Gio.FileIcon({file}) : new Gio.ThemedIcon({name});
}

function reducedMotion() {
    return !St.Settings.get().enable_animations;
}

function monitorAtPointer() {
    const [x, y] = global.get_pointer();
    return Main.layoutManager.monitors.find(m =>
        x >= m.x && x < m.x + m.width && y >= m.y && y < m.y + m.height) ?? Main.layoutManager.primaryMonitor;
}

// MARK: Preview

export class PreviewController {
    constructor(shotput) {
        this._shotput = shotput;
        this._actor = null;
        this._timer = 0;
    }

    get settings() {
        return this._shotput.settings;
    }

    destroy() {
        this.dismiss(false);
    }

    show(capture) {
        this.dismiss(false);
        const point = capture.pointSize;
        const fit = Math.min(232 / Math.max(point.width, 1), 148 / Math.max(point.height, 1), 1);
        const w = Math.round(Math.max(point.width * fit, 140));
        const h = Math.round(Math.max(point.height * fit, 84));
        const frame = this._frame(w, h, capture);

        const actor = new St.Widget({
            style_class: 'shotput-preview', reactive: true, track_hover: true,
            width: w, height: h, layout_manager: new Clutter.BinLayout(),
        });
        const image = new St.Widget({x_expand: true, y_expand: true, style_class: 'shotput-preview-image'});
        // Small captures sit centred at their real size. Larger ones fill the thumbnail.
        if (point.width <= w && point.height <= h) {
            image.set_content(imageContent(capture.pixbuf));
            image.set_content_gravity(Clutter.ContentGravity.CENTER);
        } else {
            image.set_content(coverContent(capture.pixbuf, w, h));
        }
        image.add_effect(new RoundedMask(CHIP_RADIUS));
        actor.add_child(image);

        const scrim = new St.Widget({style_class: 'shotput-preview-scrim', x_expand: true, y_expand: true, opacity: 0});
        actor.add_child(scrim);

        const buttons = new St.BoxLayout({
            style_class: 'shotput-preview-buttons', opacity: 0,
            x_align: Clutter.ActorAlign.CENTER, y_align: Clutter.ActorAlign.CENTER,
        });
        const dir = this._shotput.path;
        const button = (icon, label, onClick) => {
            const b = new St.Button({
                style_class: 'shotput-glass-button', can_focus: false,
                child: new St.Icon({gicon: iconFor(dir, icon), icon_size: 16}),
                accessible_name: label,
            });
            b.connect('clicked', () => onClick(b));
            return b;
        };
        buttons.add_child(button('shotput-annotate-symbolic', 'Annotate', () => {
            this.dismiss();
            this._shotput.openEditor(capture);
        }));
        buttons.add_child(button('edit-copy-symbolic', 'Copy again', b => {
            b.child.gicon = new Gio.ThemedIcon({name: 'object-select-symbolic'});
            this._shotput.copyCapture(capture);
            GLib.timeout_add(GLib.PRIORITY_DEFAULT, 600, () => {
                this.dismiss();
                return GLib.SOURCE_REMOVE;
            });
        }));
        buttons.add_child(button('document-save-symbolic', 'Save', () => {
            this.dismiss();
            this._shotput.saveCapture(capture);
        }));
        buttons.add_child(button('shotput-pin-symbolic', 'Pin to the top bar', () => {
            if (this._shotput.pins.pin(capture))
                this._flyToIndicator();
        }));
        actor.add_child(buttons);

        // Dismiss sits apart in the corner, as on the system's own screenshot thumbnail.
        const close = button('window-close-symbolic', 'Dismiss', () => this.dismiss());
        close.add_style_class_name('shotput-glass-small');
        close.child.icon_size = 12;
        close.opacity = 0;
        // A fixed layer, so the button keeps its own small size in the corner.
        const corner = new St.Widget({x_expand: true, y_expand: true, layout_manager: new Clutter.FixedLayout()});
        corner.add_child(close);
        close.set_position(w - 26, 4);
        actor.add_child(corner);

        actor.connect('notify::hover', () => {
            const hovered = actor.hover || this._dragging;
            for (const a of [scrim, buttons, close])
                a.ease({opacity: hovered ? 255 : 0, duration: 180, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
            if (hovered)
                this._cancelTimer();
            else
                this._scheduleDismissal();
        });
        this._connectDrag(actor, capture);

        Main.layoutManager.addTopChrome(actor);
        this._actor = actor;
        this._frameRect = frame;

        // The thumbnail leaves from where the capture was taken and settles in its corner, so it is
        // clear where the screenshot went. Without a known origin it slides in from the edge.
        const monitor = Main.layoutManager.monitors[capture.monitorIndex] ?? monitorAtPointer();
        const fromLeft = frame.x + w / 2 < monitor.x + monitor.width / 2;
        let start = {x: frame.x + (fromLeft ? -16 : 16), y: frame.y};
        const origin = capture.screenRect;
        if (origin && !intersects(origin, {x: frame.x, y: frame.y, width: w, height: h}))
            start = {x: origin.x + origin.width / 2 - w / 2, y: origin.y + origin.height / 2 - h / 2};
        const far = Math.hypot(start.x - frame.x, start.y - frame.y) > 120;
        if (reducedMotion())
            start = frame;
        actor.set_position(Math.round(start.x), Math.round(start.y));
        actor.opacity = 0;
        actor.ease({
            x: frame.x, y: frame.y, opacity: 255,
            duration: far ? 420 : 250,
            mode: Clutter.AnimationMode.EASE_OUT_EXPO,
        });
        this._scheduleDismissal();
    }

    /** Picks the corner nearest the pointer that does not cover what was just captured. */
    _frame(w, h, capture) {
        const monitor = Main.layoutManager.monitors[capture.monitorIndex] ?? monitorAtPointer();
        const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
        const area = {x: work.x + MARGIN, y: work.y + MARGIN, width: work.width - MARGIN * 2, height: work.height - MARGIN * 2};
        const corners = {
            'bottom-left': {x: area.x, y: area.y + area.height - h},
            'bottom-right': {x: area.x + area.width - w, y: area.y + area.height - h},
            'top-left': {x: area.x, y: area.y},
            'top-right': {x: area.x + area.width - w, y: area.y},
        };
        const preference = this.settings.get_string('preview-corner');
        if (corners[preference])
            return corners[preference];
        const [px, py] = global.get_pointer();
        const byDistance = Object.values(corners).sort((a, b) =>
            Math.hypot(a.x + w / 2 - px, a.y + h / 2 - py) - Math.hypot(b.x + w / 2 - px, b.y + h / 2 - py));
        const captured = capture.screenRect;
        return byDistance.find(c => !captured ||
            !intersects({x: c.x - 12, y: c.y - 12, width: w + 24, height: h + 24}, captured)) ?? byDistance[0];
    }

    _scheduleDismissal() {
        this._cancelTimer();
        const seconds = this.settings.get_double('preview-duration');
        if (seconds <= 0)
            return;
        this._timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, seconds * 1000, () => {
            this._timer = 0;
            this.dismiss();
            return GLib.SOURCE_REMOVE;
        });
    }

    _cancelTimer() {
        if (this._timer) {
            GLib.source_remove(this._timer);
            this._timer = 0;
        }
    }

    dismiss(animated = true) {
        this._cancelTimer();
        this._endDrag();
        const actor = this._actor;
        if (!actor)
            return;
        this._actor = null;
        if (!animated || reducedMotion()) {
            actor.destroy();
            return;
        }
        actor.reactive = false;
        actor.ease({opacity: 0, duration: 180, mode: Clutter.AnimationMode.EASE_OUT_QUAD, onComplete: () => actor.destroy()});
    }

    /** Shrinks the thumbnail into the top bar icon, so a pin visibly goes where it is kept. */
    _flyToIndicator() {
        this._cancelTimer();
        const actor = this._actor;
        if (!actor)
            return;
        this._actor = null;
        const target = this._shotput.indicatorTarget();
        actor.reactive = false;
        actor.set_pivot_point(0.5, 0.5);
        if (reducedMotion()) {
            actor.destroy();
            this._shotput.pulseIndicator();
            return;
        }
        actor.ease({
            x: target.x + target.width / 2 - actor.width / 2,
            y: target.y + target.height / 2 - actor.height / 2,
            scale_x: 18 / actor.width, scale_y: 12 / actor.height, opacity: 90,
            duration: 420, mode: Clutter.AnimationMode.EASE_IN_CUBIC,
            onComplete: () => {
                actor.destroy();
                this._shotput.pulseIndicator();
            },
        });
    }

    // MARK: Click to annotate, drag to pin

    _connectDrag(actor, capture) {
        actor.connect('button-press-event', (_a, event) => {
            if (event.get_button() !== Clutter.BUTTON_PRIMARY)
                return Clutter.EVENT_PROPAGATE;
            // Presses on the preview's own buttons belong to them.
            for (let target = global.stage.get_event_actor(event); target && target !== actor; target = target.get_parent()) {
                if (target instanceof St.Button)
                    return Clutter.EVENT_PROPAGATE;
            }
            const [x, y] = event.get_coords();
            this._press = {x, y, ax: actor.x, ay: actor.y};
            this._grab = global.stage.grab(actor);
            return Clutter.EVENT_STOP;
        });
        actor.connect('motion-event', (_a, event) => {
            if (!this._press)
                return Clutter.EVENT_PROPAGATE;
            const [x, y] = event.get_coords();
            if (!this._dragging && Math.hypot(x - this._press.x, y - this._press.y) > 4) {
                this._dragging = true;
                this._cancelTimer();
                this._shotput.shelf.show(capture);
            }
            if (this._dragging) {
                actor.set_position(this._press.ax + x - this._press.x, this._press.ay + y - this._press.y);
                this._shotput.shelf.track(x, y);
            }
            return Clutter.EVENT_STOP;
        });
        actor.connect('button-release-event', (_a, event) => {
            if (!this._press || event.get_button() !== Clutter.BUTTON_PRIMARY)
                return Clutter.EVENT_PROPAGATE;
            const dragged = this._dragging;
            const [x, y] = event.get_coords();
            this._endDrag();
            if (!dragged) {
                this.dismiss();
                this._shotput.openEditor(capture);
                return Clutter.EVENT_STOP;
            }
            if (this._shotput.shelf.dropAt(x, y)) {
                this._flyToIndicator();
            } else if (this._actor) {
                const frame = this._frameRect;
                actor.ease({x: frame.x, y: frame.y, duration: 250, mode: Clutter.AnimationMode.EASE_OUT_BACK});
                this._scheduleDismissal();
            }
            return Clutter.EVENT_STOP;
        });
    }

    _endDrag() {
        this._press = null;
        this._dragging = false;
        if (this._grab) {
            this._grab.dismiss();
            this._grab = null;
        }
        this._shotput.shelf?.hide();
    }
}

function intersects(a, b) {
    return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

// MARK: Shelf

/** The shelf that drops out of the top bar while a preview is dragged. Let go over it to pin. */
export class Shelf {
    constructor(shotput) {
        this._shotput = shotput;
        this._actor = null;
    }

    destroy() {
        this.hide(0, false);
    }

    show(capture) {
        if (this._actor)
            return;
        this._capture = capture;
        const pins = this._shotput.pins;
        this._phase = pins.contains(capture) ? 'already' : 'idle';

        const actor = new St.BoxLayout({
            style_class: 'shotput-shelf', vertical: true,
            width: 320, height: 132, x_align: Clutter.ActorAlign.CENTER,
        });
        const inner = new St.BoxLayout({
            style_class: 'shotput-shelf-inner', vertical: true, x_expand: true, y_expand: true,
            y_align: Clutter.ActorAlign.FILL,
        });
        this._icon = new St.Icon({
            gicon: iconFor(this._shotput.path, 'shotput-pin-symbolic'), icon_size: 20,
            style_class: 'shotput-shelf-icon', x_align: Clutter.ActorAlign.CENTER,
        });
        this._title = new St.Label({style_class: 'shotput-shelf-title', x_align: Clutter.ActorAlign.CENTER});
        this._subtitle = new St.Label({style_class: 'shotput-shelf-subtitle', x_align: Clutter.ActorAlign.CENTER});
        const column = new St.BoxLayout({vertical: true, y_expand: true, y_align: Clutter.ActorAlign.CENTER});
        column.add_child(this._icon);
        column.add_child(this._title);
        column.add_child(this._subtitle);
        inner.add_child(column);
        actor.add_child(inner);

        const anchor = this._shotput.indicatorTarget();
        const monitor = Main.layoutManager.monitors.find(m => anchor.x >= m.x && anchor.x < m.x + m.width) ??
            Main.layoutManager.primaryMonitor;
        const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
        const x = Math.min(Math.max(anchor.x + anchor.width / 2 - 160, work.x + 8), work.x + work.width - 328);
        const y = work.y + 8;
        this._rect = {x, y, width: 320, height: 132};
        Main.layoutManager.addTopChrome(actor);
        this._actor = actor;
        this._update();

        // Drops down out of the top bar, the way a menu would.
        actor.set_position(Math.round(x), Math.round(y - 14));
        actor.opacity = 0;
        actor.ease({y: Math.round(y), opacity: 255, duration: 220, mode: Clutter.AnimationMode.EASE_OUT_EXPO});
    }

    _update() {
        if (!this._actor)
            return;
        const count = this._shotput.pins.count;
        const limit = this._shotput.pins.limit;
        const titles = {idle: 'Drop to pin', targeted: 'Let go to pin', pinned: 'Pinned', already: 'Already pinned'};
        this._title.text = titles[this._phase];
        this._subtitle.text = this._phase === 'pinned' ? `In the top bar, ${count} of ${limit}`
            : this._phase === 'already' ? 'It\'s in the top bar'
                : count >= limit ? `The oldest of ${limit} pins makes room` : `Keeps up to ${limit} in the top bar`;
        if (this._phase === 'targeted')
            this._actor.add_style_pseudo_class('targeted');
        else
            this._actor.remove_style_pseudo_class('targeted');
        if (this._phase === 'pinned')
            this._icon.gicon = new Gio.ThemedIcon({name: 'object-select-symbolic'});
    }

    _contains(x, y) {
        const r = this._rect;
        return r && x >= r.x && x <= r.x + r.width && y >= r.y && y <= r.y + r.height;
    }

    track(x, y) {
        if (!this._actor || this._phase === 'already' || this._phase === 'pinned')
            return;
        const phase = this._contains(x, y) ? 'targeted' : 'idle';
        if (phase !== this._phase) {
            this._phase = phase;
            this._update();
        }
    }

    /** Pins when the point is over the shelf. */
    dropAt(x, y) {
        if (!this._actor || !this._contains(x, y) || !this._capture)
            return false;
        if (!this._shotput.pins.pin(this._capture)) {
            this._phase = 'already';
            this._update();
            return false;
        }
        this._phase = 'pinned';
        this._update();
        this.hide(550);
        return true;
    }

    hide(delay = 0, animated = true) {
        const actor = this._actor;
        if (!actor)
            return;
        if (this._phase === 'pinned' && delay === 0)
            return; // Already leaving after its confirmation.
        this._actor = null;
        this._capture = null;
        const go = () => {
            if (!animated) {
                actor.destroy();
                return GLib.SOURCE_REMOVE;
            }
            actor.ease({
                opacity: 0, y: actor.y - 8, duration: 160, mode: Clutter.AnimationMode.EASE_IN_QUAD,
                onComplete: () => actor.destroy(),
            });
            return GLib.SOURCE_REMOVE;
        };
        if (delay > 0)
            GLib.timeout_add(GLib.PRIORITY_DEFAULT, delay, go);
        else
            go();
    }
}

// MARK: Flash and HUD

/** A brief shutter over what was just captured, so the eye knows it landed. */
export function flash(rect) {
    if (reducedMotion() || !rect || rect.width <= 8 || rect.height <= 8)
        return;
    const actor = new St.Widget({
        style_class: 'shotput-flash', x: rect.x, y: rect.y, width: rect.width, height: rect.height,
        opacity: Math.round(255 * 0.32),
    });
    Main.layoutManager.addTopChrome(actor);
    actor.ease({opacity: 0, duration: 220, mode: Clutter.AnimationMode.EASE_OUT_QUAD, onComplete: () => actor.destroy()});
}

/** A short message near the bottom of the screen, for results that have no window to show them in. */
export class Hud {
    constructor(path) {
        this._path = path;
        this._actor = null;
        this._timer = 0;
    }

    show(message, icon = 'object-select-symbolic') {
        this.destroy();
        const actor = new St.BoxLayout({style_class: 'shotput-hud'});
        actor.add_child(new St.Icon({gicon: iconFor(this._path, icon), icon_size: 16, y_align: Clutter.ActorAlign.CENTER}));
        actor.add_child(new St.Label({text: message, y_align: Clutter.ActorAlign.CENTER}));
        Main.layoutManager.addTopChrome(actor);
        this._actor = actor;
        const monitor = monitorAtPointer();
        const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
        const place = () => actor.set_position(
            Math.round(work.x + (work.width - actor.width) / 2), Math.round(work.y + work.height - actor.height - 64));
        actor.connect('notify::width', place);
        place();
        actor.opacity = 0;
        actor.ease({opacity: 255, duration: 180, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
        this._timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 1800, () => {
            this._timer = 0;
            const a = this._actor;
            this._actor = null;
            a?.ease({opacity: 0, duration: 250, mode: Clutter.AnimationMode.EASE_IN_QUAD, onComplete: () => a.destroy()});
            return GLib.SOURCE_REMOVE;
        });
    }

    destroy() {
        if (this._timer) {
            GLib.source_remove(this._timer);
            this._timer = 0;
        }
        this._actor?.destroy();
        this._actor = null;
    }
}

// Full-screen selection UI over a frozen still of the whole stage. Because the screen is frozen,
// menus and hover states survive, and the crop is exactly the pixels you were looking at.
// Ported from Shotput's SelectionOverlay.swift.

import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {CircleMask} from './effects.js';

const SNAP_DISTANCE = 7;
const LOUPE_SIZE = 116;
const LOUPE_ZOOM = 6;

const WINDOW_TYPES = new Set([
    Meta.WindowType.NORMAL, Meta.WindowType.DIALOG, Meta.WindowType.MODAL_DIALOG,
    Meta.WindowType.UTILITY, Meta.WindowType.SPLASHSCREEN,
]);

/** Normal windows on the current workspace, front to back, in stage coordinates. */
export function onScreenWindows() {
    const workspace = global.workspace_manager.get_active_workspace();
    const windows = global.display.list_all_windows().filter(w =>
        !w.minimized && w.showing_on_its_workspace?.() !== false &&
        (w.is_on_all_workspaces() || w.get_workspace() === workspace) &&
        WINDOW_TYPES.has(w.get_window_type()) && w.get_compositor_private());
    return global.display.sort_windows_by_stacking(windows).reverse().map(w => {
        const f = w.get_frame_rect();
        const app = Shell.WindowTracker.get_default().get_window_app(w);
        return {window: w, frame: {x: f.x, y: f.y, width: f.width, height: f.height}, ownerName: app?.get_name() ?? null};
    }).filter(i => i.frame.width >= 40 && i.frame.height >= 40);
}

const contains = (r, x, y) => x >= r.x && x <= r.x + r.width && y >= r.y && y <= r.y + r.height;

export class SelectionOverlay {
    /**
     * @param {object} params
     * @param {'area'|'window'} params.mode
     * @param {object} params.shot  { content, scale, cursor }
     * @param {Gio.Settings} params.settings
     * @param {boolean} params.showHint
     */
    constructor({mode, shot, settings, showHint}) {
        this.mode = mode;
        this._shot = shot;
        this._settings = settings;
        this._showHint = showHint;
        this._windows = onScreenWindows();
        this._anchor = null;
        this._selection = null;
        this._pointer = {x: 0, y: 0};
        this._hovered = null;
        this._movingSelection = false;
        this._spaceDown = false;
        this._resolve = null;
    }

    /** Shows the overlay. Resolves with {kind: 'area', rect} | {kind: 'window', info} | {kind: 'cancelled'}. */
    run() {
        return new Promise(resolve => {
            this._resolve = resolve;
            try {
                this._build();
            } catch (e) {
                // Never leave the screen frozen behind a broken overlay.
                logError(e, 'Shotput: opening the selection');
                this.finish({kind: 'cancelled'});
            }
        });
    }

    _build() {
        const {width, height} = global.stage;
        this.actor = new St.Widget({
            name: 'shotputOverlay', reactive: true, can_focus: true,
            x: 0, y: 0, width, height,
        });
        this._still = new Clutter.Actor({content: this._shot.content, width, height});
        this.actor.add_child(this._still);

        this._dims = [0, 1, 2, 3].map(() => {
            const d = new St.Widget({style_class: 'shotput-dim'});
            this.actor.add_child(d);
            return d;
        });
        this._outlineShadow = new St.Widget({style_class: 'shotput-outline-shadow', visible: false});
        this.actor.add_child(this._outlineShadow);
        this._outline = new St.Widget({style_class: 'shotput-outline', visible: false});
        this.actor.add_child(this._outline);

        this._hGuide = new St.Widget({style_class: 'shotput-guide', height: 1, width});
        this._vGuide = new St.Widget({style_class: 'shotput-guide', width: 1, height});
        this.actor.add_child(this._hGuide);
        this.actor.add_child(this._vGuide);

        // The magnifier is a second view onto the same still, scaled about the pointer.
        this._loupe = new St.Widget({width: LOUPE_SIZE, height: LOUPE_SIZE});
        this._loupeClip = new Clutter.Actor({width: LOUPE_SIZE, height: LOUPE_SIZE, clip_to_allocation: true});
        this._loupeImage = new Clutter.Actor({
            content: this._shot.content, width: width * LOUPE_ZOOM, height: height * LOUPE_ZOOM,
        });
        this._loupeImage.set_content_scaling_filters(Clutter.ScalingFilter.NEAREST, Clutter.ScalingFilter.NEAREST);
        this._loupeClip.add_child(this._loupeImage);
        const reticle = new St.Widget({
            style_class: 'shotput-reticle',
            x: Math.floor(LOUPE_SIZE / 2 - LOUPE_ZOOM / 2), y: Math.floor(LOUPE_SIZE / 2 - LOUPE_ZOOM / 2),
            width: LOUPE_ZOOM, height: LOUPE_ZOOM,
        });
        this._loupeClip.add_child(reticle);
        this._loupeClip.add_effect(new CircleMask());
        this._loupe.add_child(new St.Widget({style_class: 'shotput-loupe-shadow', width: LOUPE_SIZE, height: LOUPE_SIZE}));
        this._loupe.add_child(this._loupeClip);
        this._loupe.add_child(new St.Widget({style_class: 'shotput-loupe-ring', width: LOUPE_SIZE, height: LOUPE_SIZE}));
        this.actor.add_child(this._loupe);

        this._badge = new St.Label({style_class: 'shotput-badge', visible: false});
        this.actor.add_child(this._badge);

        if (this._showHint) {
            this._hint = new St.Label({
                style_class: 'shotput-hint',
                text: 'Drag to capture an area. Space picks a window. Esc cancels.',
            });
            this.actor.add_child(this._hint);
            const monitor = Main.layoutManager.currentMonitor ?? Main.layoutManager.primaryMonitor;
            const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
            this._hint.connect('notify::width', () => {
                this._hint.set_position(
                    Math.round(monitor.x + (monitor.width - this._hint.width) / 2),
                    Math.round(work.y + work.height - this._hint.height - 32));
            });
        }

        // Get rid of any popup menus; they are in the still anyway.
        Main.layoutManager.emit('system-modal-opened');
        const group = Main.layoutManager.screenshotUIGroup;
        group.get_parent().set_child_above_sibling(group, null);
        group.add_child(this.actor);

        this._grab = Main.pushModal(this.actor, {actionMode: Shell.ActionMode.POPUP});
        this.actor.grab_key_focus();

        // Events can still arrive while the overlay tears down; ignore them once it has finished.
        const live = handler => (_a, event) => this.actor ? handler(event) : Clutter.EVENT_PROPAGATE;
        this.actor.connect('motion-event', live(event => this._onMotion(event)));
        this.actor.connect('button-press-event', live(event => this._onPress(event)));
        this.actor.connect('button-release-event', live(event => this._onRelease(event)));
        this.actor.connect('key-press-event', live(event => this._onKeyPress(event)));
        this.actor.connect('key-release-event', live(event => this._onKeyRelease(event)));
        this.actor.connect('leave-event', () => {
            if (!this.actor || this._anchor)
                return Clutter.EVENT_PROPAGATE;
            this._hovered = null;
            this._setPointerChromeHidden(true);
            this._refresh();
            return Clutter.EVENT_PROPAGATE;
        });

        this._updateCursor();
        const [x, y] = global.get_pointer();
        this._pointerMoved(x, y, true);
    }

    toggleMode() {
        this.mode = this.mode === 'area' ? 'window' : 'area';
        this._anchor = null;
        this._selection = null;
        this._hovered = null;
        this._updateCursor();
        this._pointerMoved(this._pointer.x, this._pointer.y, true);
    }

    _updateCursor() {
        // A hand over windows, and a crosshair for areas.
        this.actor.set_cursor_type(this.mode === 'window' ? Clutter.CursorType.POINTER : Clutter.CursorType.CROSSHAIR);
    }

    finish(outcome) {
        const resolve = this._resolve;
        if (!resolve)
            return;
        this._resolve = null;
        if (this._grab) {
            Main.popModal(this._grab);
            this._grab = null;
        }
        // Destroying emits leave-event, so drop the reference first.
        const actor = this.actor;
        this.actor = null;
        actor?.destroy();
        resolve(outcome);
    }

    cancel() {
        this.finish({kind: 'cancelled'});
    }

    // MARK: Chrome

    _setPointerChromeHidden(hidden) {
        const area = this.mode === 'area';
        this._loupe.visible = !hidden && area && this._settings.get_boolean('shows-magnifier');
        const guides = !hidden && area && !this._anchor;
        this._hGuide.visible = guides;
        this._vGuide.visible = guides;
    }

    /** Redraws the dim, outline, and size badge for the current selection or hovered window. */
    _refresh() {
        const {width, height} = global.stage;
        let cut = null;
        let windowMode = false;
        if (this.mode === 'window' && this._hovered) {
            const f = this._hovered.frame;
            const x = Math.max(f.x, 0), y = Math.max(f.y, 0);
            cut = {x, y, width: Math.min(f.x + f.width, width) - x, height: Math.min(f.y + f.height, height) - y};
            windowMode = true;
        } else if (this.mode === 'area' && this._selection && this._selection.width > 0 && this._selection.height > 0) {
            cut = this._selection;
        }

        if (!cut) {
            this._dims[0].set_position(0, 0);
            this._dims[0].set_size(width, height);
            for (const d of this._dims.slice(1))
                d.set_size(0, 0);
            this._outline.visible = false;
            this._outlineShadow.visible = false;
            this._badge.visible = false;
            return;
        }
        const rects = [
            [0, 0, width, cut.y],
            [0, cut.y + cut.height, width, height - cut.y - cut.height],
            [0, cut.y, cut.x, cut.height],
            [cut.x + cut.width, cut.y, width - cut.x - cut.width, cut.height],
        ];
        rects.forEach(([x, y, w, h], i) => {
            this._dims[i].set_position(x, y);
            this._dims[i].set_size(Math.max(w, 0), Math.max(h, 0));
        });
        this._outline.style_class = windowMode ? 'shotput-outline shotput-outline-window' : 'shotput-outline';
        this._outline.set_position(cut.x, cut.y);
        this._outline.set_size(cut.width, cut.height);
        this._outline.visible = true;
        this._outlineShadow.visible = !windowMode;
        this._outlineShadow.set_position(cut.x - 1, cut.y - 1);
        this._outlineShadow.set_size(cut.width + 2, cut.height + 2);
        this._showBadge(cut, windowMode ? this._hovered.ownerName : null);
    }

    _showBadge(rect, name) {
        const scale = this._shot.scale;
        const dims = `${Math.round(rect.width * scale)} × ${Math.round(rect.height * scale)}`;
        this._badge.text = name ? `${name}  ${dims}` : dims;
        this._badge.visible = true;
        const [, natW] = this._badge.get_preferred_width(-1);
        const [, natH] = this._badge.get_preferred_height(-1);
        let x = rect.x + rect.width - natW;
        let y = rect.y + rect.height + 8;
        if (y + natH > global.stage.height - 4) {
            y = rect.y + rect.height - natH - 8;
            x -= 8;
        }
        x = Math.max(4, Math.min(x, global.stage.width - natW - 4));
        this._badge.set_position(Math.round(x), Math.round(y));
    }

    // MARK: Snapping

    /**
     * Pulls the point onto nearby window and screen edges. An edge only counts where it is
     * actually visible, so windows hidden behind others do not create phantom snaps.
     */
    _snapped(px, py, enabled) {
        const {width, height} = global.stage;
        let x = Math.round(px), y = Math.round(py);
        const clamp = (vx, vy) => [Math.min(Math.max(vx, 0), width), Math.min(Math.max(vy, 0), height)];
        if (!enabled || !this._settings.get_boolean('snaps-to-edges')) {
            [x, y] = clamp(x, y);
            return {x, y, hitX: false, hitY: false};
        }
        const reach = SNAP_DISTANCE;
        let bestX = reach, bestY = reach, hitX = false, hitY = false;
        const windows = this._windows;
        const isFrontmost = (probeX, probeY, index) =>
            !windows.slice(0, index).some(w => contains(w.frame, probeX, probeY));

        windows.forEach((w, index) => {
            const f = w.frame;
            if (py >= f.y - reach && py <= f.y + f.height + reach) {
                const probeY = Math.min(Math.max(py, f.y + 1), f.y + f.height - 1);
                for (const [edge, inward] of [[f.x, 1], [f.x + f.width, -1]]) {
                    if (Math.abs(edge - px) < bestX && isFrontmost(edge + inward, probeY, index)) {
                        bestX = Math.abs(edge - px);
                        x = edge;
                        hitX = true;
                    }
                }
            }
            if (px >= f.x - reach && px <= f.x + f.width + reach) {
                const probeX = Math.min(Math.max(px, f.x + 1), f.x + f.width - 1);
                for (const [edge, inward] of [[f.y, 1], [f.y + f.height, -1]]) {
                    if (Math.abs(edge - py) < bestY && isFrontmost(probeX, edge + inward, index)) {
                        bestY = Math.abs(edge - py);
                        y = edge;
                        hitY = true;
                    }
                }
            }
        });

        // Screen edges, and the edges of the work area: the top bar and any docks.
        for (const monitor of Main.layoutManager.monitors) {
            if (!contains(monitor, px, py))
                continue;
            const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
            for (const edge of [monitor.x, monitor.x + monitor.width, work.x, work.x + work.width]) {
                if (Math.abs(edge - px) < bestX) {
                    bestX = Math.abs(edge - px);
                    x = edge;
                    hitX = true;
                }
            }
            for (const edge of [monitor.y, monitor.y + monitor.height, work.y, work.y + work.height]) {
                if (Math.abs(edge - py) < bestY) {
                    bestY = Math.abs(edge - py);
                    y = edge;
                    hitY = true;
                }
            }
        }
        [x, y] = clamp(x, y);
        return {x, y, hitX, hitY};
    }

    // MARK: Pointer

    _pointerMoved(rawX, rawY, snapping) {
        if (this.mode === 'window') {
            this._pointer = {x: rawX, y: rawY};
            this._hovered = this._windows.find(w => contains(w.frame, rawX, rawY)) ?? null;
            this._setPointerChromeHidden(true);
            this._refresh();
            return;
        }
        const snap = this._snapped(rawX, rawY, snapping);
        this._pointer = {x: snap.x, y: snap.y};
        this._setPointerChromeHidden(false);
        const {width, height} = global.stage;
        this._vGuide.set_position(snap.x, 0);
        this._hGuide.set_position(0, snap.y);
        this._vGuide.style_class = snap.hitX ? 'shotput-guide shotput-guide-snapped' : 'shotput-guide';
        this._hGuide.style_class = snap.hitY ? 'shotput-guide shotput-guide-snapped' : 'shotput-guide';

        const size = LOUPE_SIZE, gap = 22;
        let cx = snap.x + gap + size / 2;
        let cy = snap.y + gap + size / 2;
        if (cx + size / 2 > width - 8)
            cx = snap.x - gap - size / 2;
        if (cy + size / 2 > height - 8)
            cy = snap.y - gap - size / 2;
        this._loupe.set_position(Math.round(cx - size / 2), Math.round(cy - size / 2));
        this._loupeImage.set_position(
            Math.round(size / 2 - snap.x * LOUPE_ZOOM - LOUPE_ZOOM / 2),
            Math.round(size / 2 - snap.y * LOUPE_ZOOM - LOUPE_ZOOM / 2));
    }

    _snapAllowed(event) {
        // Hold Ctrl to place freely.
        return (event.get_state() & Clutter.ModifierType.CONTROL_MASK) === 0;
    }

    _onMotion(event) {
        const [x, y] = event.get_coords();
        const previous = this._pointer;
        this._pointerMoved(x, y, this._snapAllowed(event));
        if (this.mode !== 'area' || !this._anchor)
            return Clutter.EVENT_STOP;

        const {width, height} = global.stage;
        if (this._movingSelection && this._selection) {
            // Space held: slide the whole selection.
            const cur = this._selection;
            const nx = Math.min(Math.max(cur.x + this._pointer.x - previous.x, 0), width - cur.width);
            const ny = Math.min(Math.max(cur.y + this._pointer.y - previous.y, 0), height - cur.height);
            this._anchor = {x: this._anchor.x + nx - cur.x, y: this._anchor.y + ny - cur.y};
            this._selection = {x: nx, y: ny, width: cur.width, height: cur.height};
        } else {
            let end = {...this._pointer};
            const start = this._anchor;
            if (event.get_state() & Clutter.ModifierType.SHIFT_MASK) {
                const side = Math.max(Math.abs(end.x - start.x), Math.abs(end.y - start.y));
                end = {
                    x: Math.min(Math.max(start.x + (end.x < start.x ? -side : side), 0), width),
                    y: Math.min(Math.max(start.y + (end.y < start.y ? -side : side), 0), height),
                };
            }
            this._selection = {
                x: Math.min(start.x, end.x), y: Math.min(start.y, end.y),
                width: Math.abs(end.x - start.x), height: Math.abs(end.y - start.y),
            };
        }
        this._refresh();
        return Clutter.EVENT_STOP;
    }

    _onPress(event) {
        const button = event.get_button();
        if (button === Clutter.BUTTON_SECONDARY) {
            this.cancel();
            return Clutter.EVENT_STOP;
        }
        if (button !== Clutter.BUTTON_PRIMARY || this.mode !== 'area')
            return Clutter.EVENT_STOP;
        const [x, y] = event.get_coords();
        this._pointerMoved(x, y, this._snapAllowed(event));
        this._anchor = {...this._pointer};
        this._selection = {x: this._anchor.x, y: this._anchor.y, width: 0, height: 0};
        if (this._hint)
            this._hint.opacity = 0;
        this._setPointerChromeHidden(false);
        return Clutter.EVENT_STOP;
    }

    _onRelease(event) {
        if (event.get_button() !== Clutter.BUTTON_PRIMARY)
            return Clutter.EVENT_STOP;
        if (this.mode === 'window') {
            if (this._hovered)
                this.finish({kind: 'window', info: this._hovered});
            return Clutter.EVENT_STOP;
        }
        const sel = this._selection;
        this._anchor = null;
        this._movingSelection = false;
        if (!sel || sel.width < 4 || sel.height < 4) {
            this._selection = null;
            this._refresh();
            this._setPointerChromeHidden(false);
            return Clutter.EVENT_STOP;
        }
        const x = Math.floor(sel.x), y = Math.floor(sel.y);
        const rect = {
            x, y,
            width: Math.ceil(sel.x + sel.width) - x,
            height: Math.ceil(sel.y + sel.height) - y,
        };
        this.finish({kind: 'area', rect});
        return Clutter.EVENT_STOP;
    }

    _onKeyPress(event) {
        const key = event.get_key_symbol();
        if (key === Clutter.KEY_Escape) {
            this.cancel();
            return Clutter.EVENT_STOP;
        }
        if (key === Clutter.KEY_space) {
            if (this._spaceDown)
                return Clutter.EVENT_STOP;
            this._spaceDown = true;
            if (this._anchor)
                this._movingSelection = true;
            else
                this.toggleMode();
            return Clutter.EVENT_STOP;
        }
        return Clutter.EVENT_STOP;
    }

    _onKeyRelease(event) {
        if (event.get_key_symbol() === Clutter.KEY_space) {
            this._spaceDown = false;
            this._movingSelection = false;
        }
        return Clutter.EVENT_STOP;
    }
}

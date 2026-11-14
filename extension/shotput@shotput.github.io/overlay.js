// Full-screen selection UI over a frozen still of the whole stage. Because the screen is frozen,
// menus and hover states survive, and the crop is exactly the pixels you were looking at.
// Ported from Shotput's SelectionOverlay.swift.
//
// Everything drawn over the still is a plain rectangle placed on the physical pixel grid, so lines
// stay one crisp pixel wide while they move, even with fractional scaling. The dim and the chrome
// fade in over the still (which matches the live screen, so nothing jumps), and fade out again
// when the overlay closes, revealing the live screen underneath instead of cutting to it.

import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {CircleMask} from './effects.js';
import {setCursor, resetCursor} from './compat.js';

const SNAP_DISTANCE = 7;
const LOUPE_SIZE = 116;
const LOUPE_ZOOM = 6;
const FADE_IN_MS = 140;
const FADE_OUT_MS = 170;
const CANCEL_FADE_MS = 110;
/** How close the pointer may come to the hint bar before it gets out of the way. */
const HINT_CLEARANCE = 56;

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
const containsHalfOpen = (r, x, y) => x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;

/** Overlays still fading out. A new capture removes them first so they never end up in its still. */
const fading = new Set();

/** Removes any overlay that is still fading out, straight away. */
export function flushFadingOverlays() {
    for (const actor of [...fading]) {
        // Stopping the fade usually destroys the actor through its onStopped; make sure of it.
        actor.remove_all_transitions();
        if (fading.delete(actor))
            actor.destroy();
    }
}

/** Hint lines: keycaps and what they do, by mode. */
const HINTS = {
    area: [['Drag', 'area'], ['Space', 'window'], ['Enter', 'full screen'], ['←↑→↓', 'nudge'], ['Esc', 'cancel']],
    window: [['Click', 'window'], ['Space', 'area'], ['Enter', 'this window'], ['Esc', 'cancel']],
};

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
        this._magnifier = settings.get_boolean('shows-magnifier');
        this._snaps = settings.get_boolean('snaps-to-edges');
        this._windows = onScreenWindows();
        this._anchor = null;
        this._selection = null;
        this._pointer = {x: 0, y: 0};
        this._hovered = null;
        this._movingSelection = false;
        this._spaceDown = false;
        this._warped = null;
        this._resolve = null;
        // One physical pixel, in stage units. Stage coordinates are logical, so with fractional
        // scaling a 1-unit line would land between pixels and flicker between one and two wide.
        this._scale = Math.max(shot.scale || 1, 1);
        this._line = Math.max(1, Math.round(this._scale)) / this._scale;
        this._edges = this._screenEdges();
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

    /** Rounds a stage coordinate onto the physical pixel grid. */
    _px(v) {
        return Math.round(v * this._scale) / this._scale;
    }

    _screenEdges() {
        return Main.layoutManager.monitors.map(monitor => {
            const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
            return {
                monitor,
                xs: [monitor.x, monitor.x + monitor.width, work.x, work.x + work.width],
                ys: [monitor.y, monitor.y + monitor.height, work.y, work.y + work.height],
            };
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

        // Everything over the still lives in one group, which fades in.
        this._chrome = new Clutter.Actor({width, height, opacity: 0});
        this.actor.add_child(this._chrome);
        const rect = (styleClass, visible = true) => {
            const a = new St.Widget({style_class: styleClass, visible});
            this._chrome.add_child(a);
            return a;
        };

        this._dims = [0, 1, 2, 3].map(() => rect('shotput-dim'));
        // The selection frame: a white line just outside the selection with a soft dark line
        // outside that, so it reads on light and dark content alike. Every captured pixel stays visible.
        this._frameDark = [0, 1, 2, 3].map(() => rect('shotput-frame-dark', false));
        this._frameLight = [0, 1, 2, 3].map(() => rect('shotput-frame-light', false));
        this._windowOutline = rect('shotput-outline-window', false);

        this._hGuide = rect('shotput-guide');
        this._vGuide = rect('shotput-guide');
        this._hGuide.set_size(width, this._line);
        this._vGuide.set_size(this._line, height);
        this._guideSnapped = [false, false];

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
        this._chrome.add_child(this._loupe);

        this._badge = new St.Label({style_class: 'shotput-badge', visible: false});
        this._badgeText = '';
        this._chrome.add_child(this._badge);

        // Get rid of any popup menus; they are in the still anyway.
        Main.layoutManager.emit('system-modal-opened');
        const group = Main.layoutManager.screenshotUIGroup;
        group.get_parent().set_child_above_sibling(group, null);
        group.add_child(this.actor);
        // The hint measures itself, so it is built once the overlay is on the stage.
        if (this._showHint)
            this._buildHint();

        this._grab = Main.pushModal(this.actor, {actionMode: Shell.ActionMode.POPUP});
        this.actor.grab_key_focus();

        // Events can still arrive while the overlay tears down; ignore them once it has finished.
        const live = handler => (_a, event) => this._resolve ? handler(event) : Clutter.EVENT_STOP;
        this.actor.connect('motion-event', live(event => this._onMotion(event)));
        this.actor.connect('button-press-event', live(event => this._onPress(event)));
        this.actor.connect('button-release-event', live(event => this._onRelease(event)));
        this.actor.connect('key-press-event', live(event => this._onKeyPress(event)));
        this.actor.connect('key-release-event', live(event => this._onKeyRelease(event)));
        this.actor.connect('leave-event', (_a, event) => {
            if (!this._resolve || this._anchor)
                return Clutter.EVENT_PROPAGATE;
            // Crossings inside the overlay (the modal grab starts with one) are not the pointer
            // leaving; hiding the crosshair for them made it pop in only on the first move.
            const related = event.get_related();
            if (related && this.actor.contains(related))
                return Clutter.EVENT_PROPAGATE;
            this._hovered = null;
            this._setPointerChromeHidden(true);
            this._refresh();
            return Clutter.EVENT_PROPAGATE;
        });

        this._updateCursor();
        const [x, y] = global.get_pointer();
        this._pointerMoved(x, y, true);
        this._refresh();

        // The still matches the live screen exactly, so only the dim and chrome need to appear.
        this._chrome.ease({opacity: 255, duration: FADE_IN_MS, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
    }

    // MARK: Hint

    _buildHint() {
        this._hint = new St.BoxLayout({style_class: 'shotput-hint', opacity: 0, reactive: false});
        this.actor.add_child(this._hint);
        this._fillHint();
        this._hintShown = true;
        this._hint.ease({opacity: 255, duration: FADE_IN_MS + 80, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
    }

    _fillHint() {
        if (!this._hint)
            return;
        this._hint.destroy_all_children();
        for (const [keys, what] of HINTS[this.mode]) {
            const item = new St.BoxLayout({style_class: 'shotput-hint-item'});
            item.add_child(new St.Label({style_class: 'shotput-keycap', text: keys, y_align: Clutter.ActorAlign.CENTER}));
            item.add_child(new St.Label({style_class: 'shotput-hint-text', text: what, y_align: Clutter.ActorAlign.CENTER}));
            this._hint.add_child(item);
        }
        // Lay it out now so it is placed before its first frame, rather than jumping into place.
        // A fixed size from the previous mode would cap the measurement, so drop it first.
        this._hint.set_size(-1, -1);
        const [, natW] = this._hint.get_preferred_width(-1);
        const [, natH] = this._hint.get_preferred_height(natW);
        const monitor = Main.layoutManager.currentMonitor ?? Main.layoutManager.primaryMonitor;
        const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
        this._hint.set_size(natW, natH);
        this._hint.set_position(
            Math.round(monitor.x + (monitor.width - natW) / 2),
            Math.round(work.y + work.height - natH - 32));
    }

    /** Hides the hint once a drag starts, and while the pointer is close enough to be working near it. */
    _syncHint() {
        if (!this._hint)
            return;
        const h = this._hint;
        const near = this._pointer.x > h.x - HINT_CLEARANCE && this._pointer.x < h.x + h.width + HINT_CLEARANCE &&
            this._pointer.y > h.y - HINT_CLEARANCE && this._pointer.y < h.y + h.height + HINT_CLEARANCE;
        // The size badge matters more than the hint, so the hint gives way when they would overlap.
        const b = this._badgeBox;
        const covers = this._badge.visible && b && b.x < h.x + h.width + 8 && b.x + b.width > h.x - 8 &&
            b.y < h.y + h.height + 8 && b.y + b.height > h.y - 8;
        const show = !this._anchor && !near && !covers;
        if (show === this._hintShown)
            return;
        this._hintShown = show;
        h.remove_all_transitions();
        h.ease({opacity: show ? 255 : 0, duration: 120, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
    }

    // MARK: Mode

    toggleMode() {
        this.mode = this.mode === 'area' ? 'window' : 'area';
        this._anchor = null;
        this._selection = null;
        this._hovered = null;
        this._movingSelection = false;
        this._updateCursor();
        this._fillHint();
        this._pointerMoved(this._pointer.x, this._pointer.y, true);
        this._refresh();
    }

    _updateCursor() {
        // A hand over windows, and a crosshair for areas.
        setCursor(this.actor, this.mode === 'window' ? 'pointer' : 'crosshair');
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
        resetCursor();
        const actor = this.actor;
        this.actor = null;
        if (actor) {
            // Input goes back to the desktop at once; the still fades out over the live screen,
            // so closing never cuts. The frame stays on the captured area while it goes.
            actor.reactive = false;
            this._loupe.hide();
            this._hGuide.hide();
            this._vGuide.hide();
            this._badge.hide();
            this._hint?.hide();
            const captured = outcome.kind !== 'cancelled';
            fading.add(actor);
            actor.ease({
                opacity: 0,
                duration: captured ? FADE_OUT_MS : CANCEL_FADE_MS,
                mode: Clutter.AnimationMode.EASE_OUT_QUAD,
                onStopped: () => {
                    fading.delete(actor);
                    actor.destroy();
                },
            });
        }
        resolve(outcome);
    }

    cancel() {
        this.finish({kind: 'cancelled'});
    }

    // MARK: Chrome

    _setPointerChromeHidden(hidden) {
        const area = this.mode === 'area';
        this._loupe.visible = !hidden && area && this._magnifier;
        const guides = !hidden && area && !this._anchor;
        this._hGuide.visible = guides;
        this._vGuide.visible = guides;
    }

    /** The rectangle left undimmed: the selection, or the hovered window. */
    _cut() {
        const {width, height} = global.stage;
        if (this.mode === 'window' && this._hovered) {
            const f = this._hovered.frame;
            const x = Math.max(f.x, 0), y = Math.max(f.y, 0);
            return {x, y, width: Math.min(f.x + f.width, width) - x, height: Math.min(f.y + f.height, height) - y, window: true};
        }
        if (this.mode === 'area' && this._selection && this._selection.width > 0 && this._selection.height > 0)
            return {...this._selection, window: false};
        return null;
    }

    static _place(actor, x, y, w, h) {
        actor.set_position(x, y);
        actor.set_size(Math.max(w, 0), Math.max(h, 0));
    }

    /** Four rectangles framing (x, y, w, h) from the outside, `t` thick. */
    static _frame(edges, x, y, w, h, t) {
        SelectionOverlay._place(edges[0], x - t, y - t, w + 2 * t, t);
        SelectionOverlay._place(edges[1], x - t, y + h, w + 2 * t, t);
        SelectionOverlay._place(edges[2], x - t, y, t, h);
        SelectionOverlay._place(edges[3], x + w, y, t, h);
    }

    /** Redraws the dim, frame, and size badge for the current selection or hovered window. */
    _refresh() {
        const {width, height} = global.stage;
        const cut = this._cut();
        const place = SelectionOverlay._place;

        if (!cut) {
            place(this._dims[0], 0, 0, width, height);
            for (const d of this._dims.slice(1))
                d.set_size(0, 0);
            for (const e of [...this._frameLight, ...this._frameDark])
                e.visible = false;
            this._windowOutline.visible = false;
            this._badge.visible = false;
            this._syncHint();
            return;
        }
        const x = this._px(cut.x), y = this._px(cut.y);
        const w = this._px(cut.x + cut.width) - x, h = this._px(cut.y + cut.height) - y;
        place(this._dims[0], 0, 0, width, y);
        place(this._dims[1], 0, y + h, width, height - y - h);
        place(this._dims[2], 0, y, x, h);
        place(this._dims[3], x + w, y, width - x - w, h);

        const t = this._line;
        for (const e of [...this._frameLight, ...this._frameDark])
            e.visible = !cut.window;
        this._windowOutline.visible = cut.window;
        if (cut.window) {
            place(this._windowOutline, x, y, w, h);
        } else {
            SelectionOverlay._frame(this._frameLight, x, y, w, h, t);
            SelectionOverlay._frame(this._frameDark, x - t, y - t, w + 2 * t, h + 2 * t, t);
        }
        this._showBadge({x, y, width: w, height: h}, cut.window ? this._hovered.ownerName : null);
        this._syncHint();
    }

    _showBadge(rect, name) {
        const scale = this._shot.scale;
        const dims = `${Math.round(rect.width * scale)} × ${Math.round(rect.height * scale)}`;
        const text = name ? `${name}  ·  ${dims}` : dims;
        if (text !== this._badgeText) {
            this._badgeText = text;
            this._badge.text = text;
        }
        this._badge.visible = true;
        const [, natW] = this._badge.get_preferred_width(-1);
        const [, natH] = this._badge.get_preferred_height(-1);
        const {width, height} = global.stage;
        let x = rect.x + rect.width - natW;
        let y = rect.y + rect.height + 8;
        if (y + natH > height - 4) {
            // No room below: tuck it inside the bottom edge.
            y = rect.y + rect.height - natH - 8;
            x -= 8;
        }
        x = Math.max(4, Math.min(x, width - natW - 4));
        y = Math.max(4, y);
        this._badge.set_position(Math.round(x), Math.round(y));
        this._badgeBox = {x: Math.round(x), y: Math.round(y), width: natW, height: natH};
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
        if (!enabled || !this._snaps) {
            [x, y] = clamp(this._px(px), this._px(py));
            return {x, y, hitX: false, hitY: false};
        }
        const reach = SNAP_DISTANCE;
        let bestX = reach, bestY = reach, hitX = false, hitY = false;
        const windows = this._windows;
        const isFrontmost = (probeX, probeY, index) => {
            for (let i = 0; i < index; i++) {
                if (contains(windows[i].frame, probeX, probeY))
                    return false;
            }
            return true;
        };

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
        for (const {monitor, xs, ys} of this._edges) {
            if (!contains(monitor, px, py))
                continue;
            for (const edge of xs) {
                if (Math.abs(edge - px) < bestX) {
                    bestX = Math.abs(edge - px);
                    x = edge;
                    hitX = true;
                }
            }
            for (const edge of ys) {
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
            this._syncHint();
            return;
        }
        const snap = this._snapped(rawX, rawY, snapping);
        this._pointer = {x: snap.x, y: snap.y};
        this._setPointerChromeHidden(false);
        this._syncHint();
        const {width, height} = global.stage;
        this._vGuide.set_position(this._px(snap.x), 0);
        this._hGuide.set_position(0, this._px(snap.y));
        if (snap.hitX !== this._guideSnapped[0]) {
            this._guideSnapped[0] = snap.hitX;
            this._vGuide.style_class = snap.hitX ? 'shotput-guide shotput-guide-snapped' : 'shotput-guide';
        }
        if (snap.hitY !== this._guideSnapped[1]) {
            this._guideSnapped[1] = snap.hitY;
            this._hGuide.style_class = snap.hitY ? 'shotput-guide shotput-guide-snapped' : 'shotput-guide';
        }

        if (!this._magnifier)
            return;
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

    _snapAllowed(event, x, y) {
        // A point placed with the arrow keys stays exactly where it was put.
        if (this._warped && Math.abs(this._warped.x - x) < 0.5 && Math.abs(this._warped.y - y) < 0.5)
            return false;
        this._warped = null;
        // Hold Ctrl to place freely.
        return (event.get_state() & Clutter.ModifierType.CONTROL_MASK) === 0;
    }

    _onMotion(event) {
        const [x, y] = event.get_coords();
        const previous = this._pointer;
        this._pointerMoved(x, y, this._snapAllowed(event, x, y));
        if (this.mode !== 'area' || !this._anchor) {
            this._refresh();
            return Clutter.EVENT_STOP;
        }

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
            // A right click backs out of a drag first, then out of the overlay.
            if (this._anchor) {
                this._resetDrag();
                return Clutter.EVENT_STOP;
            }
            this.cancel();
            return Clutter.EVENT_STOP;
        }
        if (button !== Clutter.BUTTON_PRIMARY || this.mode !== 'area')
            return Clutter.EVENT_STOP;
        const [x, y] = event.get_coords();
        this._pointerMoved(x, y, this._snapAllowed(event, x, y));
        this._anchor = {...this._pointer};
        this._selection = {x: this._anchor.x, y: this._anchor.y, width: 0, height: 0};
        this._setPointerChromeHidden(false);
        this._syncHint();
        return Clutter.EVENT_STOP;
    }

    _resetDrag() {
        this._anchor = null;
        this._selection = null;
        this._movingSelection = false;
        this._setPointerChromeHidden(false);
        this._syncHint();
        this._refresh();
    }

    _onRelease(event) {
        if (event.get_button() !== Clutter.BUTTON_PRIMARY)
            return Clutter.EVENT_STOP;
        if (this.mode === 'window') {
            if (this._hovered)
                this.finish({kind: 'window', info: this._hovered});
            return Clutter.EVENT_STOP;
        }
        if (!this._anchor)
            return Clutter.EVENT_STOP;
        const sel = this._selection;
        if (!sel || sel.width < 4 || sel.height < 4) {
            // A click without a drag: stay, ready for the next try.
            this._resetDrag();
            return Clutter.EVENT_STOP;
        }
        this._anchor = null;
        this._movingSelection = false;
        this.finish({kind: 'area', rect: this._captureRect(sel)});
        return Clutter.EVENT_STOP;
    }

    /** The selection as whole points, covering every pixel that was inside the frame. */
    _captureRect(sel) {
        const x = Math.floor(this._px(sel.x)), y = Math.floor(this._px(sel.y));
        return {
            x, y,
            width: Math.ceil(this._px(sel.x + sel.width)) - x,
            height: Math.ceil(this._px(sel.y + sel.height)) - y,
        };
    }

    /** The monitor under the pointer, as an area. */
    _monitorRect() {
        const {x, y} = this._pointer;
        const monitor = Main.layoutManager.monitors.find(m => containsHalfOpen(m, x, y)) ??
            Main.layoutManager.currentMonitor ?? Main.layoutManager.primaryMonitor;
        return {x: monitor.x, y: monitor.y, width: monitor.width, height: monitor.height};
    }

    _nudge(dx, dy) {
        const [x, y] = global.get_pointer();
        const {width, height} = global.stage;
        const nx = Math.min(Math.max(x + dx, 0), width - 1);
        const ny = Math.min(Math.max(y + dy, 0), height - 1);
        try {
            this._warped = {x: nx, y: ny};
            Clutter.get_default_backend().get_default_seat().warp_pointer(nx, ny);
        } catch (e) {
            this._warped = null;
            logError(e, 'Shotput: moving the pointer');
        }
    }

    _onKeyPress(event) {
        const key = event.get_key_symbol();
        const shift = (event.get_state() & Clutter.ModifierType.SHIFT_MASK) !== 0;
        switch (key) {
        case Clutter.KEY_Escape:
            this.cancel();
            return Clutter.EVENT_STOP;
        case Clutter.KEY_space:
            if (this._spaceDown)
                return Clutter.EVENT_STOP;
            this._spaceDown = true;
            if (this._anchor)
                this._movingSelection = true;
            else
                this.toggleMode();
            return Clutter.EVENT_STOP;
        case Clutter.KEY_Tab:
        case Clutter.KEY_ISO_Left_Tab:
            if (!this._anchor)
                this.toggleMode();
            return Clutter.EVENT_STOP;
        case Clutter.KEY_Return:
        case Clutter.KEY_KP_Enter:
        case Clutter.KEY_ISO_Enter:
            if (this.mode === 'window') {
                if (this._hovered)
                    this.finish({kind: 'window', info: this._hovered});
            } else if (this._anchor && this._selection?.width >= 4 && this._selection?.height >= 4) {
                this.finish({kind: 'area', rect: this._captureRect(this._selection)});
            } else {
                this.finish({kind: 'area', rect: this._monitorRect()});
            }
            return Clutter.EVENT_STOP;
        case Clutter.KEY_Left:
        case Clutter.KEY_Right:
        case Clutter.KEY_Up:
        case Clutter.KEY_Down: {
            const step = shift ? 10 : 1;
            const dx = key === Clutter.KEY_Left ? -step : key === Clutter.KEY_Right ? step : 0;
            const dy = key === Clutter.KEY_Up ? -step : key === Clutter.KEY_Down ? step : 0;
            this._nudge(dx, dy);
            return Clutter.EVENT_STOP;
        }
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

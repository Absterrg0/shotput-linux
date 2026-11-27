// Clipboard history: recent captures and their edited versions, newest first, kept in
// ~/.local/share/shotput/history so they survive a restart. The Shotput app reads and writes the
// same index.json and files (editor/src/history.rs), so its history window shows the same list.
// Every change rereads the index first, so either side can clear it.

import GdkPixbuf from 'gi://GdkPixbuf';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';

/** Disk use is capped too, so a run of 4K captures cannot fill it. The oldest go first. */
const MAX_BYTES = 256 * 1024 * 1024;
/** Thumbnails cover twice the top bar cell, for sharp HiDPI menus. */
const THUMB = {width: 272, height: 172};
const LIMITS = [5, 50];

const validId = id => typeof id === 'string' && /^[A-Za-z0-9-]{1,64}$/.test(id);

/** Drops entries past the count, then the oldest until the rest fit. The newest always stays. */
export function prune(entries, limit, maxBytes) {
    const dropped = entries.splice(Math.max(limit, 1));
    let total = entries.reduce((sum, e) => sum + (e.size ?? 0), 0);
    while (entries.length > 1 && total > maxBytes) {
        const last = entries.pop();
        total -= last.size ?? 0;
        dropped.unshift(last);
    }
    return dropped;
}

export class HistoryStore {
    constructor(settings) {
        this._settings = settings;
        this._dir = GLib.build_filenamev([GLib.get_user_data_dir(), 'shotput', 'history']);
        this._listeners = new Set();
    }

    get enabled() {
        return this._settings.get_boolean('keeps-history');
    }

    get limit() {
        return Math.min(Math.max(this._settings.get_int('history-size'), LIMITS[0]), LIMITS[1]);
    }

    get _index() {
        return GLib.build_filenamev([this._dir, 'index.json']);
    }

    path(id) {
        return GLib.build_filenamev([this._dir, `${id}.png`]);
    }

    thumbPath(id) {
        return GLib.build_filenamev([this._dir, `${id}-thumb.png`]);
    }

    onChange(listener) {
        this._listeners.add(listener);
        return () => this._listeners.delete(listener);
    }

    /** The items whose images are still there, newest first. */
    entries() {
        try {
            const [, data] = GLib.file_get_contents(this._index);
            const list = JSON.parse(new TextDecoder().decode(data));
            return Array.isArray(list)
                ? list.filter(e => validId(e?.id) && GLib.file_test(this.path(e.id), GLib.FileTest.EXISTS))
                : [];
        } catch {
            return [];
        }
    }

    get count() {
        return this.entries().length;
    }

    bytes(entry) {
        const [, data] = GLib.file_get_contents(this.path(entry.id));
        return new GLib.Bytes(data);
    }

    _save(entries) {
        try {
            GLib.mkdir_with_parents(this._dir, 0o700);
            GLib.file_set_contents(this._index, JSON.stringify(entries));
        } catch (e) {
            logError(e, 'Shotput: saving the clipboard history');
        }
        for (const listener of this._listeners)
            listener();
    }

    _deleteFiles(id) {
        for (const path of [this.path(id), this.thumbPath(id)]) {
            try {
                Gio.File.new_for_path(path).delete(null);
            } catch {}
        }
    }

    /**
     * Adds a capture at the front, or replaces the item with the same id and moves it there, so
     * an edited capture updates its entry instead of adding another. The image is written off the
     * main loop; the small thumbnail and the index follow.
     */
    record(capture) {
        if (!this.enabled || !validId(capture.id))
            return;
        GLib.mkdir_with_parents(this._dir, 0o700);
        const id = capture.id;
        const png = capture.png;
        const pixbuf = capture.pixbuf;
        const serial = (this._serials ??= new Map()).get(id) + 1 || 1;
        this._serials.set(id, serial);
        Gio.File.new_for_path(this.path(id)).replace_contents_bytes_async(png, null, false,
            Gio.FileCreateFlags.REPLACE_DESTINATION, null, (file, res) => {
                try {
                    file.replace_contents_finish(res);
                } catch (e) {
                    logError(e, 'Shotput: writing to the clipboard history');
                    return;
                }
                // A newer version of the same item was written meanwhile; it updates the index.
                if (this._serials.get(id) !== serial)
                    return;
                this._serials.delete(id);
                let size = png.get_size();
                try {
                    const w = pixbuf.get_width(), h = pixbuf.get_height();
                    const k = Math.min(Math.max(THUMB.width / w, THUMB.height / h), 1);
                    const small = k < 1
                        ? pixbuf.scale_simple(Math.max(1, Math.round(w * k)), Math.max(1, Math.round(h * k)), GdkPixbuf.InterpType.BILINEAR)
                        : pixbuf;
                    const [, thumb] = small.save_to_bufferv('png', [], []);
                    GLib.file_set_contents(this.thumbPath(id), thumb);
                    size += thumb.length;
                } catch {}
                const entry = {
                    id, date: Math.floor(GLib.get_real_time() / 1e6), appName: capture.appName ?? null,
                    scale: capture.scale, width: pixbuf.get_width(), height: pixbuf.get_height(), size,
                };
                const entries = [entry, ...this.entries().filter(e => e.id !== id)];
                for (const gone of prune(entries, this.limit, MAX_BYTES))
                    this._deleteFiles(gone.id);
                this._save(entries);
            });
    }

    remove(entry) {
        this._deleteFiles(entry.id);
        this._save(this.entries().filter(e => e.id !== entry.id));
    }

    /** Empties the history, files the index lost track of included. */
    clear() {
        try {
            const dir = Gio.File.new_for_path(this._dir);
            const children = dir.enumerate_children('standard::name', Gio.FileQueryInfoFlags.NONE, null);
            let info;
            while ((info = children.next_file(null))) {
                if (info.get_name().endsWith('.png'))
                    dir.get_child(info.get_name()).delete(null);
            }
        } catch {}
        this._save([]);
    }

    /** Applies a smaller limit straight away. */
    trim() {
        const entries = this.entries();
        const dropped = prune(entries, this.limit, MAX_BYTES);
        if (dropped.length === 0)
            return;
        for (const gone of dropped)
            this._deleteFiles(gone.id);
        this._save(entries);
    }
}

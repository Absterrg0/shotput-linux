// Shortcuts that fit Linux. Shotput uses the Print Screen key family by default, which GNOME's own
// screenshot tool also listens to, so while Shotput is on it borrows exactly the keys it uses from
// GNOME and gives them back when it is turned off. Everything else of GNOME's stays untouched.

import Gio from 'gi://Gio';

export const SHORTCUT_KEYS = [
    'shortcut-area', 'shortcut-area-annotate', 'shortcut-window', 'shortcut-screen',
    'shortcut-annotate-last', 'shortcut-repeat-area', 'shortcut-text', 'shortcut-annotate-clipboard',
];

/** GNOME's screenshot and recording bindings, which share the Print key with Shotput. */
const GNOME_SCHEMA = 'org.gnome.shell.keybindings';
const GNOME_KEYS = ['show-screenshot-ui', 'screenshot', 'screenshot-window', 'show-screen-recording-ui'];

/** The defaults before 1.1 put Super in every shortcut. Existing installs move to the new ones. */
const OLD_DEFAULTS = {
    'shortcut-area': '<Control><Alt><Super>a',
    'shortcut-area-annotate': '<Control><Alt><Super>d',
    'shortcut-window': '<Control><Alt><Super>w',
    'shortcut-screen': '<Control><Alt><Super>s',
    'shortcut-annotate-last': '<Control><Alt><Super>e',
    'shortcut-text': '<Control><Alt><Super>t',
};
const SHORTCUTS_VERSION = 2;

const MOD_ALIASES = {control: 'control', ctrl: 'control', primary: 'control', alt: 'alt', mod1: 'alt', shift: 'shift', super: 'super', mod4: 'super', meta: 'meta', hyper: 'hyper'};

/** A comparable form of an accelerator: `<Ctrl><Shift>a` and `<Shift><Control>A` match. */
export function normalizeAccel(accel) {
    if (!accel)
        return '';
    const mods = [...accel.matchAll(/<([^>]+)>/g)].map(m => MOD_ALIASES[m[1].toLowerCase()] ?? m[1].toLowerCase());
    const key = accel.replace(/<[^>]+>/g, '').toLowerCase();
    // Alt+Print arrives as Sys_Req on many keyboards; GNOME treats them as one.
    const name = key === 'sys_req' ? 'print' : key;
    return [...new Set(mods)].sort().map(m => `<${m}>`).join('') + name;
}

/** Moves shortcuts still on the old Super defaults to the new ones, once. */
export function migrateShortcuts(settings) {
    if (settings.get_int('shortcuts-version') >= SHORTCUTS_VERSION)
        return;
    for (const [key, old] of Object.entries(OLD_DEFAULTS)) {
        const current = settings.get_strv(key);
        if (!current.some(a => normalizeAccel(a) === normalizeAccel(old)))
            continue;
        const fresh = settings.get_default_value(key).deepUnpack();
        const kept = current.filter(a => normalizeAccel(a) !== normalizeAccel(old));
        const merged = [...kept];
        for (const a of fresh) {
            if (!merged.some(m => normalizeAccel(m) === normalizeAccel(a)))
                merged.push(a);
        }
        settings.set_strv(key, merged);
    }
    settings.set_int('shortcuts-version', SHORTCUTS_VERSION);
}

export class GnomeKeyHandoff {
    constructor(settings) {
        this._settings = settings;
        this._gnome = new Gio.Settings({schema_id: GNOME_SCHEMA});
    }

    _released() {
        try {
            const value = JSON.parse(this._settings.get_string('released-gnome-keys') || '{}');
            return value && typeof value === 'object' ? value : {};
        } catch {
            return {};
        }
    }

    _ours() {
        const set = new Set();
        for (const key of SHORTCUT_KEYS) {
            for (const a of this._settings.get_strv(key))
                set.add(normalizeAccel(a));
        }
        set.delete('');
        return set;
    }

    /**
     * Borrows from GNOME the accelerators Shotput now uses and returns any it no longer uses.
     * What was borrowed is remembered in Shotput's settings, so it survives a crash or logout.
     */
    sync() {
        const ours = this._settings.get_boolean('takes-over-screenshot-keys') ? this._ours() : new Set();
        const released = this._released();
        for (const key of GNOME_KEYS) {
            const current = this._gnome.get_strv(key);
            const borrowed = released[key] ?? [];
            const giveBack = borrowed.filter(a => !ours.has(normalizeAccel(a)));
            const stillBorrowed = borrowed.filter(a => ours.has(normalizeAccel(a)));
            const take = current.filter(a => ours.has(normalizeAccel(a)));
            let next = current.filter(a => !ours.has(normalizeAccel(a)));
            for (const a of giveBack) {
                if (!next.some(n => normalizeAccel(n) === normalizeAccel(a)))
                    next.push(a);
            }
            const nextBorrowed = [...stillBorrowed, ...take.filter(a => !stillBorrowed.includes(a))];
            if (nextBorrowed.length)
                released[key] = nextBorrowed;
            else
                delete released[key];
            if (next.length !== current.length || next.some((a, i) => a !== current[i]))
                this._gnome.set_strv(key, next);
        }
        this._settings.set_string('released-gnome-keys', JSON.stringify(released));
    }

    /** Gives every borrowed key back to GNOME. */
    restore() {
        const released = this._released();
        for (const [key, accels] of Object.entries(released)) {
            if (!GNOME_KEYS.includes(key))
                continue;
            const current = this._gnome.get_strv(key);
            const next = [...current];
            for (const a of accels) {
                if (!next.some(n => normalizeAccel(n) === normalizeAccel(a)))
                    next.push(a);
            }
            if (next.length !== current.length)
                this._gnome.set_strv(key, next);
        }
        this._settings.set_string('released-gnome-keys', '{}');
    }
}

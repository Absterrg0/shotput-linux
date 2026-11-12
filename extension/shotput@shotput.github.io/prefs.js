// Shotput's settings, one page per area, ported from SettingsPanes.swift and ShortcutRecorder.swift.

import Adw from 'gi://Adw';
import Gdk from 'gi://Gdk';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

const DEFAULT_TEMPLATE = 'Shotput %Y-%m-%d at %H.%M.%S';

const ACTIONS = [
    ['shortcut-area', 'Capture area', 'Drag an area. It is on the clipboard when you let go.'],
    ['shortcut-area-annotate', 'Capture area and annotate', 'Same, then the editor opens where you captured.'],
    ['shortcut-window', 'Capture window', 'Click a window to get it alone, with clean corners.'],
    ['shortcut-screen', 'Capture screen', 'The whole display under the pointer.'],
    ['shortcut-annotate-last', 'Annotate last capture', 'Opens the most recent capture in the editor.'],
    ['shortcut-repeat-area', 'Repeat last area', 'Captures the same area as last time, with no selection.'],
    ['shortcut-text', 'Copy text from area', 'Drag over any text and it is copied as text.'],
    ['shortcut-annotate-clipboard', 'Annotate clipboard image', 'Opens the image on the clipboard in the editor.'],
];

const TOKENS = [
    ['%Y', 'Year, 2026'], ['%y', 'Year, 26'], ['%m', 'Month, 07'], ['%b', 'Month, Jul'],
    ['%B', 'Month, July'], ['%d', 'Day, 09'], ['%j', 'Day of year, 190'], ['%A', 'Weekday, Friday'],
    ['%a', 'Weekday, Fri'], ['%H', 'Hour, 17'], ['%I', 'Hour, 05'], ['%p', 'AM or PM'],
    ['%M', 'Minute, 08'], ['%S', 'Second, 04'], ['%n', 'App name'], ['%r', 'Random, k3qz'],
];

function fileName(template, date, appName) {
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
            result += 'k3qz';
        else if (token === '%')
            result += '%';
        else
            result += `%${token}`;
    }
    const cleaned = result.replaceAll('/', '-').replaceAll(':', '.').trim();
    return cleaned && !cleaned.startsWith('.') ? cleaned : 'Shotput';
}

function switchRow(settings, key, title, subtitle = null) {
    const row = new Adw.SwitchRow({title, subtitle: subtitle ?? ''});
    settings.bind(key, row, 'active', Gio.SettingsBindFlags.DEFAULT);
    return row;
}

function comboRow(settings, key, title, options, subtitle = null) {
    const model = Gtk.StringList.new(options.map(o => o[1]));
    const row = new Adw.ComboRow({title, model, subtitle: subtitle ?? ''});
    const sync = () => {
        const index = options.findIndex(o => o[0] === settings.get_string(key));
        row.selected = Math.max(index, 0);
    };
    sync();
    row.connect('notify::selected', () => {
        const value = options[row.selected]?.[0];
        if (value !== undefined && settings.get_string(key) !== value)
            settings.set_string(key, value);
    });
    settings.connect(`changed::${key}`, sync);
    return row;
}

function group(title = null, description = null) {
    return new Adw.PreferencesGroup({title: title ?? '', description: description ?? ''});
}

function acceleratorLabel(accel) {
    const [ok, key, mods] = Gtk.accelerator_parse(accel);
    return ok ? Gtk.accelerator_get_label(key, mods) : accel;
}

export default class ShotputPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        window.set_default_size(620, 720);
        window.search_enabled = true;
        window.add(this._general(settings, window));
        window.add(this._capture(settings));
        window.add(this._editor(settings));
        window.add(this._output(settings, window));
        window.add(this._shortcuts(settings, window));
        window.add(this._about());
    }

    // MARK: General

    _general(settings, window) {
        const page = new Adw.PreferencesPage({title: 'General', icon_name: 'emblem-system-symbolic'});

        const startup = group('Startup');
        startup.add(switchRow(settings, 'shows-panel-icon', 'Show in the top bar',
            'Click the icon for pins, captures and settings.'));
        startup.add(switchRow(settings, 'keeps-editor-ready', 'Keep the editor ready',
            'The editor waits in the background, so it opens instantly.'));
        page.add(startup);

        const feedback = group('Feedback');
        feedback.add(switchRow(settings, 'plays-sound', 'Play a sound on capture'));
        page.add(feedback);

        const links = group();
        links.add(switchRow(settings, 'allows-links', 'Allow shotput:// links',
            'Lets launchers and mouse software start a capture.'));
        page.add(links);

        const reset = group();
        const row = new Adw.ActionRow({title: 'All settings'});
        const button = new Gtk.Button({label: 'Reset…', valign: Gtk.Align.CENTER});
        button.connect('clicked', () => {
            const dialog = new Adw.AlertDialog({
                heading: 'Reset every setting?',
                body: 'Shortcuts and preferences go back to how Shotput ships. Your save folder stays.',
            });
            dialog.add_response('cancel', 'Cancel');
            dialog.add_response('reset', 'Reset');
            dialog.set_response_appearance('reset', Adw.ResponseAppearance.DESTRUCTIVE);
            dialog.connect('response', (_d, response) => {
                if (response !== 'reset')
                    return;
                for (const key of settings.settings_schema.list_keys()) {
                    if (!['save-folder', 'has-launched', 'hints-shown'].includes(key))
                        settings.reset(key);
                }
            });
            dialog.present(window);
        });
        row.add_suffix(button);
        reset.add(row);
        page.add(reset);
        return page;
    }

    // MARK: Capture

    _capture(settings) {
        const page = new Adw.PreferencesPage({title: 'Capture', icon_name: 'camera-photo-symbolic'});

        const selecting = group('While selecting');
        selecting.add(switchRow(settings, 'shows-magnifier', 'Show the magnifier'));
        selecting.add(switchRow(settings, 'snaps-to-edges', 'Snap to window edges', 'Hold Ctrl to place freely either way.'));
        selecting.add(comboRow(settings, 'selection-hints', 'Show the hint', [
            ['first-few', 'First few captures'], ['always', 'Always'], ['never', 'Never'],
        ]));
        page.add(selecting);

        const after = group('After a capture');
        after.add(comboRow(settings, 'after-capture', 'Then', [
            ['preview', 'Show a preview'], ['editor', 'Open the editor'], ['clipboard', 'Copy quietly'],
        ]));
        const corner = comboRow(settings, 'preview-corner', 'Preview appears', [
            ['automatic', 'Nearest the pointer'], ['bottom-left', 'Bottom left'], ['bottom-right', 'Bottom right'],
            ['top-left', 'Top left'], ['top-right', 'Top right'],
        ]);
        after.add(corner);
        const durations = [[4, '4 seconds'], [6, '6 seconds'], [10, '10 seconds'], [0, 'Until dismissed']];
        const duration = new Adw.ComboRow({title: 'Preview stays', model: Gtk.StringList.new(durations.map(d => d[1]))});
        const syncDuration = () => {
            const value = settings.get_double('preview-duration');
            const i = durations.findIndex(d => Math.abs(d[0] - value) < 0.01);
            duration.selected = i >= 0 ? i : 1;
        };
        syncDuration();
        duration.connect('notify::selected', () => settings.set_double('preview-duration', durations[duration.selected][0]));
        after.add(duration);
        const syncAfter = () => {
            const preview = settings.get_string('after-capture') === 'preview';
            corner.sensitive = preview;
            duration.sensitive = preview;
        };
        settings.connect('changed::after-capture', syncAfter);
        syncAfter();
        page.add(after);

        const windows = group('Windows');
        windows.add(switchRow(settings, 'window-shadow', 'Keep the window\'s shadow'));
        windows.add(comboRow(settings, 'window-background', 'Background', [
            ['transparent', 'None'], ['wallpaper', 'Wallpaper'], ['solid', 'Solid colour'],
        ]));
        const colorRow = new Adw.ActionRow({title: 'Colour'});
        const colorButton = new Gtk.ColorDialogButton({dialog: new Gtk.ColorDialog({with_alpha: false}), valign: Gtk.Align.CENTER});
        const rgba = new Gdk.RGBA();
        rgba.parse(settings.get_string('window-background-color'));
        colorButton.rgba = rgba;
        colorButton.connect('notify::rgba', () => {
            const c = colorButton.rgba;
            const hex = [c.red, c.green, c.blue].map(v => Math.round(v * 255).toString(16).padStart(2, '0')).join('');
            settings.set_string('window-background-color', `#${hex.toUpperCase()}`);
        });
        colorRow.add_suffix(colorButton);
        windows.add(colorRow);
        const padding = new Adw.SpinRow({
            title: 'Padding', subtitle: 'Points of background around the window',
            adjustment: new Gtk.Adjustment({lower: 0, upper: 160, step_increment: 8, page_increment: 16}),
        });
        settings.bind('window-padding', padding, 'value', Gio.SettingsBindFlags.DEFAULT);
        windows.add(padding);
        const syncWindows = () => {
            const bg = settings.get_string('window-background');
            colorRow.visible = bg === 'solid';
            padding.visible = bg !== 'transparent';
        };
        settings.connect('changed::window-background', syncWindows);
        syncWindows();
        page.add(windows);

        const screen = group('Screen');
        screen.add(switchRow(settings, 'shows-pointer', 'Include the pointer'));
        page.add(screen);
        return page;
    }

    // MARK: Editor

    _editor(settings) {
        const page = new Adw.PreferencesPage({title: 'Editor', icon_name: 'document-edit-symbolic'});
        const closing = group('Closing', 'Return always copies and closes. Esc first clears a selection or crop, then closes.');
        closing.add(comboRow(settings, 'escape-in-editor', 'Esc', [
            ['discard', 'Closes without copying'], ['copy', 'Copies and closes'],
        ]));
        closing.add(switchRow(settings, 'editor-closes-on-copy', 'Copy (Ctrl+C) also closes the editor'));
        page.add(closing);

        const drawing = group('Drawing');
        drawing.add(switchRow(settings, 'recognizes-shapes', 'Turn sketches into shapes',
            'A pen stroke that looks like a line, arrow, rectangle, or circle becomes one. Ctrl+Z keeps the sketch.'));
        page.add(drawing);

        const text = group('Text recognition', 'Read on this machine with Tesseract. Nothing leaves it.');
        text.add(switchRow(settings, 'ocr-keeps-line-breaks', 'Keep line breaks',
            'Off joins every line into one, which suits pasting a paragraph.'));
        const lang = new Adw.EntryRow({title: 'Languages (Tesseract codes, such as eng or eng+deu)'});
        settings.bind('ocr-language', lang, 'text', Gio.SettingsBindFlags.DEFAULT);
        text.add(lang);
        page.add(text);
        return page;
    }

    // MARK: Output

    _output(settings, window) {
        const page = new Adw.PreferencesPage({title: 'Output', icon_name: 'folder-symbolic'});
        const clipboard = group('Clipboard and files');
        clipboard.add(switchRow(settings, 'copy-at-point-size', 'Use standard resolution',
            'Halves the pixel size on HiDPI displays. Smaller images are quicker for an agent to read.'));
        page.add(clipboard);

        const saving = group('Saving', 'The clipboard always gets a PNG. Without a save folder, files go to Pictures/Screenshots.');
        const folderRow = new Adw.ActionRow({title: 'Save folder'});
        const folderLabel = new Gtk.Label({css_classes: ['dim-label'], ellipsize: 3, max_width_chars: 22});
        const clear = new Gtk.Button({label: 'Clear', valign: Gtk.Align.CENTER});
        const choose = new Gtk.Button({valign: Gtk.Align.CENTER});
        const syncFolder = () => {
            const folder = settings.get_string('save-folder');
            folderLabel.label = folder ? GLib.path_get_basename(folder) : 'Pictures/Screenshots';
            clear.visible = Boolean(folder);
            choose.label = folder ? 'Change' : 'Choose';
        };
        clear.connect('clicked', () => {
            settings.set_string('save-folder', '');
            settings.set_boolean('saves-every-capture', false);
        });
        choose.connect('clicked', () => {
            const dialog = new Gtk.FileDialog({title: 'Save Folder'});
            dialog.select_folder(window, null, (d, res) => {
                try {
                    const file = d.select_folder_finish(res);
                    if (file?.get_path())
                        settings.set_string('save-folder', file.get_path());
                } catch {}
            });
        });
        settings.connect('changed::save-folder', syncFolder);
        syncFolder();
        folderRow.add_suffix(folderLabel);
        folderRow.add_suffix(clear);
        folderRow.add_suffix(choose);
        saving.add(folderRow);
        const every = switchRow(settings, 'saves-every-capture', 'Save every capture there');
        saving.add(every);
        saving.add(comboRow(settings, 'save-format', 'Format', [['png', 'PNG'], ['jpeg', 'JPEG']]));
        const quality = new Adw.ActionRow({title: 'Quality'});
        const scale = new Gtk.Scale({
            adjustment: new Gtk.Adjustment({lower: 0.4, upper: 1, step_increment: 0.05}),
            hexpand: true, width_request: 160, draw_value: false, valign: Gtk.Align.CENTER,
        });
        const qualityLabel = new Gtk.Label({css_classes: ['dim-label', 'numeric'], width_chars: 5});
        settings.bind('jpeg-quality', scale.adjustment, 'value', Gio.SettingsBindFlags.DEFAULT);
        const syncQuality = () => {
            qualityLabel.label = `${Math.round(settings.get_double('jpeg-quality') * 100)}%`;
            quality.visible = settings.get_string('save-format') !== 'png';
        };
        settings.connect('changed::jpeg-quality', syncQuality);
        settings.connect('changed::save-format', syncQuality);
        syncQuality();
        quality.add_suffix(scale);
        quality.add_suffix(qualityLabel);
        saving.add(quality);
        page.add(saving);

        const names = group('File name');
        const template = new Adw.EntryRow({title: 'Template'});
        settings.bind('filename-template', template, 'text', Gio.SettingsBindFlags.DEFAULT);
        names.add(template);
        const example = new Adw.ActionRow({title: 'Example'});
        const exampleLabel = new Gtk.Label({css_classes: ['dim-label'], ellipsize: 2, selectable: true, max_width_chars: 36});
        const syncExample = () => {
            const ext = settings.get_string('save-format') === 'jpeg' ? 'jpg' : 'png';
            exampleLabel.label = `${fileName(settings.get_string('filename-template'), GLib.DateTime.new_now_local(), 'Firefox')}.${ext}`;
        };
        settings.connect('changed::filename-template', syncExample);
        settings.connect('changed::save-format', syncExample);
        syncExample();
        example.add_suffix(exampleLabel);
        names.add(example);

        const tokensRow = new Adw.ActionRow({title: 'Tokens'});
        const grid = new Gtk.Grid({column_spacing: 16, row_spacing: 8, margin_top: 12, margin_bottom: 12, margin_start: 12, margin_end: 12});
        const popover = new Gtk.Popover({child: grid});
        TOKENS.forEach(([token, meaning], i) => {
            const button = new Gtk.Button({css_classes: ['flat']});
            const box = new Gtk.Box({spacing: 6});
            box.append(new Gtk.Label({label: token, css_classes: ['monospace', 'accent']}));
            box.append(new Gtk.Label({label: meaning, css_classes: ['dim-label']}));
            button.set_child(box);
            button.connect('clicked', () => settings.set_string('filename-template', settings.get_string('filename-template') + token));
            grid.attach(button, i % 2, Math.floor(i / 2), 1, 1);
        });
        const insert = new Gtk.MenuButton({label: 'Insert…', popover, valign: Gtk.Align.CENTER});
        tokensRow.add_suffix(insert);
        const resetTemplate = new Gtk.Button({label: 'Reset', css_classes: ['flat'], valign: Gtk.Align.CENTER});
        resetTemplate.connect('clicked', () => settings.set_string('filename-template', DEFAULT_TEMPLATE));
        tokensRow.add_suffix(resetTemplate);
        names.add(tokensRow);
        page.add(names);
        return page;
    }

    // MARK: Shortcuts

    _shortcuts(settings, window) {
        const page = new Adw.PreferencesPage({title: 'Shortcuts', icon_name: 'preferences-desktop-keyboard-shortcuts-symbolic'});
        const list = group(null, 'Click a shortcut, then press the new keys. Backspace clears it, Esc keeps it.');
        for (const [key, title, detail] of ACTIONS) {
            const row = new Adw.ActionRow({title, subtitle: detail});
            const button = new Gtk.Button({valign: Gtk.Align.CENTER, css_classes: ['flat']});
            const sync = () => {
                const accels = settings.get_strv(key);
                button.label = accels.length ? accels.map(acceleratorLabel).join(', ') : 'Record';
            };
            sync();
            settings.connect(`changed::${key}`, sync);
            button.connect('clicked', () => this._record(window, title, accel => {
                if (accel !== null)
                    settings.set_strv(key, accel ? [accel] : []);
            }));
            row.add_suffix(button);
            row.activatable_widget = button;
            list.add(row);
        }
        page.add(list);

        const mouse = group('Use a mouse button', 'Mouse software such as Solaar, Piper or Input Remapper can press any of these shortcuts for you: map a button to "keyboard shortcut" and press the Shotput shortcut you want on it.');
        page.add(mouse);
        return page;
    }

    /** Records the next key combination pressed. Calls back with '' to clear, or null to keep. */
    _record(window, title, done) {
        const dialog = new Adw.Dialog({title, content_width: 380, content_height: 200});
        const status = new Adw.StatusPage({
            icon_name: 'preferences-desktop-keyboard-shortcuts-symbolic',
            title: 'Press the new shortcut',
            description: 'Use Ctrl, Alt or Super with a key, or a function key on its own.\nBackspace clears it. Esc keeps it.',
        });
        status.add_css_class('compact');
        dialog.set_child(status);
        let finished = false;
        const finish = value => {
            if (finished)
                return;
            finished = true;
            done(value);
            dialog.close();
        };
        const keys = new Gtk.EventControllerKey();
        keys.connect('key-pressed', (_c, keyval, keycode, state) => {
            const mods = state & Gtk.accelerator_get_default_mod_mask();
            const lower = Gdk.keyval_to_lower(keyval);
            if (mods === 0 && lower === Gdk.KEY_Escape) {
                finish(null);
                return Gdk.EVENT_STOP;
            }
            if (mods === 0 && (lower === Gdk.KEY_BackSpace || lower === Gdk.KEY_Delete)) {
                finish('');
                return Gdk.EVENT_STOP;
            }
            if ([Gdk.KEY_Shift_L, Gdk.KEY_Shift_R, Gdk.KEY_Control_L, Gdk.KEY_Control_R, Gdk.KEY_Alt_L, Gdk.KEY_Alt_R,
                Gdk.KEY_Super_L, Gdk.KEY_Super_R, Gdk.KEY_Meta_L, Gdk.KEY_Meta_R, Gdk.KEY_ISO_Level3_Shift].includes(keyval))
                return Gdk.EVENT_STOP;
            const name = Gdk.keyval_name(lower) ?? '';
            const isFunctionKey = /^F([1-9]|1[0-9]|2[0-4])$/.test(name);
            const hasModifier = (mods & (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.ALT_MASK | Gdk.ModifierType.SUPER_MASK)) !== 0;
            if (!isFunctionKey && !hasModifier) {
                status.description = 'That needs Ctrl, Alt or Super, unless it is a function key.';
                window.get_display().beep();
                return Gdk.EVENT_STOP;
            }
            finish(Gtk.accelerator_name_with_keycode(null, lower, keycode, mods));
            return Gdk.EVENT_STOP;
        });
        dialog.add_controller(keys);
        dialog.connect('closed', () => finish(null));
        dialog.present(window);
    }

    // MARK: About

    _about() {
        const page = new Adw.PreferencesPage({title: 'About', icon_name: 'help-about-symbolic'});
        const about = group();
        const status = new Adw.StatusPage({
            icon_name: 'camera-photo-symbolic',
            title: 'Shotput',
            description: `Version ${this.metadata.version}\n\nScreenshots for pasting. A Linux port of Shotput for macOS, free and open source under the MIT licence. Nothing you capture leaves this machine.`,
        });
        about.add(status);
        const links = new Gtk.Box({spacing: 8, halign: Gtk.Align.CENTER});
        for (const [label, url] of [
            ['Original Project', 'https://github.com/keshav-exe/shotput'],
            ['Report an Issue', 'https://github.com/keshav-exe/shotput/issues/new'],
        ]) {
            const b = new Gtk.Button({label});
            b.connect('clicked', () => Gtk.show_uri(null, url, Gdk.CURRENT_TIME));
            links.append(b);
        }
        about.add(links);
        page.add(about);
        return page;
    }
}

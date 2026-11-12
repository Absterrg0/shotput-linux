// Test only. Loaded in a nested, headless gnome-shell with its own D-Bus session, never in a
// real login: it turns on unsafe mode so the test script can use Shell.Eval and Screenshot.
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

export default class Testkit extends Extension {
    enable() {
        global.context.unsafe_mode = true;
    }

    disable() {
        global.context.unsafe_mode = false;
    }
}

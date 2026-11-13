// The few places GNOME Shell's API changed between 46 (Ubuntu 24.04) and 50. Everything else
// Shotput uses is the same across those versions.

import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
import St from 'gi://St';

const ACTOR_CURSORS = typeof Clutter.Actor.prototype.set_cursor_type === 'function' && Clutter.CursorType;

/** Sets the pointer shape over an actor: 'crosshair', 'pointer' or 'default'. */
export function setCursor(actor, kind) {
    if (ACTOR_CURSORS) {
        const types = {crosshair: Clutter.CursorType.CROSSHAIR, pointer: Clutter.CursorType.POINTER, default: Clutter.CursorType.DEFAULT};
        actor.set_cursor_type(types[kind] ?? Clutter.CursorType.DEFAULT);
    } else {
        // GNOME 46 and 47 set the cursor for the whole display.
        const cursors = {crosshair: Meta.Cursor.CROSSHAIR, pointer: Meta.Cursor.POINTING_HAND, default: Meta.Cursor.DEFAULT};
        global.display.set_cursor(cursors[kind] ?? Meta.Cursor.DEFAULT);
    }
}

/** Puts the cursor back after a modal overlay on versions that set it display-wide. */
export function resetCursor() {
    if (!ACTOR_CURSORS)
        global.display.set_cursor(Meta.Cursor.DEFAULT);
}

/** An St.ImageContent filled with pixel bytes; GNOME 48 added a Cogl context argument. */
export function imageContent(bytes, format, width, height, rowstride, preferredWidth, preferredHeight) {
    const content = St.ImageContent.new_with_preferred_size(preferredWidth, preferredHeight);
    const backend = global.stage.context?.get_backend?.();
    if (backend)
        content.set_bytes(backend.get_cogl_context(), bytes, format, width, height, rowstride);
    else
        content.set_bytes(bytes, format, width, height, rowstride);
    return content;
}

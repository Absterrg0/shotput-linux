# Shotput for Linux

A GNOME port of [Shotput](https://github.com/keshav-exe/shotput) for macOS: screenshots for
pasting. Press a shortcut, drag an area, and it is on the clipboard when you let go. Annotate
first when the picture needs a pointer.

It has two parts:

- **A GNOME Shell extension** (`extension/`). On GNOME Wayland only the compositor can freeze the
  screen, snap a selection to window edges, float a preview that never takes focus, own the
  clipboard and keep a top bar icon, so captures happen there. The still is taken on the GPU
  (`Shell.Screenshot.screenshot_stage_to_content`), so the screen freezes the instant you press
  the shortcut.
- **The editor and `shotput` command** (`editor/`, Rust with GTK 4 and libadwaita). The
  screenshot is a GPU texture, and marks are render nodes with soft shadows drawn by GSK. Export
  renders the same nodes on the GPU at full resolution, so what you see is what you copy. The
  editor stays resident and is opened over D-Bus, so it appears instantly.

## Install

```bash
./install.sh
```

No root needed. It builds the editor, installs `~/.local/bin/shotput`, the app entry, a D-Bus
activation file and the extension, and sets up Tesseract for "Copy text" (the system one, or
Fedora's packages unpacked into `~/.local/share/shotput`). GNOME loads a newly installed extension
at the next login, so log out and back in once. Until then `shotput area` and `shotput annotate`
fall back to GNOME's own screenshot picker.

## Shortcuts

| Action | Default | What happens |
|---|---|---|
| Capture area | Ctrl+Alt+Super+A | Drag an area. It is on the clipboard when you let go. |
| Capture area and annotate | Ctrl+Alt+Super+D | Same, then the editor opens where you captured. |
| Capture window | Ctrl+Alt+Super+W | Click a window. You get it alone, with clean corners. |
| Capture screen | Ctrl+Alt+Super+S | The display under the pointer. |
| Annotate last capture | Ctrl+Alt+Super+E | Opens the most recent capture in the editor. |
| Copy text from area | Ctrl+Alt+Super+T | Drag over text and it is copied as text, read on this machine. |
| Repeat last area | none | Captures the same area again, with no selection. |
| Annotate clipboard image | none | Opens the image on the clipboard in the editor. |

Change any of them in Settings (the camera icon in the top bar, then Settings…). Pressing a
capture shortcut again while selecting backs out. Mouse software (Solaar, Piper, Input Remapper)
can press these shortcuts from a mouse button.

Launchers can also use the command or links: `shotput area`, `shotput annotate`,
`shotput window`, `shotput text`, `shotput last`, `shotput clipboard`, `shotput settings`, and
the same as `shotput://area` and so on. A silent full-screen capture is shortcut only, so another
program cannot use Shotput to read your screen.

## Selecting

- The screen freezes while you select, so open menus and hover states stay put.
- The crosshair snaps to window edges, the screen edges and the top bar. The guides turn blue when
  snapped. Hold Ctrl to place freely.
- Space switches between area and window. While dragging, hold Space to move the selection and
  Shift to keep it square.
- Esc or a right click cancels.

## After a capture

The image is on the clipboard straight away. A small preview then appears in the screen corner
nearest your pointer that does not cover what you captured.

- Click it to annotate. Hover for Copy again, Save and Pin.
- Drag it up towards the top bar and a shelf drops out; let go over it to pin.
- Ignore it and it leaves after a few seconds. It never takes focus.

## Pins

The top bar icon holds the last 15 pins in a strip that scrolls sideways. Click a pin to copy it,
Alt-click or middle-click to annotate it. The same menu has every capture and Settings.

## Annotating

| Tool | Key | Notes |
|---|---|---|
| Arrow | A | Shift snaps to 45° steps. Drag the middle handle to curve it. |
| Line | L | Drag the middle handle to curve it, double click it to straighten. |
| Rectangle | R | Shift for a square. Fill tints the inside. |
| Ellipse | O | Shift for a circle. |
| Text | T | Return finishes, Shift+Return adds a line. Double click to edit. |
| Step number | N | Counts up on each click. |
| Pen | P | Sketch a line, arrow, rectangle, or circle and it becomes one. Ctrl+Z keeps the sketch. |
| Highlighter | H | |
| Redact | B | Mosaic by default. Fill makes it a solid block. |
| Crop | C | |

Keys 1 to 8 pick a colour. Every mark has a thin halo and soft shadow. Drag a mark to move it,
drag its handles to reshape it, arrow keys nudge (Shift for 10). Delete removes it. Ctrl+scroll
or a pinch zooms; Ctrl+0 is actual size, Ctrl+9 fits.

**Return** copies the result and closes the editor. Ctrl+C copies and keeps it open, Ctrl+S
saves. The toolbar's Background button puts the screenshot on your wallpaper or a colour, with
padding, a soft shadow and rounded corners, and "Copy text" copies the text in the screenshot.

## Differences from the Mac version

- Captures happen in a GNOME Shell extension, because Wayland gives ordinary apps no way to freeze
  the screen or float a preview. It needs GNOME Shell 48 to 50.
- Right click on the top bar icon is reserved by GNOME's top bar, so the pins and the capture menu
  share one menu.
- Dragging the preview into another app as a file is not possible from the shell; drag it to the
  top bar to pin it, or paste it, since it is already on the clipboard.
- Text recognition uses Tesseract on this machine instead of Apple's Vision. HEIC saving is
  replaced by PNG and JPEG.

## Development

```bash
cd editor && cargo test --release              # model and shape recognition tests
./target/release/shotput dev-editor /tmp/out   # renders the editor with every kind of mark
tests/nested-shell.sh                          # headless nested GNOME Shell with the extension
tests/sh-eval.sh 'sp().run("area")'            # drive it (after loading tests/drive.js)
```

MIT licence, like the original. See `LICENSE`.

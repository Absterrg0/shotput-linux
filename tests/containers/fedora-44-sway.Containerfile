# Sway headless with software rendering: grim captures reliably, and swaymsg can move and click
# the pointer, so the native overlay and preview can be driven with real input. wl-clipboard reads
# the clipboard, and wtype types into the focused window (input gives it a serial to copy with).
FROM registry.fedoraproject.org/fedora:44
RUN dnf -y install --setopt=install_weak_deps=False sway grim gtk4-layer-shell gtk4 libadwaita dbus-daemon \
      dbus-tools xdg-desktop-portal xdg-desktop-portal-wlr adwaita-icon-theme librsvg2 google-noto-sans-fonts \
      glib2 procps-ng desktop-file-utils mesa-dri-drivers jq wl-clipboard wtype \
    && dnf clean all
RUN useradd -m tester
USER tester

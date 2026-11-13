# KDE Plasma 6's compositor (KWin) headless, with its portals and global shortcuts, for testing
# Shotput's own capture path. Fedora 44 matches the build machine, so the host binary runs as is.
FROM registry.fedoraproject.org/fedora:44
RUN dnf -y install --setopt=install_weak_deps=False kwin-wayland kglobalacceld xdg-desktop-portal \
      xdg-desktop-portal-kde gtk4-layer-shell gtk4 libadwaita dbus-daemon dbus-tools mesa-dri-drivers \
      adwaita-icon-theme breeze-icon-theme librsvg2 google-noto-sans-fonts python3-gobject glib2 procps-ng \
      desktop-file-utils \
    && dnf clean all
# KWin's screenshot service needs GL, so tests pass the host GPU in (KDE_GPU=1).
RUN dnf -y install --setopt=install_weak_deps=False libglvnd-gles libglvnd-egl mesa-libEGL mesa-libgbm \
    && dnf clean all
RUN useradd -m tester
USER tester

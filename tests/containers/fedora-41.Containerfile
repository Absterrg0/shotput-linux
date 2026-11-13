# GNOME 47, as on Fedora 41, for testing the extension headless.
FROM registry.fedoraproject.org/fedora:41
RUN dnf -y install --setopt=install_weak_deps=False gnome-shell gnome-extensions-app dbus-daemon dbus-tools \
      mesa-dri-drivers adwaita-icon-theme librsvg2 python3-gobject gtk4 libadwaita glib2 procps-ng \
    && dnf clean all
RUN useradd -m tester
USER tester

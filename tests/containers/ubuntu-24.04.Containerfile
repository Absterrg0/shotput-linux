# GNOME 46, as on Ubuntu 24.04 LTS, for testing the extension headless.
FROM docker.io/library/ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      gnome-shell gnome-shell-extension-prefs dbus dbus-x11 libgl1-mesa-dri mesa-vulkan-drivers \
      libgtk-4-1 libadwaita-1-0 gir1.2-gtk-4.0 python3-gi fonts-inter xdg-desktop-portal xdg-desktop-portal-gnome \
      gsettings-desktop-schemas glib-networking ca-certificates procps \
      adwaita-icon-theme librsvg2-common ubuntu-wallpapers-noble yaru-theme-gnome-shell yaru-theme-icon \
    && rm -rf /var/lib/apt/lists/*
RUN useradd -m tester
USER tester

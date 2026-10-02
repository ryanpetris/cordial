# Arch Linux packaging environment. The build context is a recipe directory; its PKGBUILD's
# dependencies are installed so makepkg can check them.
FROM archlinux:base-devel

COPY PKGBUILD /tmp/PKGBUILD
RUN pacman -Syu --noconfirm --needed git python \
    && bash -c 'CORDIAL_ARCHIVE_SHA256=SKIP source /tmp/PKGBUILD && pacman -S --noconfirm --needed "${depends[@]}"' \
    && pacman -Scc --noconfirm \
    && rm /tmp/PKGBUILD \
    && useradd -m builder
# makepkg refuses to run as root, so builds run as an unprivileged user that owns the sources.
RUN printf '%s\n' '#!/bin/sh' 'set -e' 'chown -R builder /src /out' 'exec runuser -u builder -- "$@"' \
        > /opt/cordial-run \
    && chmod 755 /opt/cordial-run

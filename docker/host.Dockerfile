# Build environment for the native desktop application, CLI and TUI.
# Resolute supplies the host archive and AppImage; Debian builds use their native libraries.
ARG BASE=ubuntu:26.04
FROM ${BASE}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git libadwaita-1-dev libgtk-4-dev \
        libudev-dev librsvg2-dev librsvg2-common libgirepository1.0-dev libglib2.0-bin \
        adwaita-icon-theme dconf-gsettings-backend file make pkg-config python3 \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_TOOLCHAIN=1.99.1 RUSTUP_HOME=/opt/rustup PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | CARGO_HOME=/opt/cargo sh -s -- -y --no-modify-path --profile minimal --default-toolchain none \
    && CARGO_HOME=/opt/cargo rustup toolchain install 1.99.1 --profile minimal \
    && chmod -R a+rX,go-w /opt/rustup

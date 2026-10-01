# Firmware build environment for the RP2040 and RP2350 boards.
FROM debian:13-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential ca-certificates clang cmake curl gcc-arm-none-eabi git \
        libclang-dev libnewlib-arm-none-eabi libusb-1.0-0 make ninja-build pkg-config \
        python3 python3-venv qemu-system-arm \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | CARGO_HOME=/opt/cargo sh -s -- -y --no-modify-path --profile minimal --default-toolchain none \
    && CARGO_HOME=/opt/cargo rustup toolchain install 1.98.1 --profile minimal \
        --target thumbv6m-none-eabi --target thumbv8m.main-none-eabihf \
    && chmod -R a+rwX /opt/rustup

# Build environment for the desktop application, its web version and the CLI. The toolchain has
# the targets rust/rust-toolchain.toml lists, so rustup never changes it at build time.
FROM node:24-trixie

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git libudev-dev make pkg-config python3 \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | CARGO_HOME=/opt/cargo sh -s -- -y --no-modify-path --profile minimal --default-toolchain none \
    && CARGO_HOME=/opt/cargo rustup toolchain install 1.98.1 --profile minimal \
        --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl \
        --target thumbv6m-none-eabi --target thumbv8m.main-none-eabihf \
    && chmod -R a+rX,go-w /opt/rustup

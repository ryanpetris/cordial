# Firmware build environment for the ESP32-S3 boards, on Espressif's ESP-IDF image.
FROM espressif/idf:v6.1

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git libusb-1.0-0 libudev-dev make pkg-config \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | CARGO_HOME=/opt/cargo sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable \
    && CARGO_HOME=/opt/cargo cargo install espup --version 0.17.1 --locked \
    && CARGO_HOME=/opt/cargo cargo install ldproxy --version 0.3.5 --locked \
    && CARGO_HOME=/opt/cargo espup install --toolchain-version 1.97.0.0 --targets esp32s3 --std \
        --export-file /opt/export-esp.sh \
    && rm -rf /opt/cargo/registry /opt/cargo/git \
    && chmod -R a+rwX /opt/rustup

COPY esp32s3-entrypoint.sh /opt/cordial-entrypoint.sh
ENTRYPOINT ["/opt/cordial-entrypoint.sh"]

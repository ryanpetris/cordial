# syntax=docker/dockerfile:1
# Builds a clean checkout with persistent dependency and compilation caches. Only the paths
# matched by OUTPUTS are exported. Supplied package archives come from /inputs.
ARG IMAGE
FROM ${IMAGE} AS toolchain
ARG CACHE_ID
ENV CARGO_HOME=/cache/cargo XDG_CACHE_HOME=/cache/xdg
WORKDIR /src

FROM toolchain AS build
ARG CORDIAL_VERSION=0.0.0
ARG SOURCE_DATE_EPOCH
ARG CORDIAL_HOMEPAGE=https://github.com/ryanpetris/cordial
ARG DEPENDENCY_ID
ARG COMMAND
ARG OUTPUTS
ENV CORDIAL_VERSION=${CORDIAL_VERSION} CORDIAL_HOMEPAGE=${CORDIAL_HOMEPAGE}
COPY --from=inputs . /inputs
COPY . /src
# COMMAND runs in its own shell so any failure in it fails the build. SOURCE_DATE_EPOCH is only set
# when supplied: compilers reject an empty value.
# ponytail: builds in one image serialize; split cache locks if parallel throughput needs it.
# Locked mounts protect SDK checkouts, generated configuration, and native build trees as well as
# Cargo outputs. Packages and /out stay outside the caches so wildcard exports are current.
RUN --mount=type=cache,id=cordial-downloads-${CACHE_ID},target=/cache,sharing=locked \
    --mount=type=cache,id=cordial-rust-${CACHE_ID},target=/src/rust/target,sharing=locked \
    --mount=type=cache,id=cordial-firmware-${CACHE_ID}-${DEPENDENCY_ID},target=/src/target,sharing=locked \
    --mount=type=cache,id=cordial-dependencies-${CACHE_ID}-${DEPENDENCY_ID},target=/src/.cache/dependencies,sharing=locked \
    set -eu; \
    test -n "$CACHE_ID"; test -n "$DEPENDENCY_ID"; \
    python3 docker/build-cache.py /src /cache/source-stamps.json; \
    mkdir -p /out; \
    if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then export SOURCE_DATE_EPOCH; else unset SOURCE_DATE_EPOCH; fi; \
    set -- bash -eu -o pipefail -c 'bash -eu -o pipefail -c "$COMMAND"; for pattern in $OUTPUTS; do for path in $pattern; do mkdir -p "/out/$(dirname "$path")"; cp -a "$path" "/out/$path"; done; done'; \
    if [ -x /opt/cordial-run ]; then /opt/cordial-run "$@"; else "$@"; fi

FROM scratch
COPY --from=build /out /

# syntax=docker/dockerfile:1
# Runs one build in a toolchain image. The checkout is copied in, COMMAND builds from scratch, and
# the paths matched by OUTPUTS are exported back to the same places in the checkout. Nothing is
# kept between builds. Files a build needs from outside the checkout, such as an archive to
# package, come from the "inputs" build context at /inputs.
ARG IMAGE
FROM ${IMAGE} AS build
ARG CORDIAL_VERSION=0.0.0
ARG SOURCE_DATE_EPOCH
ARG CORDIAL_HOMEPAGE=https://github.com/ryanpetris/cordial
ARG COMMAND
ARG OUTPUTS
ENV CORDIAL_VERSION=${CORDIAL_VERSION} CORDIAL_HOMEPAGE=${CORDIAL_HOMEPAGE} \
    CARGO_HOME=/cache/cargo npm_config_cache=/cache/npm
COPY --from=inputs . /inputs
COPY . /src
WORKDIR /src
# COMMAND runs in its own shell so any failure in it fails the build. SOURCE_DATE_EPOCH is only set
# when supplied: compilers reject an empty value.
RUN mkdir -p /out; \
    if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then export SOURCE_DATE_EPOCH; else unset SOURCE_DATE_EPOCH; fi; \
    set -- bash -eu -o pipefail -c 'bash -eu -o pipefail -c "$COMMAND"; for pattern in $OUTPUTS; do for path in $pattern; do mkdir -p "/out/$(dirname "$path")"; cp -a "$path" "/out/$path"; done; done'; \
    if [ -x /opt/cordial-run ]; then /opt/cordial-run "$@"; else "$@"; fi

FROM scratch
COPY --from=build /out /

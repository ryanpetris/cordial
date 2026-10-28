# Prerequisites only. Each validation runs in a fresh container with the checkout mounted.
ARG BASE=ubuntu:26.04
FROM ${BASE} AS base
COPY docker/check-prerequisites.sh packaging/debian/control packaging/arch/PKGBUILD /tmp/cordial-check/
WORKDIR /src

FROM base AS packages
ARG KIND
RUN bash /tmp/cordial-check/check-prerequisites.sh "$KIND"

FROM base AS desktop
RUN bash /tmp/cordial-check/check-prerequisites.sh desktop-resolute \
    && rm -rf /tmp/cordial-check

FROM packages AS prerequisites
RUN rm -rf /tmp/cordial-check

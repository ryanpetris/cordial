# Debian and Ubuntu packaging environment. BASE selects the distribution; the build context is a
# recipe directory whose control file lists the build dependencies.
ARG BASE=debian:13
FROM ${BASE}

ENV DEBIAN_FRONTEND=noninteractive
COPY control /tmp/control
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential debhelper devscripts dh-apparmor equivs git make python3 \
    && mk-build-deps --install --remove --tool 'apt-get -y --no-install-recommends' /tmp/control \
    && rm -rf /var/lib/apt/lists/* /tmp/control

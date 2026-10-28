set shell := ['bash', '-eu', '-o', 'pipefail', '-c']

export PYTHON := env('PYTHON', 'python3')
export BUF := env('BUF', 'buf')
export CORDIAL_VERSION := env('CORDIAL_VERSION', '0.0.0')
export CORDIAL_HOMEPAGE := env('CORDIAL_HOMEPAGE', 'https://github.com/ryanpetris/cordial')
SOURCE_DATE_EPOCH := env('SOURCE_DATE_EPOCH', '')
export HOST_TARGET := env('HOST_TARGET', 'x86_64-unknown-linux-gnu')
export BOARD := env('BOARD', 'pico_w')
export PROFILE := env('PROFILE', 'development')
export PROTOCOL_STABLE_BASE := env('PROTOCOL_STABLE_BASE', '')
export PROTOCOL_BASE := env('PROTOCOL_BASE', shell('python=' + quote(PYTHON) + '; CORDIAL_VERSION=' + quote(CORDIAL_VERSION) + ' $python tools/version.py --protocol-base 2>/dev/null || true'))
export DOCKER := env('DOCKER', 'docker')
export DEB_DISTRIBUTION := env('DEB_DISTRIBUTION', 'trixie')
export DEB_REVISION := env('DEB_REVISION', '1~' + DEB_DISTRIBUTION)
export HOST_ARCHIVE := env('HOST_ARCHIVE', '')
export CHECK_IMAGE_FLAGS := env('CHECK_IMAGE_FLAGS', '')
export BOARDS := 'pico_w pico2_w pimoroni_pico_plus2_w waveshare_rp2350b_plus_w esp32s3'
export ESP_BOARDS := 'esp32s3'
export FIRMWARE_PLATFORM := ''
firmware_platform := if FIRMWARE_PLATFORM != '' { FIRMWARE_PLATFORM } else if replace(' ' + ESP_BOARDS + ' ', ' ' + BOARD + ' ', '') != ' ' + ESP_BOARDS + ' ' { 'esp32s3' } else { 'pico' }
firmware_command := 'python3 rust/tools/build_firmware.py rust/boards/' + BOARD + '.json --profile ' + PROFILE
export DEB_BASE_trixie := 'debian:13'
export DEB_BASE_resolute := 'ubuntu:26.04'
export DEB_BASE := if DEB_DISTRIBUTION == 'trixie' { DEB_BASE_trixie } else if DEB_DISTRIBUTION == 'resolute' { DEB_BASE_resolute } else { '' }
host_archive := if HOST_ARCHIVE == '' { 'build/release/cordial-' + CORDIAL_VERSION + '-linux-amd64.tar.gz' } else { HOST_ARCHIVE }
build_host_archive := if HOST_ARCHIVE == '' { 'true' } else { 'false' }
just := quote(just_executable()) + ' SOURCE_DATE_EPOCH=' + quote(SOURCE_DATE_EPOCH) + ' BOARDS=' + quote(BOARDS) + ' ESP_BOARDS=' + quote(ESP_BOARDS) + ' FIRMWARE_PLATFORM=' + quote(FIRMWARE_PLATFORM)

[default]
help:
    @{{ quote(just_executable()) }} --list
    @printf '%s\n' {{ quote('Firmware options: BOARD=' + BOARD + ' PROFILE=' + PROFILE) }} {{ quote('Boards: ' + BOARDS) }}

all: host firmware

# Images supply toolchains; builds copy sources and export only their outputs.
image-host: (_image-host 'true')

[private]
_image-host needed:
    #!/usr/bin/env bash
    set -euo pipefail
    if {{ needed }}; then $DOCKER build -t cordial-host -f docker/host.Dockerfile docker; fi

image-host-trixie: (_image-host-trixie 'true')

[private]
_image-host-trixie needed:
    #!/usr/bin/env bash
    set -euo pipefail
    if {{ needed }}; then $DOCKER build -t cordial-host-trixie --build-arg "BASE=$DEB_BASE_trixie" -f docker/host.Dockerfile docker; fi

image-pico: (_image-firmware 'pico')
image-esp32s3: (_image-firmware 'esp32s3')

[private]
_image-firmware platform:
    $DOCKER build -t cordial-{{ platform }} -f docker/{{ platform }}.Dockerfile docker

image-arch:
    $DOCKER build -t cordial-arch --build-context tools=docker -f docker/arch.Dockerfile packaging/arch

image-deb: (_image-host-trixie (if DEB_DISTRIBUTION == 'trixie' { 'true' } else { 'false' }))
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$DEB_DISTRIBUTION" != trixie ] && [ -z "$DEB_BASE" ]; then echo "Unknown DEB_DISTRIBUTION $DEB_DISTRIBUTION" >&2; exit 1; fi
    $DOCKER build -t "cordial-deb-$DEB_DISTRIBUTION" --build-arg BASE={{ quote(if DEB_DISTRIBUTION == 'trixie' { 'cordial-host-trixie' } else { DEB_BASE }) }} --build-context tools=docker -f docker/deb.Dockerfile packaging/debian

# Docker reuses prerequisite layers until their inputs change. Refresh with CHECK_IMAGE_FLAGS='--pull --no-cache'.
image-check-package-arch: (_image-check 'package-arch' 'archlinux:base-devel')
image-check-package-deb: (_image-check ('package-' + DEB_DISTRIBUTION) DEB_BASE 'package-deb')
image-check-appimage-resolute: (_image-check 'appimage-resolute' DEB_BASE_resolute)
image-check-appimage-arch: (_image-check 'appimage-arch' 'archlinux:base')
image-check-desktop: (_image-check 'desktop-resolute' DEB_BASE_resolute 'desktop-resolute' 'desktop')

[private]
_image-check name base kind='' target='prerequisites':
    #!/usr/bin/env bash
    set -euo pipefail
    test -n {{ quote(base) }} || { echo "Unknown DEB_DISTRIBUTION $DEB_DISTRIBUTION" >&2; exit 1; }
    $DOCKER build $CHECK_IMAGE_FLAGS -t cordial-check-{{ name }} --target {{ target }} --build-arg BASE={{ quote(base) }} --build-arg KIND={{ quote(if kind == '' { name } else { kind }) }} -f docker/check.Dockerfile .

# Each task owns its export directory. Concurrent runs of one task use separate checkouts.
# Runtime contents select retained image tags and caches independently of export metadata.
[positional-arguments]
[private]
_build task image command outputs='' input='':
    #!/usr/bin/env bash
    set -euo pipefail
    task="$1"; image="$2"; command="$3"; outputs="$4"; input="$5"
    scratch=".cache/docker/$task"
    rm -rf "$scratch"
    mkdir -p "$scratch/inputs"
    if [ -n "$input" ]; then cp --reflink=auto "$input" "$scratch/inputs/"; fi
    image=$($DOCKER image inspect --format '{{ '{{' }}.Id}}' "cordial-$image")
    runtime_format='{{ '{{' }}json .RootFS}} {{ '{{' }}json .Config}} {{ '{{' }}json .Os}} {{ '{{' }}json .Architecture}} {{ '{{' }}json (or .Variant "")}}'
    runtime=$($DOCKER image inspect --format "$runtime_format" "$image")
    cache_key=$(printf '%s\n' "$runtime" | sha256sum | cut -d' ' -f1)
    image_ref="cordial-toolchain:$cache_key"
    cached_runtime=$($DOCKER image inspect --format "$runtime_format" "$image_ref" 2>/dev/null || true)
    if [ "$cached_runtime" != "$runtime" ]; then $DOCKER tag "$image" "$image_ref"; fi
    dependency_key=$(sha256sum rust/tools/firmware_dependencies.py | cut -d' ' -f1)
    $DOCKER build --progress=plain -f docker/build.Dockerfile --build-arg IMAGE="$image_ref" \
      --build-arg CACHE_ID="$cache_key" --build-arg DEPENDENCY_ID="$dependency_key" \
      --build-arg CORDIAL_VERSION --build-arg SOURCE_DATE_EPOCH={{ quote(SOURCE_DATE_EPOCH) }} --build-arg CORDIAL_HOMEPAGE \
      --build-arg COMMAND="$command" --build-arg OUTPUTS="$outputs" --build-context "inputs=$scratch/inputs" \
      --output "type=local,dest=$scratch/out" .
    read -r -a patterns <<< "$outputs"
    for pattern in "${patterns[@]}"; do
      for exported in "$scratch/out/"$pattern; do
        path=${exported#"$scratch/out/"}
        rm -rf "$path"
        mkdir -p "$(dirname "$path")"
        mv "$exported" "$path"
      done
    done

host: image-host
    {{ just }} _build host host "just HOST_TARGET=$HOST_TARGET local-host" "rust/target/$HOST_TARGET/release/cordial rust/target/$HOST_TARGET/release/share"

# Build and run locally with Cargo's development profile.
[positional-arguments]
cordial *args:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n {{ quote(SOURCE_DATE_EPOCH) }} ]; then export SOURCE_DATE_EPOCH={{ quote(SOURCE_DATE_EPOCH) }}; else unset SOURCE_DATE_EPOCH; fi
    cd rust
    executable=$(cargo build --locked -p cordial --bin cordial --message-format=json-render-diagnostics | $PYTHON -c '
    import json, sys
    executables = [message["executable"] for message in map(json.loads, sys.stdin)
                   if message.get("executable") and message["target"]["name"] == "cordial"]
    if not executables:
        sys.exit(0)
    [executable] = executables
    print(executable)
    ')
    schemas="$(dirname "$executable")/share/glib-2.0/schemas"
    mkdir -p "$schemas"
    glib-compile-schemas --strict --targetdir "$schemas" ../packaging
    cd {{ quote(invocation_directory()) }}
    exec "$executable" "$@"

local-host:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n {{ quote(SOURCE_DATE_EPOCH) }} ]; then export SOURCE_DATE_EPOCH={{ quote(SOURCE_DATE_EPOCH) }}; else unset SOURCE_DATE_EPOCH; fi
    $PYTHON tools/version.py
    schemas="rust/target/$HOST_TARGET/release/share/glib-2.0/schemas"
    mkdir -p "$schemas"
    cp packaging/dev.petris.cordial.gschema.xml "$schemas/"
    glib-compile-schemas --strict "$schemas"
    cd rust
    cargo build --locked --release -p cordial --bin cordial --target "$HOST_TARGET"

firmware: (_image-firmware firmware_platform)
    {{ just }} _build firmware {{ firmware_platform }} {{ quote(firmware_command) }} 'build/firmware/*/*'

check-and-build-firmware: (_image-firmware firmware_platform)
    {{ just }} _build check-and-build-firmware {{ firmware_platform }} {{ quote('time ' + firmware_command + ' --clippy && time ' + firmware_command) }} 'build/firmware/*/*'

firmware-all:
    for board in {{ BOARDS }}; do {{ just }} BOARD="$board" firmware; done

package-host-tar: (_host-tar 'true')

[private]
_host-tar needed: (_image-host needed)
    #!/usr/bin/env bash
    set -euo pipefail
    if {{ needed }}; then
      {{ just }} _build package-host-tar host "just HOST_TARGET=$HOST_TARGET local-package-host-tar" 'build/release/cordial-*-linux-amd64.tar.gz'
    fi

package-host-appimage: image-host (_host-tar build_host_archive)
    {{ just }} _build package-host-appimage host {{ quote('python3 rust/tools/build_appimage.py --archive ' + quote('/inputs/' + file_name(host_archive))) }} 'build/release/cordial-*-x86_64.AppImage' {{ quote(host_archive) }}

local-package-host-tar: local-host
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n {{ quote(SOURCE_DATE_EPOCH) }} ]; then export SOURCE_DATE_EPOCH={{ quote(SOURCE_DATE_EPOCH) }}; else unset SOURCE_DATE_EPOCH; fi
    $PYTHON rust/tools/package_host.py --target "$HOST_TARGET"

local-package-host-appimage: local-package-host-tar
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n {{ quote(SOURCE_DATE_EPOCH) }} ]; then export SOURCE_DATE_EPOCH={{ quote(SOURCE_DATE_EPOCH) }}; else unset SOURCE_DATE_EPOCH; fi
    version=$($PYTHON tools/version.py)
    $PYTHON rust/tools/build_appimage.py --archive "build/release/cordial-$version-linux-amd64.tar.gz"

# Each distribution build exports one package. Trixie builds against its native libraries.
package-host-arch: image-arch (_host-tar build_host_archive)
    {{ just }} _build package-host-arch arch {{ quote('just --justfile packaging/justfile HOST_ARCHIVE=' + quote('/inputs/' + file_name(host_archive)) + ' arch') }} "build/packages/host-arch/cordial-$CORDIAL_VERSION-1-x86_64.pkg.tar.zst" {{ quote(host_archive) }}

package-host-deb: image-deb (_host-tar (if DEB_DISTRIBUTION != 'trixie' { build_host_archive } else { 'false' }))
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$DEB_DISTRIBUTION" = trixie ]; then
      if [ -n "$HOST_ARCHIVE" ]; then echo 'Trixie packages build their executable on Debian 13; omit HOST_ARCHIVE' >&2; exit 1; fi
      {{ just }} _build package-host-deb deb-trixie "just HOST_TARGET=$HOST_TARGET local-package-host-tar && just --justfile packaging/justfile DEB_DISTRIBUTION=trixie DEB_REVISION=$DEB_REVISION deb" "build/packages/host-deb/trixie/cordial_$CORDIAL_VERSION-${DEB_REVISION}_amd64.deb"
    else
      {{ just }} _build package-host-deb "deb-$DEB_DISTRIBUTION" {{ quote('just --justfile packaging/justfile HOST_ARCHIVE=' + quote('/inputs/' + file_name(host_archive)) + ' DEB_DISTRIBUTION=' + quote(DEB_DISTRIBUTION) + ' DEB_REVISION=' + quote(DEB_REVISION) + ' deb') }} "build/packages/host-deb/$DEB_DISTRIBUTION/cordial_$CORDIAL_VERSION-${DEB_REVISION}_amd64.deb" {{ quote(host_archive) }}
    fi

# These recipes build current sources unless HOST_ARCHIVE supplies a checked release archive.
check-package-arch: package-host-arch image-check-package-arch
    time $DOCKER run --rm --init -v "$PWD:/src" -w /src -e CHECK_UID="$(id -u)" -e CHECK_GID="$(id -g)" -e CORDIAL_VERSION cordial-check-package-arch bash packaging/check-arch.sh

check-package-deb: package-host-deb image-check-package-deb
    time $DOCKER run --rm --init -v "$PWD:/src" -w /src -e CHECK_UID="$(id -u)" -e CHECK_GID="$(id -g)" -e CORDIAL_VERSION -e DEB_DISTRIBUTION -e DEB_REVISION cordial-check-package-{{ DEB_DISTRIBUTION }} bash packaging/check-deb.sh

check-appimage-resolute: package-host-appimage image-check-appimage-resolute
    time $DOCKER run --rm --init -v "$PWD:/src" -w /src -e CHECK_UID="$(id -u)" -e CHECK_GID="$(id -g)" -e CORDIAL_VERSION cordial-check-appimage-resolute bash docker/check-appimage.sh resolute

check-appimage-arch: package-host-appimage image-check-appimage-arch
    time $DOCKER run --rm --init -v "$PWD:/src" -w /src -e CHECK_UID="$(id -u)" -e CHECK_GID="$(id -g)" -e CORDIAL_VERSION cordial-check-appimage-arch bash docker/check-appimage.sh arch

check-installed-desktop: _require-resolute package-host-deb image-check-desktop
    #!/usr/bin/env bash
    set -euo pipefail
    time $DOCKER run --rm --init -v "$PWD:/src" -w /src -e CHECK_UID="$(id -u)" -e CHECK_GID="$(id -g)" -e CORDIAL_VERSION -e DEB_DISTRIBUTION -e DEB_REVISION cordial-check-desktop-resolute bash docker/check-installed-desktop.sh

[private]
_require-resolute:
    test "$DEB_DISTRIBUTION" = resolute || { echo 'Installed desktop checks require DEB_DISTRIBUTION=resolute' >&2; exit 1; }

check: check-rust check-protocol check-tools

check-gtk:
    $PYTHON rust/tools/test_gtk.py

# Check firmware Clippy in the board's toolchain image.
check-firmware: (_image-firmware firmware_platform)
    {{ just }} _build check-firmware {{ firmware_platform }} {{ quote(firmware_command + ' --clippy') }}

check-firmware-all:
    for board in {{ BOARDS }}; do for profile in production debug; do {{ just }} BOARD="$board" PROFILE="$profile" check-firmware; done; done

check-rust:
    #!/usr/bin/env bash
    set -euo pipefail
    $PYTHON rust/tools/firmware_dependencies.py --btstack
    cd rust
    cargo test --locked --workspace --all-features
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    $PYTHON tools/test_btstack.py

check-tools:
    JUST={{ quote(just_executable()) }} $PYTHON -m unittest discover -s tools -p 'test_*.py'
    $PYTHON -m unittest discover -s rust/tests -p 'test_*.py'

# Check permanent serial contracts and storage/migration IPC within a release series.
check-protocol:
    $BUF lint "$PWD/proto" --config buf.yaml
    $PYTHON tools/check_protocol.py --base "$PROTOCOL_BASE" --stable-base "$PROTOCOL_STABLE_BASE"

version:
    @$PYTHON tools/version.py

clean:
    rm -rf rust/target rust/platforms/pico/target rust/platforms/esp32s3/target build/packages build/firmware build/release target/firmware target/tmp

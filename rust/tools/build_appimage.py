#!/usr/bin/env python3
"""Bundle the unified Resolute host archive with GTK and libadwaita."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import quote
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from check_release_archive import check

TOOLS = {
    "linuxdeploy.AppImage": (
        "https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage",
        "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"),
    "linuxdeploy-plugin-gtk.sh": (
        "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/7a3fbc31a9e5075073ff8790f26effbac5f84453/linuxdeploy-plugin-gtk.sh",
        "b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171"),
    "appimagetool.AppImage": (
        "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage",
        "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"),
    "runtime-x86_64": (
        "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64",
        "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"),
}

# Bundle GTK's text stack while keeping compiler and window-system ABIs on the host.
EXPLICIT_LIBRARIES = (
    "libfontconfig.so.1", "libfreetype.so.6", "libfribidi.so.0", "libharfbuzz.so.0",
)
HOST_LIBRARIES = (
    "libstdc++.so.*", "libgcc_s.so.*", "libX*.so.*", "libxcb*.so.*",
    "libwayland-*.so.*", "libvulkan.so.*",
)

# GTK 4 resource paths in the Resolute build image.
GTK_HOOK = '''#!/bin/bash
export GTK_DATA_PREFIX="$APPDIR"
export XDG_DATA_DIRS="$APPDIR/usr/share:/usr/share:${XDG_DATA_DIRS:-}"
export GSETTINGS_SCHEMA_DIR="$APPDIR/usr/share/glib-2.0/schemas"
export GIO_MODULE_DIR="$APPDIR/usr/lib/gio/modules"
unset GIO_EXTRA_MODULES
export GI_TYPELIB_PATH="$APPDIR/usr/lib/girepository-1.0"
export GTK_EXE_PREFIX="$APPDIR/usr"
export GTK_PATH="$APPDIR/usr/lib/gtk-4.0"
export GDK_PIXBUF_MODULE_FILE="$APPDIR/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
'''

APP_RUN = '''#!/bin/bash
set -e
export APPDIR="${APPDIR:-$(dirname "$(readlink -f "$0")")}"
for hook in "$APPDIR"/apprun-hooks/*.sh; do
    if [ -f "$hook" ]; then . "$hook"; fi
done
if [ "$#" -eq 0 ] || [ "$1" = --hidden ]; then
    set -- desktop "$@"
fi
exec "$APPDIR/usr/bin/cordial" "$@"
'''


def prepare_tools(cache, directory):
    cache.mkdir(parents=True, exist_ok=True)
    directory.mkdir()
    for name, (url, digest) in TOOLS.items():
        cached = cache / name
        data = cached.read_bytes() if cached.exists() else b""
        if hashlib.sha256(data).hexdigest() != digest:
            with urlopen(url, timeout=120) as response:
                data = response.read()
            if hashlib.sha256(data).hexdigest() != digest:
                raise ValueError(f"AppImage tool checksum mismatch: {name}")
            cached.write_bytes(data)
        destination = directory / name
        destination.write_bytes(data)
        destination.chmod(0o755)


def appimage_environment(directory, epoch):
    return dict(os.environ, APPIMAGE_EXTRACT_AND_RUN="1", DEPLOY_GTK_VERSION="4",
                TMPDIR=str(directory.parent),
                LINUXDEPLOY_EXCLUDED_LIBRARIES=";".join(HOST_LIBRARIES),
                ARCH="x86_64", SOURCE_DATE_EPOCH=str(epoch),
                PATH=str(directory) + os.pathsep + os.defpath)


def prepare_gtk_plugin(plugin):
    # GTK 4.20 includes its modules in libgtk, so the module directory is optional.
    copy = '        copy_lib_tree "$gtk4_libdir" "$APPDIR/"\n'
    script = plugin.read_text()
    if script.count(copy) != 1:
        raise ValueError("Pinned GTK plugin module copy does not match")
    plugin.write_text(script.replace(copy,
        '        if [ -d "$gtk4_libdir" ]; then\n' + copy + '        fi\n'))


def gtk_data(appdir, share=Path("/usr/share")):
    for directory in ("icons/Adwaita", "icons/hicolor", "mime/packages"):
        shutil.copytree(share / directory, appdir / "usr/share" / directory, symlinks=True)
    # GdkPixbuf uses GIO's MIME database to select the image loader.
    shutil.copy2(share / "mime/mime.cache", appdir / "usr/share/mime/mime.cache")


def native_notices(appdir, documentation):
    """Match relocated system files to the build image's Debian package records."""
    owners = {}
    for listing in sorted(Path("/var/lib/dpkg/info").glob("*.list")):
        for path in listing.read_text().splitlines():
            owners.setdefault(path, listing.stem)
    multiarch = subprocess.check_output(["dpkg-architecture", "-qDEB_HOST_MULTIARCH"], text=True).strip()
    packages = {}
    own = {"usr/bin/cordial", "usr/share/applications/dev.petris.cordial.desktop",
           "usr/share/glib-2.0/schemas/dev.petris.cordial.gschema.xml",
           "usr/share/icons/hicolor/512x512/apps/dev.petris.cordial.png",
           "usr/share/icons/hicolor/scalable/apps/dev.petris.cordial.svg"}
    for path in sorted((appdir / "usr").rglob("*")):
        relative = path.relative_to(appdir).as_posix()
        if not path.is_file() or path.is_symlink() or relative in own:
            continue
        if relative.startswith("usr/share/doc/") or path.name in (
                "loaders.cache", "giomodule.cache", "gschemas.compiled", "icon-theme.cache",
                "mime.cache"):
            continue
        candidates = ["/" + relative]
        if relative.startswith("usr/lib/"):
            tail = relative.removeprefix("usr/lib/")
            candidates.extend((f"/usr/lib/{multiarch}/{tail}", f"/lib/{multiarch}/{tail}"))
        owner = next((owners[item] for item in candidates if item in owners), None)
        if owner is None:
            raise ValueError(f"No Debian source package for bundled file: {relative}")
        packages.setdefault(owner, []).append(relative)
    destination = documentation / "notices/native"
    destination.mkdir(parents=True)
    shutil.copytree("/usr/share/common-licenses", destination / "common-licenses")
    inventory = json.loads((documentation / "dependencies.json").read_text())
    for package, files in sorted(packages.items()):
        metadata = subprocess.check_output([
            "dpkg-query", "--show", "--showformat=${Version}\t${source:Package}\t${source:Version}",
            package], text=True).split("\t")
        version, source, source_version = metadata
        name = package.split(":")[0]
        notice = Path("/usr/share/doc") / name / "copyright"
        if not notice.is_file():
            raise ValueError(f"Missing redistribution notice for {package}")
        copied = destination / name / "copyright"
        copied.parent.mkdir(exist_ok=True)
        shutil.copyfile(notice, copied)
        inventory["native"].append({
            "name": name, "version": version, "source_package": source,
            "source_version": source_version,
            "source": f"https://launchpad.net/ubuntu/+source/{quote(source)}/{quote(source_version)}",
            "files": files,
            "notices": [{"file": copied.relative_to(documentation).as_posix(),
                         "sha256": hashlib.sha256(copied.read_bytes()).hexdigest()}]})
    runtime_notices(documentation, inventory)
    (documentation / "dependencies.json").write_text(json.dumps(inventory, indent=2) + "\n")
    with (documentation / "DEPENDENCIES.md").open("a") as stream:
        stream.write(
            "\n## AppImage native libraries\n\n"
            "The native entries in `dependencies.json` identify the bundled Debian packages, "
            "their source versions and the corresponding source download pages. "
            "`notices/native/` contains their redistribution notices and referenced common licences. "
            "The shared libraries remain separate files under `usr/lib/` in the extracted AppImage. "
            "`notices/native/appimage-runtime/` contains the runtime and static dependency "
            "licence texts, source revisions and build provenance.\n")


def runtime_notices(documentation, inventory):
    source = ROOT / "notices/appimage-runtime"
    manifest = json.loads((source / "sources.json").read_text())
    if manifest["runtime_sha256"] != TOOLS["runtime-x86_64"][1]:
        raise ValueError("AppImage runtime notices do not match the pinned runtime")
    destination = documentation / "notices/native/appimage-runtime"
    destination.mkdir(parents=True)
    shutil.copy2(source / "sources.json", destination / "sources.json")
    for component in manifest["components"]:
        entry = dict(component)
        entry["notices"] = []
        for notice in component["notices"]:
            path = source / notice["file"]
            if hashlib.sha256(path.read_bytes()).hexdigest() != notice["sha256"]:
                raise ValueError(f"AppImage runtime notice checksum mismatch: {path.name}")
            copied = destination / path.name
            shutil.copy2(path, copied)
            entry["notices"].append(dict(notice, file=copied.relative_to(documentation).as_posix()))
        inventory["native"].append(entry)


def build(archive, tool_cache):
    version = subprocess.check_output(["python3", "tools/version.py"], cwd=ROOT, text=True).strip()
    epoch = check(archive, "host", version)
    output = ROOT / "build/release"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".appimage-", dir=output) as temporary:
        temporary = Path(temporary)
        tools = temporary / "tools"
        prepare_tools(tool_cache, tools)
        prepare_gtk_plugin(tools / "linuxdeploy-plugin-gtk.sh")
        with tarfile.open(archive, "r:gz") as tar:
            tar.extractall(temporary, filter="data")
        payload = temporary / f"cordial-{version}-linux-amd64"
        appdir = temporary / "AppDir"
        documentation = appdir / "usr/share/doc/cordial"
        documentation.mkdir(parents=True)
        for path in payload.iterdir():
            if path.name in ("cordial", "share"):
                continue
            destination = documentation / path.name
            if path.is_dir():
                shutil.copytree(path, destination)
            else:
                shutil.copy2(path, destination)
        gtk_data(appdir)
        scalable = appdir / "usr/share/icons/hicolor/scalable/apps"
        scalable.mkdir(parents=True, exist_ok=True)
        shutil.copy2(payload / "dev.petris.cordial.svg", scalable / "dev.petris.cordial.svg")
        env = appimage_environment(tools, epoch)
        multiarch = subprocess.check_output(
            ["dpkg-architecture", "-qDEB_HOST_MULTIARCH"], text=True).strip()
        library_dir = Path("/usr/lib") / multiarch
        modules = appdir / "usr/lib/gio/modules"
        modules.mkdir(parents=True)
        shutil.copy2(library_dir / "gio/modules/libdconfsettings.so", modules)
        subprocess.run([
            str(tools / "linuxdeploy.AppImage"), "--appdir", str(appdir),
            *(f"--library={library_dir / name}" for name in EXPLICIT_LIBRARIES),
            "--deploy-deps-only", str(modules / "libdconfsettings.so"),
            "--executable", str(payload / "cordial"),
            "--desktop-file", str(payload / "dev.petris.cordial.desktop"),
            "--icon-file", str(payload / "dev.petris.cordial.png"), "--plugin", "gtk"],
            check=True, env=env)
        subprocess.run(["gio-querymodules", str(modules)], check=True)
        schemas = appdir / "usr/share/glib-2.0/schemas"
        schemas.mkdir(parents=True, exist_ok=True)
        shutil.copy2(payload / "share/glib-2.0/schemas/dev.petris.cordial.gschema.xml", schemas)
        subprocess.run(["glib-compile-schemas", "--strict", str(schemas)], check=True)
        hook = appdir / "apprun-hooks/linuxdeploy-plugin-gtk.sh"
        hook.write_text(GTK_HOOK)
        launcher = appdir / "AppRun"
        launcher.unlink(missing_ok=True)
        launcher.write_text(APP_RUN)
        launcher.chmod(0o755)
        native_notices(appdir, documentation)
        runtime_notice = subprocess.check_output(
            [str(tools / "runtime-x86_64"), "--appimage-help"], text=True,
            stderr=subprocess.STDOUT)
        (documentation / "notices/native/appimage-runtime.txt").write_text(
            "AppImage runtime\n\nLicense:\n" + runtime_notice.split("License:\n", 1)[1])
        files = sorted(path for path in documentation.rglob("*")
                       if path.is_file() and path.name != "SHA256SUMS")
        (documentation / "SHA256SUMS").write_text("".join(
            f"{hashlib.sha256(path.read_bytes()).hexdigest()}  ./{path.relative_to(documentation).as_posix()}\n"
            for path in files))
        for path in appdir.rglob("*"):
            os.utime(path, (epoch, epoch), follow_symlinks=False)
        name = f"cordial-{version}-x86_64.AppImage"
        subprocess.run([str(tools / "appimagetool.AppImage"), "--runtime-file",
                        str(tools / "runtime-x86_64"), str(appdir), str(temporary / name)],
                       check=True, env=env)
        (temporary / name).replace(output / name)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--tool-cache", type=Path, default=Path(
        os.environ.get("XDG_CACHE_HOME", ROOT / ".cache")) / "cordial-appimage")
    args = parser.parse_args()
    build(args.archive.resolve(), args.tool_cache.resolve())

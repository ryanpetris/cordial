#!/usr/bin/env python3
"""Launch a packaged AppImage and inspect its simulated desktop in a private X11 session."""
import argparse
import configparser
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import traceback

import pyatspi

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "rust/tests"))
from gtk_helpers import Desktop
from gtk_smoke import APP_ID, require_private_session
from gi.repository import Gio, GLib


def check(image, output, renderer=None):
    bus = require_private_session()
    if not shutil.which("gsettings"):
        raise RuntimeError("Install the host's gsettings command and dconf backend and service.")
    output.mkdir(parents=True, exist_ok=True)
    processes = []
    ui = None
    completed = []
    with tempfile.TemporaryDirectory(prefix="cordial-appimage-") as directory, \
            (output / "application.log").open("w") as log:
        root = Path(directory)
        env = dict(os.environ, APPIMAGE_EXTRACT_AND_RUN="1", CORDIAL_DESKTOP_SIMULATE="2",
                   GDK_BACKEND="x11", GTK_A11Y="atspi", GTK_USE_PORTAL="0",
                   GIO_USE_VFS="local", GIO_USE_VOLUME_MONITOR="unix",
                   LC_ALL="C.UTF-8")
        for key in ("GSETTINGS_BACKEND", "GSETTINGS_SCHEMA_DIR", "GIO_MODULE_DIR",
                    "GIO_EXTRA_MODULES"):
            env.pop(key, None)
        env.pop("GSK_RENDERER", None)
        if renderer:
            env["GSK_RENDERER"] = renderer
        for kind in ("CONFIG_HOME", "CACHE_HOME", "DATA_HOME", "RUNTIME_DIR"):
            path = root / kind.split("_")[0].lower()
            path.mkdir(mode=0o700)
            env[f"XDG_{kind}"] = str(path)
        profile = root / "dconf-profile"
        profile.write_text("user-db:user\n")
        env["DCONF_PROFILE"] = str(profile)
        names = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
            "org.freedesktop.DBus", "ListNames", None, GLib.VariantType.new("(as)"),
            Gio.DBusCallFlags.NONE, 3000, None).unpack()[0]
        if "ca.desrt.dconf" in names:
            raise RuntimeError("The dconf service is already running. Use a new dbus-run-session.")
        activation = {key: value for key, value in env.items()
                      if key.startswith("XDG_") or key == "DCONF_PROFILE"}
        bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
            "org.freedesktop.DBus", "UpdateActivationEnvironment",
            GLib.Variant("(a{ss})", (activation,)), None, Gio.DBusCallFlags.NONE, 3000, None)
        schemas = root / "schemas"

        def host_settings(*args):
            return subprocess.check_output([
                "gsettings", "--schemadir", str(schemas), args[0], APP_ID, *args[1:]],
                env=env, text=True, timeout=10).strip()

        def saved_login():
            return host_settings("get", "start-at-login")

        def launch(args=(), capture=False):
            process = subprocess.Popen([str(image), *args], cwd=root, env=env,
                stdout=subprocess.PIPE if capture else log, stderr=log,
                text=True, start_new_session=True)
            processes.append(process)
            return process

        def owner():
            try:
                return bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                    GLib.Variant("(s)", (APP_ID,)), GLib.VariantType.new("(u)"),
                    Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
            except GLib.Error:
                return None

        def attach(process):
            desktop = Desktop(process, output)
            # The AppImage runtime may keep its extraction process alive beside the application.
            try:
                pid = desktop.wait("acquire the desktop application name", owner, timeout=60)
                desktop.external_pids.add(pid)
                return desktop, pid
            except BaseException:
                desktop.x11.close()
                raise

        def hidden():
            ui.retain("keep the original instance hidden", lambda:
                owner() == pid and not ui.x11.windows(pid))

        def remote(args):
            process = launch(args)
            if process.wait(timeout=60) != 0:
                raise AssertionError(f"A second AppImage launch exited with {process.returncode}")
            if owner() != pid:
                raise AssertionError("A second AppImage launch replaced the original instance")

        def quit():
            ui.focus(ui.row("Overview"))
            ui.key("Control_L", "q")
            ui.wait("quit cleanly", lambda: ui.process.poll() is not None,
                    timeout=30, running=False)
            if ui.process.returncode != 0:
                raise AssertionError(f"Quit exited with {ui.process.returncode}")
            ui.wait("release the application name", lambda: owner() is None, running=False)

        try:
            for option, name in (("--version", "cli-version"), ("--help", "cli-help")):
                process = launch([option], capture=True)
                stdout, _ = process.communicate(timeout=60)
                (output / f"{name}.txt").write_text(stdout)
                if process.returncode != 0 or owner() is not None:
                    raise AssertionError(f"AppImage {option} did not route to the CLI")
                if option == "--version":
                    valid = re.fullmatch(r"cordial \S+\n?", stdout)
                else:
                    valid = all(value in stdout for value in
                                ("Usage: cordial", "desktop", "tui", "--version"))
                if not valid:
                    raise AssertionError(f"Unexpected AppImage {option} output: {stdout!r}")
                completed.append(name)

            ui, pid = attach(launch())
            bundle = Path(f"/proc/{pid}/exe").resolve().parent.parent
            shutil.copytree(bundle / "share/glib-2.0/schemas", schemas)
            ui.expect("Example Adapter 1")
            ui.expect("Example Adapter 2")
            modules = [line.split(maxsplit=5)[-1] for line in
                       Path(f"/proc/{pid}/maps").read_text().splitlines()
                       if "/gio/modules/" in line]
            if not modules or any(not path.startswith(str(bundle / "lib/gio/modules") + "/")
                                  for path in modules):
                raise AssertionError(f"GIO must load only bundled modules: {modules}")
            if not any(path.endswith("/libdconfsettings.so") for path in modules):
                raise AssertionError("The AppImage did not load its bundled dconf backend")
            completed.append("bundled-gio-modules")
            ui.wait("map the desktop window", lambda: ui.x11.windows(pid))
            ui.dump("launch")
            ui.screenshot("launch")
            completed.append("no-args-desktop")

            ui.focus(ui.row("Overview"))
            ui.key("Control_L", "comma")
            page = ui.settings_page()
            ui.switch("Start at Login", True, page)
            ui.wait("save Start at Login", lambda:
                saved_login() == "true")
            autostart = root / f"config/autostart/{APP_ID}.desktop"
            ui.wait("write the autostart entry", autostart.is_file)
            entry = configparser.ConfigParser(interpolation=None)
            entry.read(autostart)
            desktop = entry["Desktop Entry"]
            command = shlex.split(desktop["Exec"])
            command[0] = command[0].replace("%%", "%")
            if command != [str(image), "desktop", "--hidden"]:
                raise AssertionError(f"Unexpected autostart command: {desktop['Exec']!r}")
            if desktop["Type"] != "Application" or desktop["Terminal"] != "false":
                raise AssertionError("The autostart entry is not a background desktop application")
            ui.dump("preferences")
            ui.screenshot("preferences")
            ui.click(node=ui.row("Overview"))
            ui.absent("Start at Login")
            quit()
            if saved_login() != "true":
                raise AssertionError("The host's gsettings did not read Start at Login after quit")
            completed.append("default-backend-persistence")
            completed.extend(("appimage-autostart-entry", "quit-no-args"))
            ui.x11.close()

            ui, pid = attach(launch(["--hidden"]))
            hidden()
            remote(command[1:])
            hidden()
            ui.screenshot("hidden")
            completed.append("hidden-start-and-autostart")
            remote([])
            ui.expect("Example Adapter 1")
            ui.expect("Example Adapter 2")
            ui.wait("present the original desktop window", lambda: ui.x11.windows(pid))
            ui.dump("presented")
            ui.screenshot("presented")
            completed.append("present-original-instance")
            ui.click(node=ui.row("Settings"))
            page = ui.settings_page()
            login = ui.find("Start at Login", {"switch", "check box", "toggle button"}, page)
            if not login.getState().contains(pyatspi.STATE_CHECKED):
                raise AssertionError("Start at Login was not restored after restart")
            ui.switch("Start at Login", False, page)
            ui.wait("remove the autostart entry", lambda: not autostart.exists())
            ui.wait("save Start at Login off", lambda:
                saved_login() == "false")
            for key, title, value, default in (
                ("start-at-login", "Start at Login", True, False),
                ("always-show-tray", "Always Show Tray Icon", True, False),
                ("notify-low-battery", "Low Battery", False, True),
                ("low-battery-percent", "Low Battery Level", 35, 20),
                ("notify-connections", "Connections", True, False),
            ):
                def shows(expected):
                    if isinstance(expected, bool):
                        control = ui.find(title, {"switch", "check box", "toggle button"}, page)
                        return control.getState().contains(pyatspi.STATE_CHECKED) == expected
                    return ui.number(title, root=page) == expected

                host_settings("set", key, str(value).lower())
                ui.wait(f"show external {title} change", lambda: shows(value))
                if key == "start-at-login":
                    ui.wait("write externally enabled autostart", autostart.is_file)
                host_settings("reset", key)
                ui.wait(f"show external {title} reset", lambda: shows(default))
                if host_settings("get", key) != str(default).lower():
                    raise AssertionError(f"The host did not read the reset {title} default")
                if key == "start-at-login":
                    ui.wait("remove externally disabled autostart", lambda: not autostart.exists())
            ui.dump("preferences-external-reset")
            ui.screenshot("preferences-external-reset")
            completed.append("external-preference-updates-and-resets")
            ui.click(node=ui.row("Overview"))
            ui.absent("Start at Login")
            quit()
            completed.extend(("disable-autostart", "quit-presented"))
            log.flush()
            errors = re.findall(
                r"^.*(?:Failed to load icon|Unrecognized image file format|"
                r"error while loading shared libraries|cannot open shared object file|"
                r"undefined symbol:|version [^\n]* not found|"
                r"Failed to load (?:module|shared library)).*$",
                (output / "application.log").read_text(), re.MULTILINE)
            if errors:
                raise AssertionError("AppImage runtime errors:\n" + "\n".join(errors))
            completed.append("runtime-log")
            (output / "result.json").write_text(json.dumps({
                "status": "passed", "renderer": renderer or "default", "completed": completed,
            }, indent=2) + "\n")
        except BaseException:
            (output / "failure.txt").write_text(traceback.format_exc())
            (output / "result.json").write_text(json.dumps({
                "status": "failed", "renderer": renderer or "default", "completed": completed,
            }, indent=2) + "\n")
            if ui:
                try:
                    ui.dump("failure")
                    ui.screenshot("failure")
                except Exception as error:
                    print(f"Cannot capture failure: {error}", file=sys.stderr)
            raise
        finally:
            for process in processes:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    continue
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=10)
            if ui:
                ui.x11.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--renderer", help="Select GSK_RENDERER; otherwise use GTK's default.")
    args = parser.parse_args()
    check(args.image.resolve(), args.output.resolve(), args.renderer)

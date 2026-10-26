#!/usr/bin/env python3
"""Exercise the native desktop through AT-SPI with two framed simulated adapters.

Run CORDIAL_TEST_BINARY=... xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session --
python3 rust/tests/gtk_smoke.py OUTPUT_DIRECTORY.
"""

import argparse
import configparser
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import traceback

from gtk_helpers import BUTTONS, Desktop, text, visible, walk
from gi.repository import Gio, GLib
import pyatspi


APP_ID = "dev.petris.cordial"
ELLIPSIS = "\N{HORIZONTAL ELLIPSIS}"
ADAPTER_WRITE = ("adapter", "SIMULATED0000001")
MOUSE_DETAILS = ("device", "SIMULATED0000001", "1", "details")
MOUSE_PROFILES = ("device", "SIMULATED0000001", "1", "profiles")
MOUSE_SETTINGS = ("settings", "SIMULATED0000001", "1")
CONTROLLER_SETTINGS = ("device", "SIMULATED0000001", "4", "settings")
SETTINGS_WRITE_FAILED = "The adapter couldn't save the change."


def saved_preferences(config):
    saved = configparser.ConfigParser(interpolation=None)
    saved.read(config / "glib-2.0/settings/keyfile")
    group = "dev/petris/cordial"
    if group not in saved:
        return {}
    return {key: int(value) if key == "low-battery-percent" else value == "true"
            for key, value in saved[group].items()}


def require_private_session():
    pid = os.getppid()
    while pid > 1:
        proc = Path("/proc") / str(pid)
        args = proc.joinpath("cmdline").read_bytes().split(b"\0")
        if args and Path(os.fsdecode(args[0])).name == "dbus-run-session":
            break
        pid = int(proc.joinpath("stat").read_text().rsplit(")", 1)[1].split()[1])
    else:
        raise RuntimeError("Use xvfb-run -a dbus-run-session so the fixture has a private session.")
    if not os.environ.get("DISPLAY") or not os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        raise RuntimeError("An X11 display and a private D-Bus session are required.")
    if not shutil.which("import"):
        raise RuntimeError("Install ImageMagick's import command to capture X11 screenshots.")
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    names = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
        "org.freedesktop.DBus", "ListNames", None, GLib.VariantType.new("(as)"),
        Gio.DBusCallFlags.NONE, 3000, None).unpack()[0]
    if APP_ID in names:
        raise RuntimeError("A Cordial instance already owns this session. Use a new dbus-run-session.")
    return bus


class Smoke:
    def __init__(self, binary, root, output, bus):
        self.binary, self.root, self.output, self.bus = binary, root, output, bus
        self.config = root / "config"
        self.autostart = self.config / f"autostart/{APP_ID}.desktop"
        self.log = (output / "application.log").open("w")
        self.processes = []
        self.ui = None
        self.completed = []
        self.stage = "startup"
        self.frame_origins = set()
        self.trace = root / "writes.txt"
        self.trace.touch(mode=0o600)
        self.env = dict(os.environ, CORDIAL_DESKTOP_SIMULATE="2", GDK_BACKEND="x11",
                        CORDIAL_SIM_WRITE_DELAY_MS="0", CORDIAL_SIM_WRITE_TRACE=str(self.trace),
                        CORDIAL_SIM_FAIL_SETTINGS_ONCE="0",
                        GDK_SCALE="1", GDK_DPI_SCALE="1",
                        GTK_A11Y="atspi", GTK_USE_PORTAL="0", GSK_RENDERER="cairo",
                        GIO_USE_VFS="local", GIO_USE_VOLUME_MONITOR="unix",
                        GSETTINGS_BACKEND="keyfile", LC_ALL="C.UTF-8")
        for kind in ("CONFIG", "CACHE", "DATA"):
            directory = root / kind.lower()
            directory.mkdir()
            self.env[f"XDG_{kind}_HOME"] = str(directory)
        # URI activation records destinations inside the fixture instead of opening a browser.
        handler = root / "capture-uri.py"
        self.uri_capture = handler.with_suffix(".json")
        handler.write_text(
            "import json, sys\nfrom pathlib import Path\n"
            "Path(__file__).with_suffix('.json').write_text(json.dumps(sys.argv[1:]) + '\\n')\n")
        applications = root / "data/applications"
        applications.mkdir()
        command = " ".join('"' + str(arg).replace("\\", "\\\\").replace('"', '\\"')
                           .replace("`", "\\`").replace("$", "\\$").replace("%", "%%") + '"'
                           for arg in (sys.executable, handler)) + " %u"
        entry = GLib.KeyFile()
        for key, value in {"Type": "Application", "Name": "Fixture URI Capture",
                           "Exec": command, "MimeType": "x-scheme-handler/https;x-scheme-handler/http;",
                           "NoDisplay": "true"}.items():
            entry.set_string("Desktop Entry", key, value)
        (applications / "fixture-uri-capture.desktop").write_text(entry.to_data()[0])
        (self.config / "mimeapps.list").write_text(
            "[Default Applications]\n"
            "x-scheme-handler/https=fixture-uri-capture.desktop\n"
            "x-scheme-handler/http=fixture-uri-capture.desktop\n")
        activation = {key: value for key, value in self.env.items() if key.startswith("XDG_")
                      or key in {"GSETTINGS_BACKEND", "GIO_USE_VFS", "GIO_USE_VOLUME_MONITOR",
                                 "GTK_A11Y", "GDK_BACKEND", "GDK_SCALE", "GDK_DPI_SCALE", "LC_ALL"}}
        self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
            "org.freedesktop.DBus", "UpdateActivationEnvironment",
            GLib.Variant("(a{ss})", (activation,)), None, Gio.DBusCallFlags.NONE, 3000, None)
        # The chooser's initial download directory stays inside the fixture.
        self.downloads = root / "downloads"
        self.downloads.mkdir()
        (self.config / "user-dirs.dirs").write_text(f'XDG_DOWNLOAD_DIR="{self.downloads}"\n')

    def launch(self, hidden=False):
        process = subprocess.Popen([str(self.binary), "desktop"] + (["--hidden"] if hidden else []),
            cwd=self.root, env=self.env, stdout=self.log, stderr=subprocess.STDOUT)
        self.processes.append(process)
        return process

    def owner(self):
        try:
            return self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                GLib.Variant("(s)", (APP_ID,)), GLib.VariantType.new("(u)"),
                Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
        except GLib.Error:
            return None

    def remote(self, hidden=False):
        remote = self.launch(hidden)
        if remote.wait(timeout=10) != 0:
            raise AssertionError(f"A second desktop launch exited with {remote.returncode}")
        if self.owner() != self.ui.process.pid:
            raise AssertionError("The second desktop launch did not retain the original instance")

    def step(self, name, action):
        self.stage = name
        print(f"RUN {name}", flush=True)
        action()
        self.ui.dump(name)
        self.ui.screenshot(name)
        self.completed.append(name)
        self.report("running")
        print(f"PASS {name}", flush=True)

    def report(self, status, error=None):
        (self.output / "result.json").write_text(json.dumps({
            "status": status, "completed": self.completed, "stage": self.stage, "error": error,
        }, indent=2) + "\n")

    def modal(self, exclude=None):
        def current():
            nodes = self.ui.nodes()
            dialogs = [n for n in nodes if visible(n) and n != exclude
                       and n.getRoleName() in {"dialog", "alert", "file chooser"}]
            for node in nodes:
                if node.getState().contains(pyatspi.STATE_FOCUSED):
                    while node is not None:
                        if node in dialogs:
                            if node.getRoleName() == "file chooser":
                                parent = node.parent
                                while parent is not None:
                                    if parent in dialogs and parent.getRoleName() == "file chooser":
                                        node = parent
                                    parent = parent.parent
                            return node
                        node = node.parent
            return dialogs[-1] if dialogs else None
        return self.ui.wait("show a dialog", current)

    def navigation(self):
        def shown():
            for row in self.ui.matches("Overview", {"list item"}):
                if (row.parent.getRoleName() == "list"
                        and row.getState().contains(pyatspi.STATE_SHOWING)):
                    return row.parent
            return None
        return self.ui.wait("show the navigation list", shown)

    def nav(self, name, occurrence=None):
        navigation = self.navigation()
        if occurrence is None:
            row = self.ui.find(name, {"list item"}, navigation)
        else:
            matches = self.ui.matches(name, {"list item"}, navigation)
            if len(matches) != 2:
                raise AssertionError(f"Expected one {name!r} per simulated adapter")
            row = matches[occurrence]
        self.ui.click(node=row)
        self.ui.wait(f"show the {name!r} navigation selection or collapsed content", lambda:
            self.navigation_content(name, occurrence))
        if name == "Settings":
            self.ui.settings_page()
            self.no_tabs()
        elif name != "Overview":
            self.main_page()
        else:
            self.no_tabs()

    def sidebar_visible(self):
        return any(row.parent.getRoleName() == "list"
                   and row.getState().contains(pyatspi.STATE_SHOWING)
                   for row in self.ui.matches("Overview", {"list item"}))

    def navigation_content(self, name, occurrence=None):
        return self.page_header(name) and (
            self.selected_navigation(name, occurrence) if self.sidebar_visible() else
            len([node for node in self.ui.matches("Back", BUTTONS)
                 if node.getState().contains(pyatspi.STATE_SHOWING)]) == 1)

    def expose_navigation(self, name):
        self.ui.wait(f"show the {name!r} navigation selection or collapsed content", lambda:
            self.navigation_content(name))
        if not self.sidebar_visible():
            self.ui.click("Back")
        self.navigation()
        self.ui.wait(f"retain {name!r} in the exposed sidebar", lambda:
            self.selected_navigation(name))
        self.ui.absent("Back")

    def selected_navigation(self, name, occurrence=None):
        for overview in self.ui.matches("Overview", {"list item"}):
            if (overview.parent.getRoleName() != "list"
                    or not overview.getState().contains(pyatspi.STATE_SHOWING)):
                continue
            rows = self.ui.matches(name, {"list item"}, overview.parent)
            return (len(rows) == (1 if occurrence is None else 2)
                    and rows[0 if occurrence is None else occurrence].getState().contains(
                        pyatspi.STATE_SHOWING)
                    and rows[0 if occurrence is None else occurrence].getState().contains(
                        pyatspi.STATE_SELECTED))
        return False

    def no_tabs(self):
        tabs = [n for n in self.ui.nodes() if visible(n)
                and n.getState().contains(pyatspi.STATE_SHOWING)
                and (n.getRoleName() in {"page tab", "page tab list"}
                     or n.name in {"Details", "Settings", "Profiles", "Diagnostics"}
                     and n.getRoleName() in {"radio button", "toggle button"})]
        if tabs:
            raise AssertionError(f"The main page exposes tab controls: {[n.name for n in tabs]}")

    def main_page(self):
        self.ui.find("Diagnostics", {"list item"})
        self.no_tabs()

    def overview(self):
        self.nav("Overview")
        page = self.page_header("Overview").parent
        while page is not None and not (
                page.getRoleName() in {"grouping", "panel"} and page.name == "Overview"):
            page = page.parent
        if page is None:
            raise AssertionError("Overview has no content page")
        self.ui.absent("Adapters", page)
        rows = [n.name for n in self.ui.nodes(page)
                if visible(n) and n.getRoleName() == "list item"]
        if rows[:2] != ["Example Adapter 1", "Example Adapter 2"]:
            raise AssertionError(f"Overview does not list its adapters first: {rows}")

    def page_header(self, name):
        for node in self.ui.matches(name, {"label"}):
            if not node.getState().contains(pyatspi.STATE_SHOWING):
                continue
            parent = node.parent
            while parent is not None:
                role = parent.getRoleName()
                if role in {"list item", "list"}:
                    break
                if role in {"grouping", "panel"} and parent.name == name:
                    return node
                parent = parent.parent
        return None

    def marker(self, title, state):
        self.ui.find(f"{title}: {state}", BUTTONS, self.ui.row(title))

    def page_end(self):
        for node in self.ui.nodes():
            if visible(node) and node.getRoleName() == "scroll bar":
                scroll = node.queryValue()
                scroll.currentValue = scroll.maximumValue

    def subpage(self, name):
        self.main_page()
        self.page_end()
        self.ui.click(node=self.ui.row(name))
        self.ui.wait(f"open the {name!r} subpage", lambda:
            self.ui.matches(name, {"label"}) and not self.ui.matches("Diagnostics", {"list item"}))
        self.ui.wait("finish the subpage transition", lambda:
            len(self.ui.matches("Back", BUTTONS)) == 1)
        self.ui.absent("Add Device")
        self.no_tabs()

    def back(self):
        self.ui.click("Back")
        self.main_page()
        self.ui.wait("finish returning to the main page", lambda:
            len(self.ui.matches("Back", BUTTONS)) == (0 if any(
                n.getState().contains(pyatspi.STATE_SHOWING)
                for n in self.ui.matches("Overview", {"list item"})) else 1))

    def enabled(self, name, on=True, root=None):
        self.ui.wait(f"{name!r} enabled={on}", lambda:
            self.ui.find(name, BUTTONS, root).getState().contains(pyatspi.STATE_SENSITIVE) == on)

    def checked(self, name, on, root=None):
        self.ui.wait(f"read {name!r} checked={on}", lambda: any(
            n.getState().contains(pyatspi.STATE_CHECKED) == on
            for n in self.ui.matches(name, {"switch", "check box", "toggle button"}, root)))

    def writes(self, target):
        return [tokens[-1] for line in self.trace.read_text(encoding="utf-8").splitlines()
                if (tokens := line.split()) and tuple(tokens[:-1]) == target]

    def wait_write(self, target, before):
        def completed():
            outcomes = self.writes(target)[before:]
            if any(outcome != "ok" for outcome in outcomes):
                raise AssertionError(f"Apply failed for {target}: {outcomes}")
            return bool(outcomes)
        self.ui.wait(f"complete apply for {target}", completed)

    def no_save_discard(self):
        for name in ("Save", "Discard"):
            if self.ui.matches(name, BUTTONS):
                raise AssertionError(f"Immediate settings expose {name}")

    def apply(self, target, action):
        self.no_save_discard()
        before = len(self.writes(target))
        action()
        self.wait_write(target, before)
        self.no_save_discard()

    def quiet(self):
        feedback = {"Save", "Discard", "Saved", "Applied", "Success", "Applying", "Saving",
                    "Applying...", "Saving...", "Applying" + ELLIPSIS,
                    "Saving" + ELLIPSIS, "Settings are being applied"}
        for node in self.ui.nodes():
            if visible(node) and node.getState().contains(pyatspi.STATE_SHOWING):
                if node.getRoleName() in {"animation", "progress bar"} or node.name in feedback:
                    raise AssertionError(f"Settings show apply feedback: {node.getRoleName()} {node.name!r}")
        return True

    def refresh_device(self):
        if not self.ui.matches("Diagnostics", {"list item"}):
            self.back()
        self.subpage("Diagnostics")
        self.device_menu("Refresh")
        self.back()

    def device_status(self, name, status):
        def header():
            title = self.page_header(name)
            if title is None:
                return None
            labels = [node.name for node in self.ui.nodes(title.parent)
                      if visible(node) and node.getState().contains(pyatspi.STATE_SHOWING)
                      and node.getRoleName() == "label"]
            if any("Battery" in label or "%" in label for label in labels):
                raise AssertionError("The device header repeats the battery reading")
            return status in labels
        self.ui.wait(f"show {status!r} in the {name!r} header", header)
        if self.ui.matches("Status", {"list item"}):
            raise AssertionError("The device page repeats its header status in a row")
        for action in ("Refresh", "Connect", "Disconnect"):
            if any(node.getState().contains(pyatspi.STATE_SHOWING)
                   for node in self.ui.matches(action, BUTTONS)):
                raise AssertionError(f"The device page exposes {action} outside Device Menu")

    def device_menu(self, choice):
        self.ui.click("Device Menu")
        menu = self.ui.find("Device Menu", {"menu"})
        connection = "Connect" if choice == "Connect" else "Disconnect"
        self.ui.click(node=self.menu_item(menu, choice, ("Refresh", connection)))
        self.ui.wait("close Device Menu", lambda: not self.ui.matches("Device Menu", {"menu"}))

    def reconnect_adapter(self):
        def ready(status):
            rows = self.ui.matches("Smoke Adapter", {"list item"}, self.navigation())
            return (any(row.description == status
                        and row.getState().contains(pyatspi.STATE_SELECTED) for row in rows)
                and self.ui.matches("Diagnostics", {"list item"}) and any(
                    node.getState().contains(pyatspi.STATE_SHOWING)
                    and node.getState().contains(pyatspi.STATE_SENSITIVE)
                    for node in self.ui.matches("Adapter Menu", BUTTONS)))

        def no_connection_controls():
            self.ui.absent("Status")
            for action in ("Connect", "Disconnect"):
                if any(n.getState().contains(pyatspi.STATE_SHOWING)
                       for n in self.ui.matches(action, BUTTONS)):
                    raise AssertionError(f"The adapter page exposes {action} outside Adapter Menu")

        self.nav("Overview")
        self.no_tabs()
        self.nav("Smoke Adapter")
        self.ui.wait("show the connected adapter's menu", lambda: ready("Ready"))
        no_connection_controls()
        self.adapter_menu("Disconnect")
        self.ui.wait("show the disconnected adapter in navigation", lambda: any(
            row.description == "Disconnected" for row in
            self.ui.matches("Smoke Adapter", {"list item"}, self.navigation())))
        self.nav("Smoke Adapter")
        self.ui.wait("show the disconnected adapter's menu", lambda: ready("Disconnected"))
        no_connection_controls()
        self.adapter_menu("Connect")
        self.ui.wait("show the reconnected adapter in navigation", lambda: any(
            row.description == "Ready" for row in
            self.ui.matches("Smoke Adapter", {"list item"}, self.navigation())))
        self.nav("Smoke Adapter")
        self.ui.wait("show the reconnected adapter's menu", lambda: ready("Ready"))
        no_connection_controls()

    def hidden(self):
        self.ui.wait("hide the main window", lambda: not any(visible(n)
            and n.getState().contains(pyatspi.STATE_SHOWING)
            and n.getRoleName() in {"frame", "window"} for n in self.ui.nodes()))
        if self.ui.process.poll() is not None:
            raise AssertionError("Closing the window quit the application")

    def startup(self):
        self.ui = Desktop(self.launch(hidden=True), self.output)
        self.ui.wait("register the hidden instance", lambda: self.owner() == self.ui.process.pid)
        self.hidden()
        self.remote(hidden=True)
        self.hidden()
        self.remote()
        self.ui.expect("Example Adapter 1")
        self.ui.expect("Example Adapter 2")
        self.ui.expect("Example Mouse")
        self.ui.expect("Example Trackball")
        self.sidebar()

    def sidebar(self):
        navigation = self.navigation()
        rows = [n for n in self.ui.nodes(navigation) if visible(n)
                and n.getRoleName() == "list item"]
        expected = ["Overview", "Settings"]
        for adapter, mouse in (("Example Adapter 1", "Example Mouse"),
                               ("Example Adapter 2", "Example Trackball")):
            expected.extend((adapter, "Example Controller", "Example Keys Wireless",
                             mouse, "Travel Keyboard"))
        if [n.name for n in rows] != expected:
            raise AssertionError(f"Unexpected sidebar group order: {[n.name for n in rows]}")
        for title in ("Devices", "Adapters", "Add Device"):
            if self.ui.matches(title, root=navigation):
                raise AssertionError(f"The populated sidebar exposes {title!r}")
        separators = [n for n in self.ui.nodes(navigation) if visible(n)
                      and n.getRoleName() == "separator"]
        if len(separators) != 2:
            raise AssertionError("The sidebar must separate its two adapter groups")
        for name in ("Example Mouse", "Example Trackball"):
            row = self.ui.row(name, navigation)
            self.ui.find("Battery 72%", {"image"}, row)
        if any(n.getRoleName() == "label" and "%" in n.name
               for n in self.ui.nodes(navigation) if visible(n)):
            raise AssertionError("The sidebar shows battery percentage text")

    def about(self):
        self.nav("Overview")
        self.ui.click("Main Menu")
        menu = self.ui.find("Main Menu", {"menu"})
        self.ui.click(node=self.menu_item(menu, "About Cordial",
            ("Refresh Adapters", "About Cordial", "Quit")))
        dialog = self.modal()
        self.ui.expect("Cordial", dialog)
        self.ui.key("Control_L", "f")
        self.ui.retain("keep About active without opening Search", lambda:
            visible(dialog) and self.modal() == dialog
            and not self.ui.matches("Search", {"text", "entry"}))

        def links(node):
            try:
                content = node.queryHypertext()
                uris = set()
                for i in range(content.getNLinks()):
                    link = content.getLink(i)
                    if hasattr(link, "queryHyperlink"):
                        link = link.queryHyperlink()
                    uris.add(link.getURI(0))
                return uris
            except (NotImplementedError, GLib.Error):
                return set()

        def activated_uri(activate):
            self.uri_capture.write_text("")
            activate()

            def captured():
                value = self.uri_capture.read_text()
                return json.loads(value) if value.endswith("\n") else None

            arguments = self.ui.wait("capture the activated hyperlink destination", captured)
            if len(arguments) != 1:
                raise AssertionError(f"Unexpected URI handler arguments: {arguments!r}")
            return arguments[0]

        website = "https://github.com/ryanpetris/cordial"
        website_link = self.ui.matches("Website", {"link"}, dialog)
        if website_link:
            destination = self.ui.copy_link(self.ui.find("Website", {"link"}, dialog))
        else:
            destination = activated_uri(lambda: self.ui.click(node=self.ui.row("Website", dialog)))
        if destination != website:
            raise AssertionError(f"Unexpected Website hyperlink destination: {destination!r}")
        self.ui.dump("about-main")
        self.ui.screenshot("about-main")
        self.ui.click(node=self.ui.row("Legal", dialog))
        legal_text = ("\N{COPYRIGHT SIGN} 2026 Ryan Petris\n\n"
                      "This application comes with absolutely no warranty. "
                      "See the MIT License for details.")
        legal = self.ui.wait("show the copyright and MIT License in the Legal text", lambda:
            next((node for node in self.ui.nodes(dialog)
                  if visible(node) and node.getRoleName() == "label"
                  and text(node) == legal_text), None))
        license_uri = website + "/blob/master/LICENSE"
        if "Hypertext" in legal.get_interfaces():
            self.ui.wait("expose the MIT License hyperlink destination", lambda:
                license_uri in links(legal))
        else:
            def activate_license():
                self.ui.focus(legal)
                self.ui.key("Tab")
                self.ui.key("Return")
            destination = activated_uri(activate_license)
            if destination != license_uri:
                raise AssertionError(f"Unexpected MIT License hyperlink destination: {destination!r}")
        self.ui.dump("about-legal")
        self.ui.screenshot("about-legal")
        self.ui.click("Close", root=dialog)
        self.ui.wait("close About Cordial", lambda:
            not any(visible(n) and n.getRoleName() in {"dialog", "alert"}
                    for n in self.ui.nodes()))
        self.ui.wait("restore Overview after About Cordial", lambda: self.page_header("Overview"))

    def adapters(self):
        self.nav("Example Adapter 2")
        self.ui.expect("Platform")
        self.ui.expect("Bluetooth LE")
        self.nav("Example Adapter 1")
        self.adapter_menu("Rename" + ELLIPSIS)
        dialog = self.modal()
        self.ui.edit("Name", "Smoke Adapter", dialog)
        self.enabled("Rename", root=dialog)
        self.ui.click("Rename", root=dialog)
        self.ui.absent("Rename Adapter")
        self.ui.expect("Smoke Adapter")
        self.nav("Example Adapter 2")
        self.nav("Smoke Adapter")
        self.apply(ADAPTER_WRITE, lambda: self.ui.choose("Platform", "Windows"))
        self.reconnect_adapter()
        self.ui.expect("Windows", self.ui.row("Platform"))
        self.apply(ADAPTER_WRITE, lambda: self.ui.choose("Platform", "Linux"))
        for transport in ("Bluetooth Classic", "Bluetooth LE"):
            self.apply(ADAPTER_WRITE, lambda: self.ui.switch(transport, False))
            self.checked(transport, False)
            self.reconnect_adapter()
            self.checked(transport, False)
            self.apply(ADAPTER_WRITE, lambda: self.ui.switch(transport, True))
            self.checked(transport, True)
            self.reconnect_adapter()
            self.checked(transport, True)
        self.ui.expect("Linux", self.ui.row("Platform"))

    def details(self):
        self.nav("Example Mouse")
        self.device_status("Example Mouse", "Connected")
        self.device_menu("Disconnect")
        self.device_status("Example Mouse", "Disconnected")
        self.device_menu("Connect")
        self.device_status("Example Mouse", "Connected")
        for title, initial in (("Use This Device", True), ("Automatic Connections", True),
                               ("Logitech Features", True), ("Block Connections", False)):
            self.checked(title, initial)
            self.apply(MOUSE_DETAILS, lambda: self.ui.switch(title, not initial))
            self.checked(title, not initial)
            self.nav("Example Trackball")
            self.nav("Example Mouse")
            self.refresh_device()
            self.checked(title, not initial)
            self.apply(MOUSE_DETAILS, lambda: self.ui.switch(title, initial))
            self.checked(title, initial)
            self.refresh_device()
            self.checked(title, initial)

    def search_flow(self):
        self.nav("Example Mouse")
        writes = self.trace.read_text()
        preferences = saved_preferences(self.config)
        autostart = self.autostart.exists()

        def unchanged():
            return (self.trace.read_text() == writes
                    and saved_preferences(self.config) == preferences
                    and self.autostart.exists() == autostart)

        def open_search(query, icon=False):
            if icon:
                toggle = self.ui.find("Search", BUTTONS)
                menu = self.ui.find("Main Menu", BUTTONS)
                _, _, (sx, sy, sw, sh) = self.node_bounds(toggle)
                _, _, (mx, my, _, mh) = self.node_bounds(menu)
                if sx + sw > mx or sy + sh <= my or my + mh <= sy:
                    raise AssertionError("Search is not at the start of the sidebar header")
                self.ui.click(node=toggle)
            else:
                self.ui.key("Control_L", "f")
            entry = self.ui.control("Search", "queryEditableText")
            self.ui.wait("focus the search entry", lambda:
                entry.getState().contains(pyatspi.STATE_FOCUSED))
            self.ui.edit("Search", query)
            return entry

        def closed():
            return not any(n.getState().contains(pyatspi.STATE_SHOWING)
                for n in self.ui.matches("Search", {"text", "entry"})
                + self.ui.matches("Search Results", {"list"}))

        def choose(query, title, context, page, control=None, subpage=None,
                   icon=False, keyboard=False, width=980, height=680, occurrence=None,
                   group=None):
            open_search(query, icon)
            results = self.ui.find("Search Results", {"list"})
            hit = self.ui.wait(f"find search result {title!r} in {context!r}", lambda:
                next((n for n in self.ui.matches(title, {"list item"}, results)
                      if n.description == context), None))
            self.fit_node(hit, width, height)
            if any(visible(n) and n.getRoleName() in {"dialog", "alert"}
                   for n in self.ui.nodes()):
                raise AssertionError("Search opened a modal dialog")
            if icon:
                self.ui.dump("search-results")
                self.ui.screenshot("search-results")
            if keyboard:
                if keyboard == "row":
                    self.ui.key("Down")
                    self.ui.wait("focus the first search result", lambda:
                        hit.getState().contains(pyatspi.STATE_FOCUSED))
                self.ui.key("Return")
            else:
                self.ui.click(node=hit)
            self.ui.wait("close search after choosing a result", closed)
            self.ui.wait(f"open the searched {page!r} page", lambda:
                self.page_header(subpage or page))
            if width > 660:
                self.ui.wait(f"select the searched {page!r} sidebar row", lambda:
                    self.selected_navigation(page, occurrence))
            if page == "Settings":
                self.ui.settings_page()
            if control:
                root = self.ui.row(group).parent if group else None
                row = (self.ui.find(control, {"list item"})
                       if subpage == "Profiles" and control not in {"VIA", "Vial"}
                       else self.ui.row(control, root))
                self.ui.wait(f"focus the searched {control!r} control", lambda: any(
                    n.getState().contains(pyatspi.STATE_FOCUSED) for n in walk(row)))
                if control in {"VIA", "Vial"}:
                    if row.getRoleName() != "combo box":
                        raise AssertionError(f"The searched {control!r} row is not a combo box")
                    self.ui.wait(f"focus the {control!r} profile selector without opening it", lambda:
                        row.getState().contains(pyatspi.STATE_FOCUSED)
                        and not row.getState().contains(pyatspi.STATE_EXPANDED))
                self.fit_node(row, width, height)
                pane = row.parent
                while pane is not None and pane.getRoleName() != "scroll pane":
                    pane = pane.parent
                if pane is None:
                    raise AssertionError(f"The searched {control!r} row has no scroll pane")
                def in_view():
                    px, py, pw, ph = pane.queryComponent().getExtents(pyatspi.XY_WINDOW)
                    x, y, w, h = row.queryComponent().getExtents(pyatspi.XY_WINDOW)
                    return (px <= x and py <= y and x + w <= px + pw
                            and y + h <= py + ph)
                self.ui.wait(f"scroll the searched {control!r} control into view", in_view)
            self.ui.retain("search without writing settings", unchanged)

        for replacement in ("", "cordial-no-matching-setting"):
            entry = open_search("settings")
            results = self.ui.find("Search Results", {"list"})
            self.ui.find("Settings", {"list item"}, results)
            if not entry.queryEditableText().setTextContents(replacement):
                raise AssertionError("Cannot replace the search query")
            self.ui.key("Return")
            self.ui.retain("keep the current page after replacing a matching query", lambda:
                self.page_header("Example Mouse")
                and unchanged())
            self.ui.key("Escape")
            self.ui.wait("close search after replacing a matching query", closed)
            self.ui.wait("retain the selected device after replacing a matching query", lambda:
                self.selected_navigation("Example Mouse"))

        entry = open_search("settings")
        results = self.ui.find("Search Results", {"list"})
        self.ui.find("Settings", {"list item"}, results)
        if not entry.queryEditableText().setTextContents("pointer sensitivity trackball"):
            raise AssertionError("Cannot replace the search query")
        self.ui.key("Return")
        self.ui.wait("close search after immediately activating a replacement query", closed)
        self.ui.wait("open the replacement query's Example Trackball page", lambda:
            self.page_header("Example Trackball"))
        self.ui.wait("select Example Trackball after replacing the query", lambda:
            self.selected_navigation("Example Trackball"))
        row = self.ui.row("Pointer Sensitivity")
        self.ui.wait("focus Pointer Sensitivity after replacing the query", lambda: any(
            n.getState().contains(pyatspi.STATE_FOCUSED) for n in walk(row)))
        self.ui.retain("replace and activate a search query without writing settings", unchanged)

        choose("CONNECTIONS settings", "Connections", "Settings", "Settings",
               control="Connections", icon=True)
        for title in ("Start at Login", "Always Show Tray Icon", "Low Battery"):
            choose(title, title, "Settings", "Settings", control=title)
        choose("battery level", "Low Battery Level", "Settings", "Settings",
               control="Low Battery Level")
        choose("platform smoke", "Platform", "Smoke Adapter", "Smoke Adapter",
               control="Platform")
        choose("bluetooth le adapter 2", "Bluetooth LE", "Example Adapter 2",
               "Example Adapter 2", control="Bluetooth LE")
        for title in ("VIA", "Vial"):
            choose(title + " smoke", title, "Smoke Adapter", "Smoke Adapter",
                   control=title, subpage="Profiles")
        for query, adapter in (("typing smoke", "Smoke Adapter"),
                               ("typing adapter 2", "Example Adapter 2")):
            choose(query, "Typing", adapter, adapter, control="Typing",
                   subpage="Profiles", keyboard="entry")
        choose("example profile 11 smoke", "Example Profile 11", "Smoke Adapter",
               "Smoke Adapter", control="Example Profile 11", subpage="Profiles")
        choose("typing keys adapter 2", "Typing",
               "Example Keys Wireless \N{MIDDLE DOT} Example Adapter 2", "Example Keys Wireless",
               control="Typing", subpage="Profiles", occurrence=0)
        choose("reverse vertical mouse", "Reverse Vertical Scrolling", "Example Mouse",
               "Example Mouse", control="Reverse Vertical Scrolling")
        choose("block connections mouse", "Block Connections", "Example Mouse",
               "Example Mouse", control="Block Connections")
        choose("pointer sensitivity trackball", "Pointer Sensitivity", "Example Trackball",
               "Example Trackball", control="Pointer Sensitivity")
        choose("battery mouse", "Battery", "Example Mouse", "Example Mouse", control="Battery")
        choose("ratchets per rotation mouse", "Ratchets per Rotation", "Example Mouse",
               "Example Mouse", control="Ratchets per Rotation")
        choose("firmware smoke", "Firmware", "Smoke Adapter", "Smoke Adapter", control="Firmware")
        for transport in ("Bluetooth Classic", "Bluetooth LE"):
            choose("active devices smoke", "Active Devices",
                   f"Smoke Adapter \N{MIDDLE DOT} {transport}", "Smoke Adapter",
                   control="Active Devices", group=transport)
        choose("device id mouse", "Device ID", "Example Mouse", "Example Mouse",
               control="Device ID", subpage="Diagnostics")
        choose("identifiers mouse", "Identifiers", "Example Mouse", "Example Mouse",
               control="Device ID", subpage="Diagnostics")
        self.ui.dump("search-information")
        self.ui.screenshot("search-information")
        choose("overview", "Overview", "", "Overview", keyboard="row")
        choose("settings", "Settings", "", "Settings", keyboard="entry")
        choose("example adapter 2", "Example Adapter 2", "", "Example Adapter 2")
        choose("trackball", "Example Trackball", "Example Adapter 2", "Example Trackball")
        choose("profiles mouse", "Profiles", "Example Mouse", "Example Mouse",
               subpage="Profiles")
        choose("diagnostics smoke", "Diagnostics", "Smoke Adapter", "Smoke Adapter",
               subpage="Diagnostics")
        open_search("cordial-no-matching-setting")
        self.ui.expect("No Results Found")
        self.ui.expect("Try a different search")
        self.ui.screenshot("search-no-results")
        self.ui.key("Escape")
        self.ui.wait("restore navigation after Escape", closed)
        self.ui.wait("retain Diagnostics after cancelling search", lambda:
            self.page_header("Diagnostics"))
        self.ui.wait("retain the selected adapter after Escape", lambda:
            self.selected_navigation("Smoke Adapter"))
        self.ui.retain("cancel search without writing settings", unchanged)
        self.nav("Example Mouse")
        self.ui.resize(480, 720)
        choose("reverse vertical mouse", "Reverse Vertical Scrolling", "Example Mouse",
               "Example Mouse", control="Reverse Vertical Scrolling", width=480, height=720)
        self.ui.dump("search-control-480")
        self.ui.screenshot("search-control-480")
        self.ui.click("Back")
        self.ui.wait("retain the searched mouse on returning to the narrow sidebar", lambda:
            self.selected_navigation("Example Mouse"))
        choose("example profile 11 adapter 2", "Example Profile 11", "Example Adapter 2",
               "Example Adapter 2", control="Example Profile 11", subpage="Profiles",
               width=480, height=720)
        self.ui.dump("search-profile-480")
        self.ui.screenshot("search-profile-480")
        self.back()
        self.ui.click("Back")
        self.ui.wait("retain the searched adapter on returning to the narrow sidebar", lambda:
            self.selected_navigation("Example Adapter 2"))
        entry = open_search("cordial-no-matching-setting")
        self.ui.expect("No Results Found")
        self.ui.expect("Try a different search")
        self.fit_node(entry, 480, 720)
        self.ui.screenshot("search-no-results-480")
        self.ui.key("Escape")
        self.ui.wait("restore the narrow sidebar after Escape", closed)
        self.ui.wait("retain the searched adapter after Escape in the narrow sidebar", lambda:
            self.selected_navigation("Example Adapter 2"))
        self.ui.retain("search at 480px without writing settings", unchanged)
        self.ui.resize(980, 680)
        self.nav("Example Mouse")

    def settings(self):
        self.nav("Example Mouse")
        self.ui.wait("show the connected device's sidebar status", lambda:
            self.ui.row("Example Mouse", self.navigation()).description == "Connected")
        self.device_status("Example Mouse", "Connected")
        self.ui.expect("72%", self.ui.row("Battery"))
        self.fit_capacity("Battery", 980, 680)
        self.ui.expect("Pointer Sensitivity")
        self.ui.expect("Resolution Multiplier")
        self.ui.expect("Ratchets per Rotation")
        self.main_page()
        self.marker("Pointer Sensitivity", "Saved")
        self.marker("Pointer Sensitivity 2", "Not Saved")
        self.ui.scroll_into_view(self.ui.row("Pointer Sensitivity"))
        baseline = self.ui.number("Pointer Sensitivity")
        before = len(self.writes(MOUSE_SETTINGS))
        entry = self.ui.control("Pointer Sensitivity", "queryEditableText")
        (self.output / "numeric-baseline.json").write_text(json.dumps({
            "value": baseline, "text": text(entry),
        }, indent=2) + "\n")
        self.ui.focus(entry)
        self.ui.key("Control_L", "a")
        for digit in "12":
            self.ui.key(digit)
        self.ui.wait("receive the partial numeric entry", lambda: text(entry) == "12")
        self.ui.retain("retain a paused numeric edit", lambda:
            entry.getState().contains(pyatspi.STATE_FOCUSED) and text(entry) == "12"
            and self.ui.number("Pointer Sensitivity") == baseline
            and len(self.writes(MOUSE_SETTINGS)) == before)
        for digit in "00":
            self.ui.key(digit)
        self.ui.wait("receive the complete numeric entry", lambda: text(entry) == "1200")
        # Discovery redraws the model without committing the focused numeric edit.
        self.ui.key("F5")
        self.ui.retain("retain numeric focus after refresh", lambda:
            entry.getState().contains(pyatspi.STATE_FOCUSED) and text(entry) == "1200"
            and self.ui.number("Pointer Sensitivity") == baseline
            and len(self.writes(MOUSE_SETTINGS)) == before)
        self.apply(MOUSE_SETTINGS, lambda: self.ui.key("Tab"))
        self.device_menu("Refresh")
        self.ui.wait("read saved pointer sensitivity", lambda:
            self.ui.number("Pointer Sensitivity") == 1200)
        self.marker("Pointer Sensitivity", "Saved")
        self.ui.focus(entry)
        self.ui.key("Control_L", "a")
        for digit in "1250":
            self.ui.key(digit)
        self.apply(MOUSE_SETTINGS, lambda: self.ui.key("Return"))
        self.ui.wait("commit pointer sensitivity with Enter", lambda:
            self.ui.number("Pointer Sensitivity") == 1250)
        self.apply(MOUSE_SETTINGS, lambda: self.ui.key("Down"))
        self.ui.wait("commit a pointer sensitivity step", lambda:
            self.ui.number("Pointer Sensitivity") == 1200)
        self.device_menu("Refresh")
        self.apply(MOUSE_SETTINGS, lambda: self.ui.choose("Pointer Sensitivity 2", "1600"))
        self.apply(MOUSE_SETTINGS, lambda: self.ui.choose("Wheel Mode", "Free Spin"))
        self.apply(MOUSE_SETTINGS, lambda: self.ui.switch("Reverse Vertical Scrolling", True))
        self.nav("Example Trackball")
        self.nav("Example Mouse")
        self.device_menu("Refresh")
        self.ui.wait("read saved pointer sensitivity", lambda:
            self.ui.number("Pointer Sensitivity") == 1200)
        self.nav("Example Trackball")
        self.nav("Example Mouse")
        self.main_page()
        self.ui.wait("retain saved numeric setting", lambda:
            self.ui.number("Pointer Sensitivity") == 1200)
        self.ui.expect("Free Spin", self.ui.row("Wheel Mode"))
        self.ui.expect("1600", self.ui.row("Pointer Sensitivity 2"))
        self.marker("Pointer Sensitivity 2", "Saved")
        self.checked("Reverse Vertical Scrolling", True)
        self.ui.screenshot("device-settings")
        self.nav("Example Controller", occurrence=1)
        self.ui.expect("Smoke Adapter", self.ui.row("Adapter"))
        self.ui.expect("XInput", self.ui.row("Gamepad Output"))
        for mode in ("HID", "Disabled", "XInput"):
            self.apply(CONTROLLER_SETTINGS, lambda: self.ui.choose("Gamepad Output", mode))
            self.nav("Example Mouse")
            self.nav("Example Controller", occurrence=1)
            self.refresh_device()
            self.ui.expect(mode, self.ui.row("Gamepad Output"))

    def diagnostics(self):
        self.nav("Example Mouse")
        self.subpage("Diagnostics")
        self.ui.expect("Logitech Features")
        self.ui.expect("Device ID")
        self.ui.expect("Identifiers")
        self.ui.screenshot("device-diagnostics")
        self.back()
        for name in ("Identifiers", "Device ID", "HID++ Protocol"):
            if self.ui.matches(name):
                raise AssertionError(f"Device diagnostics leaked onto Details: {name}")

    def profile_page(self, name):
        row = self.ui.find(name, {"list item"})
        self.ui.scroll_into_view(row)
        return row

    def all_profiles(self, root=None):
        for name in ("Next", "Previous"):
            if self.ui.matches(name, BUTTONS, root):
                raise AssertionError(f"The profile list exposes {name}")
        for name in ("Natural Scrolling", "Typing", *(
                f"Example Profile {i}" for i in range(3, 13))):
            self.ui.find(name, {"list item"}, root)

    def profile_menu(self, name, choice):
        self.ui.click(name, root=self.profile_page(name))
        menu = self.ui.find(name, {"menu"})
        self.ui.click(node=self.menu_item(menu, choice + ELLIPSIS,
            ("Copy" + ELLIPSIS, "Delete" + ELLIPSIS)))
        self.ui.expect(choice, self.modal())

    def menu_item(self, menu, choice, choices):
        named = self.ui.matches(choice, {"menu item"}, menu)
        if named:
            return self.ui.find(choice, {"menu item"}, menu)
        # Some GTK releases expose model menu items without their text labels.
        items = [n for n in self.ui.nodes(menu) if n.getRoleName() == "menu item"]
        if len(items) != len(choices):
            raise AssertionError(f"Expected menu items {choices!r}, got {len(items)}")
        return items[choices.index(choice)]

    def adapter_menu(self, choice):
        self.ui.click("Adapter Menu")
        menu = self.ui.find("Adapter Menu", {"menu"})
        connection = "Connect" if choice == "Connect" else "Disconnect"
        choices = (("Rename" + ELLIPSIS, connection)
                   if choice in {"Rename" + ELLIPSIS, "Connect", "Disconnect"} else
                   ("Restart", "Files" + ELLIPSIS, "Enter Bootloader" + ELLIPSIS))
        self.ui.click(node=self.menu_item(menu, choice, choices))
        self.ui.wait("close Adapter Menu", lambda: not self.ui.matches("Adapter Menu", {"menu"}))

    def layer_order(self, names):
        def ordered():
            if len(self.ui.matches("Remove", BUTTONS)) != len(names):
                return None
            rows = [self.ui.row(name) for name in names]
            if len(set(rows)) != len(names) or len({row.parent for row in rows}) != 1:
                raise AssertionError("Profile layers do not occupy distinct rows in one list")
            positions = [row.getIndexInParent() for row in rows]
            if positions != sorted(positions):
                return None
            for name, row in zip(names, rows):
                if not row.getState().contains(pyatspi.STATE_SENSITIVE):
                    raise AssertionError(f"The {name!r} profile row is disabled")
                self.enabled("Remove", root=row)
                if self.ui.matches(name, BUTTONS, row):
                    raise AssertionError(f"The {name!r} profile row exposes a menu button")
                for action in ("Move Up", "Move Down"):
                    if self.ui.matches(action, BUTTONS, row):
                        raise AssertionError(f"A profile row exposes a {action} button")
            return list(names)
        return self.ui.wait(f"show profile layers in order {names!r}", ordered)

    def profiles(self):
        self.nav("Smoke Adapter")
        self.subpage("Profiles")
        self.ui.expect("Configuration Interfaces")
        self.ui.expect("Typing")
        self.all_profiles()
        self.profile_page("Example Profile 11")
        self.profile_page("Example Profile 12")
        self.ui.click("New Profile")
        dialog = self.modal()
        self.ui.edit("Name", "Smoke Profile", dialog)
        self.enabled("Create", root=dialog)
        self.ui.click("Create", root=dialog)
        self.ui.wait("close New Profile", lambda: not self.ui.matches("New Profile", {"dialog", "alert"}))
        self.profile_menu("Smoke Profile", "Copy")
        dialog = self.modal()
        self.ui.edit("Name", "Smoke Copy", dialog)
        self.enabled("Copy", root=dialog)
        self.ui.click("Copy", root=dialog)
        self.ui.absent("Copy", root=dialog)
        self.profile_menu("Smoke Copy", "Delete")
        self.ui.click("Delete", root=self.modal())
        self.ui.absent("Smoke Copy")
        self.apply(ADAPTER_WRITE, lambda: self.ui.choose("VIA", "Example Profile 11"))
        self.reconnect_adapter()
        self.subpage("Profiles")
        self.ui.expect("Example Profile 11", self.ui.row("VIA"))
        self.apply(ADAPTER_WRITE, lambda: self.ui.choose("VIA", "Natural Scrolling"))
        self.reconnect_adapter()
        self.subpage("Profiles")
        self.ui.expect("Natural Scrolling", self.ui.row("VIA"))
        self.apply(ADAPTER_WRITE, lambda: self.ui.switch("VIA", True))
        self.checked("VIA", True)
        self.reconnect_adapter()
        self.subpage("Profiles")
        self.checked("VIA", True)
        self.ui.expect("Natural Scrolling", self.ui.row("VIA"))
        self.apply(ADAPTER_WRITE, lambda: self.ui.switch("VIA", False))
        self.checked("VIA", False)
        self.reconnect_adapter()
        self.subpage("Profiles")
        self.checked("VIA", False)
        self.apply(ADAPTER_WRITE, lambda: self.ui.choose("Vial", "Example Profile 11"))
        self.reconnect_adapter()
        self.subpage("Profiles")
        self.ui.expect("Example Profile 11", self.ui.row("Vial"))
        self.ui.screenshot("adapter-profiles")
        self.nav("Example Mouse")
        self.subpage("Profiles")
        self.ui.click("Add Profile")
        picker = self.modal()
        self.all_profiles(picker)
        self.ui.click(node=self.ui.row("Example Profile 11", picker))
        self.apply(MOUSE_PROFILES, lambda: self.ui.click("Choose", root=picker))
        self.refresh_device()
        self.subpage("Profiles")
        self.layer_order(("Natural Scrolling", "Example Profile 11"))
        self.apply(MOUSE_PROFILES, lambda:
            self.ui.click("Remove", root=self.ui.row("Example Profile 11")))
        self.layer_order(("Natural Scrolling",))
        self.ui.click("Add Profile")
        picker = self.modal()
        self.ui.click(node=self.ui.row("Typing", picker))
        self.apply(MOUSE_PROFILES, lambda: self.ui.click("Choose", root=picker))
        self.ui.expect("Typing")
        self.ui.click("Add Profile")
        picker = self.modal()
        self.ui.click(node=self.ui.row("Example Profile 3", picker))
        self.apply(MOUSE_PROFILES, lambda: self.ui.click("Choose", root=picker))
        self.refresh_device()
        self.subpage("Profiles")
        self.layer_order(("Natural Scrolling", "Typing", "Example Profile 3"))
        before = len(self.writes(MOUSE_PROFILES))
        pointer = self.ui.drag(self.ui.row("Natural Scrolling"), self.ui.row("Example Profile 3"),
                               capture="device-profiles-drag-pointer")
        self.wait_write(MOUSE_PROFILES, before)
        self.layer_order(("Typing", "Example Profile 3", "Natural Scrolling"))
        self.ui.dump("device-profiles-drag")
        self.ui.screenshot("device-profiles-drag")
        self.refresh_device()
        self.subpage("Profiles")
        refreshed = self.layer_order(("Typing", "Example Profile 3", "Natural Scrolling"))
        self.reconnect_adapter()
        self.nav("Example Mouse")
        self.subpage("Profiles")
        reconnected = self.layer_order(("Typing", "Example Profile 3", "Natural Scrolling"))
        outcomes = self.writes(MOUSE_PROFILES)[before:]
        if outcomes != ["ok"]:
            raise AssertionError(f"One pointer drop must write the final order once: {outcomes}")
        self.ui.dump("device-profiles-drag-readback")
        self.ui.screenshot("device-profiles-drag-readback")
        (self.output / "device-profiles-drag.json").write_text(json.dumps({
            "initial_order": ["Natural Scrolling", "Typing", "Example Profile 3"],
            "dragged_profile": "Natural Scrolling", "target_profile": "Example Profile 3",
            "pointer": pointer, "completed_writes": outcomes,
            "refreshed_order": refreshed, "reconnected_order": reconnected,
        }, indent=2) + "\n")
        self.apply(MOUSE_PROFILES, lambda:
            self.ui.drag(self.ui.row("Typing"), self.ui.row("Example Profile 3")))
        self.refresh_device()
        self.subpage("Profiles")
        self.layer_order(("Example Profile 3", "Typing", "Natural Scrolling"))
        self.apply(MOUSE_PROFILES, lambda:
            self.ui.click("Remove", root=self.ui.row("Example Profile 3")))
        self.layer_order(("Typing", "Natural Scrolling"))
        self.apply(MOUSE_PROFILES, lambda: self.ui.click("Remove", root=self.ui.row("Typing")))
        self.refresh_device()
        self.subpage("Profiles")
        self.ui.absent("Typing")
        self.ui.expect("Natural Scrolling")
        self.ui.click("Add Profile")
        picker = self.modal()
        self.ui.click(node=self.ui.row("Typing", picker))
        self.apply(MOUSE_PROFILES, lambda: self.ui.click("Choose", root=picker))
        self.refresh_device()
        self.subpage("Profiles")
        self.ui.expect("Typing")

    def start_pair(self, candidate, context, narrow=False):
        self.nav(context)
        if narrow:
            self.ui.resize(480, 720)
            self.fit_header(context, 480, 720)
        add = self.ui.find("Add Device", BUTTONS)
        header = self.ui.wait(f"show the {context!r} header", lambda: self.page_header(context))
        ax, ay, aw, ah = add.queryComponent().getExtents(pyatspi.XY_WINDOW)
        hx, hy, _, hh = header.queryComponent().getExtents(pyatspi.XY_WINDOW)
        if ax + aw > hx or ay + ah <= hy or hy + hh <= ay:
            raise AssertionError("Add Device is not at the start of the content header")
        self.ui.click(node=add)
        dialog = self.modal()
        self.ui.expect(candidate, dialog)
        for node in self.ui.nodes(dialog):
            if visible(node) and node.getState().contains(pyatspi.STATE_SHOWING):
                if node.getRoleName() == "combo box" or node.name == "Adapter":
                    raise AssertionError("Add Device exposes an adapter selector")
                if node.getRoleName() == "label" and node.name.startswith("Searching"):
                    raise AssertionError("Add Device shows searching text")
        spinner = self.ui.wait("show the scan indicator", lambda: next((
            node for node in self.ui.nodes(dialog) if visible(node)
            and node.getRoleName() in {"animation", "progress bar"} and node.name == "Searching"
            and node.getState().contains(pyatspi.STATE_SHOWING)), None))
        title = self.ui.find("Add Device", {"label"}, dialog)
        sx, sy, sw, sh = spinner.queryComponent().getExtents(pyatspi.XY_WINDOW)
        tx, ty, _, th = title.queryComponent().getExtents(pyatspi.XY_WINDOW)
        if sx + sw > tx or sy + sh <= ty or ty + th <= sy:
            raise AssertionError("The scan indicator is not in the dialog header")
        if spinner.getState().contains(pyatspi.STATE_FOCUSABLE):
            raise AssertionError("The scan indicator is interactive")
        for name in ("Refresh", "Retry"):
            if any(node.getState().contains(pyatspi.STATE_SHOWING)
                   for node in self.ui.matches(name, BUTTONS, dialog)):
                raise AssertionError(f"Add Device shows {name} while scanning")
        self.ui.dump(f"pairing-scan-{context.replace(' ', '-').lower()}")
        self.ui.screenshot(f"pairing-scan-{context.replace(' ', '-').lower()}")
        self.ui.click("Pair", root=self.ui.row(candidate, dialog))
        return dialog

    def empty_profiles(self):
        self.nav("Example Mouse")
        self.subpage("Profiles")
        self.layer_order(("Natural Scrolling",))
        self.apply(MOUSE_PROFILES, lambda:
            self.ui.click("Remove", root=self.ui.row("Natural Scrolling")))
        self.refresh_device()
        self.subpage("Profiles")
        for width, height in ((980, 680), (480, 720)):
            self.ui.resize(width, height)
            self.fit_header("Profiles", width, height)

            def layout():
                header = self.page_header("Profiles")
                if header is None:
                    return None
                page = header.parent
                while page is not None and not (
                        page.name == "Profiles" and page.getRoleName() in {"grouping", "panel"}):
                    page = page.parent
                if page is None:
                    raise AssertionError("The empty state has no Profiles content page")
                titles = [node for node in self.ui.matches("No Profiles", {"label"}, page)
                          if node.getState().contains(pyatspi.STATE_SHOWING)]
                adds = [node for node in self.ui.matches("Add Profile" + ELLIPSIS, BUTTONS, page)
                        if node.getState().contains(pyatspi.STATE_SHOWING)]
                if len(titles) != 1 or len(adds) != 1:
                    return None
                title, add = titles[0], adds[0]
                shown_adds = [node for node in self.ui.nodes() if visible(node)
                              and node.name in {"Add Profile", "Add Profile" + ELLIPSIS}
                              and node.getRoleName() in BUTTONS
                              and node.getState().contains(pyatspi.STATE_SHOWING)]
                if shown_adds != [add]:
                    return None
                nodes = {"title": title, "add": add}
                bounds = {name: self.node_bounds(node, name == "add")[2]
                          for name, node in nodes.items()}
                px, py, pw, ph = self.node_bounds(page)[2]
                tx, ty, tw, th = bounds["title"]
                ax, ay, aw, ah = bounds["add"]
                center = px + pw / 2
                if (min(pw, ph) <= 0 or ty + th > ay
                        or any(min(w, h) <= 0 or x < px or y < py
                               or x + w > px + pw or y + h > py + ph
                               for x, y, w, h in bounds.values())
                        or any(abs(x + w / 2 - center) > 8
                               for x, _, w, _ in bounds.values())
                        or abs((ty + ay + ah) / 2 - (py + ph / 2)) > ph * 0.2):
                    return None
                if not add.getState().contains(pyatspi.STATE_SENSITIVE):
                    return None
                for name, node in nodes.items():
                    self.fit_node(node, width, height, button=name == "add")
                return {"page": (px, py, pw, ph), **bounds}

            geometry = self.ui.wait("center the empty Profiles title and Add Profile", layout)
            self.ui.absent("Remove")
            self.ui.dump(f"empty-profiles-{width}")
            self.ui.screenshot(f"empty-profiles-{width}")
            geometry["decorative_icon_check"] = (
                f"Visual review of empty-profiles-{width}.png. GTK presents the StatusPage icon "
                "as decoration without an AT-SPI image node.")
            (self.output / f"empty-profiles-{width}-bounds.json").write_text(
                json.dumps(geometry, indent=2) + "\n")
        self.ui.click("Add Profile" + ELLIPSIS)
        picker = self.modal()
        self.ui.click(node=self.ui.row("Typing", picker))
        self.apply(MOUSE_PROFILES, lambda: self.ui.click("Choose", root=picker))
        self.refresh_device()
        self.subpage("Profiles")
        self.layer_order(("Typing",))
        self.ui.absent("No Profiles")
        self.ui.resize(980, 680)
        self.ui.wait("show the expanded sidebar", self.sidebar_visible)

    def sidebar_groups(self, groups):
        expected = ["Overview", "Settings"]
        for adapter, devices in groups.items():
            expected.extend((adapter, *(devices or ["Add Device"])))

        def grouped():
            navigation = self.navigation()
            rows = [node for node in self.ui.nodes(navigation)
                    if visible(node) and node.getRoleName() == "list item"]
            if [node.name for node in rows] != expected:
                return None
            result = {}
            offset = 2
            for adapter, devices in groups.items():
                count = len(devices) or 1
                result[adapter] = rows[offset + 1:offset + 1 + count]
                if not devices and result[adapter][0].description != adapter:
                    return None
                offset += count + 1
            return result

        return self.ui.wait(f"show adapter groups {expected!r}", grouped)

    def empty_sidebar(self):
        groups = {adapter: ["Example Controller", "Example Keys Wireless", mouse, "Travel Keyboard"]
                  for adapter, mouse in (("Example Adapter 1", "Example Mouse"),
                                         ("Example Adapter 2", "Example Trackball"))}

        def forget_group(adapter):
            for name in list(groups[adapter]):
                rows = self.sidebar_groups(groups)[adapter]
                self.ui.click(node=next(row for row in rows if row.name == name))
                self.ui.wait(f"show {name!r} before forgetting it", lambda: self.page_header(name))
                self.ui.expect(adapter, self.ui.row("Adapter"))
                self.page_end()
                self.ui.click("Forget Device" + ELLIPSIS)
                self.ui.click("Forget", root=self.modal())
                groups[adapter].remove(name)
                self.sidebar_groups(groups)

        def pair_from_sidebar(adapter):
            rows = self.sidebar_groups(groups)[adapter]
            if len(rows) != 1 or rows[0].name != "Add Device":
                raise AssertionError(f"The empty {adapter!r} group has no Add Device row")
            self.ui.click(node=rows[0])
            dialog = self.modal()
            for node in self.ui.nodes(dialog):
                if visible(node) and (node.getRoleName() == "combo box" or node.name == "Adapter"):
                    raise AssertionError("Sidebar Add Device exposes an adapter selector")
            self.ui.click("Pair", root=self.ui.row("Example Numpad", dialog))
            self.pair_success(dialog, "Example Numpad",
                              f"empty-sidebar-pair-{adapter[-1]}", automatic=True)
            self.ui.expect(adapter, self.ui.row("Adapter"))
            groups[adapter].append("Example Numpad")
            self.sidebar_groups(groups)

        forget_group("Example Adapter 1")
        self.ui.dump("empty-sidebar-one-adapter")
        self.ui.screenshot("empty-sidebar-one-adapter")
        pair_from_sidebar("Example Adapter 1")
        forget_group("Example Adapter 1")
        forget_group("Example Adapter 2")
        self.ui.dump("empty-sidebar-both-adapters")
        self.ui.screenshot("empty-sidebar-both-adapters")
        pair_from_sidebar("Example Adapter 2")

    def run_empty_states(self):
        self.startup()
        self.step("empty-profiles", self.empty_profiles)
        self.step("empty-sidebar", self.empty_sidebar)
        self.quit()

    def run_pairing_reconnect(self):
        self.pairing_trace = self.root / "pairing-reconnect.txt"
        self.pairing_trace.touch(mode=0o600)
        self.env.update(CORDIAL_SIM_PAIRING_RECONNECT_MS="1500",
                        CORDIAL_SIM_PAIRING_RECONNECT_TRACE=str(self.pairing_trace))
        self.startup()
        self.step("pairing-reconnect-automatic", lambda: self.pairing_reconnect(automatic=True))
        self.step("pairing-reconnect-dismiss", lambda: self.pairing_reconnect(automatic=False))
        self.quit()

    def pairing_reconnect(self, automatic):
        adapter = "SIMULATED0000001"
        candidate, device = ("Example Keys Mini", "11") if automatic else ("Example Pebble", "12")

        def records():
            return [line.split() for line in self.pairing_trace.read_text().splitlines()]

        session = [row[2] for row in records() if row[:2] == ["opened", adapter]][-1]
        event = lambda stage: [stage, adapter, session, device] in records()
        dialog = self.start_pair(candidate, "Example Adapter 1")
        prompt = "Enter Passkey" if automatic else "Compare Codes"
        self.ui.expect(prompt, dialog)
        self.ui.retain("keep the prompt open without a success countdown", lambda:
            bool(self.ui.matches(prompt, {"label"}, dialog))
            and not self.ui.matches("Dismiss", BUTTONS, dialog) and not event("done"), duration=3.3)
        if automatic:
            self.ui.edit("Code", "123456", dialog)
        self.ui.click("Pair" if automatic else "Codes Match", root=dialog)
        message = f"{candidate} is paired and connected."
        success = []

        def retained():
            nodes = self.ui.nodes(self.navigation())
            names = [node.name for node in nodes if visible(node)
                     and node.getRoleName() == "list item"]
            connected = bool(self.ui.matches(message, {"label"}, dialog))
            if connected and not success:
                success.append(time.monotonic())
                if not automatic:
                    self.ui.key("Tab")
            return (all(names.count(name) == 1 for name in (
                "Example Adapter 1", "Example Adapter 2", "Example Mouse", "Example Trackball",
                candidate)) and bool(self.ui.matches("Add Device", {"dialog", "alert"}))
                and (connected or bool(self.ui.matches("Connecting", {"label"}, dialog))))

        self.ui.wait("close the framed session after PairingDone and SessionEnding", lambda:
            event("closed"))
        trace = records()
        if trace.index(["done", adapter, session, device]) >= trace.index(
                ["closed", adapter, session, device]):
            raise AssertionError("The simulated transport closed before PairingDone")
        self.ui.wait("show the retained dialog, adapter and new device while the port is absent",
                     lambda: retained() and not event("present"))
        self.ui.retain("retain the dialog, adapter and new device during the absent port", lambda:
            retained() and not event("present"), duration=0.4)
        self.ui.dump(self.stage + "-gap")
        self.ui.screenshot(self.stage + "-gap")

        self.ui.wait("confirm the paired device is connected", lambda:
            retained() and self.ui.matches(message, {"label"}, dialog))
        observed = success[0]
        self.ui.retain("retain success across the replacement session", lambda:
            retained() and bool(self.ui.matches(message, {"label"}, dialog)),
                       duration=max(0.1, (2.6 if automatic else 3.3)
                                    - (time.monotonic() - observed)))
        if not automatic:
            self.enabled("Add Another", root=dialog)
            self.ui.click("Dismiss", root=dialog)
        self.ui.wait("dismiss the confirmed pairing result", lambda:
            not self.ui.matches("Add Device", {"dialog", "alert"}), timeout=5)
        elapsed = time.monotonic() - observed
        replacement = self.ui.wait("open a replacement session for the same adapter", lambda:
            next((row[2] for row in records() if row[:2] == ["opened", adapter]
                  and int(row[2]) > int(session)), None))
        if not event("present"):
            raise AssertionError("The replacement opened before the simulated port returned")
        self.ui.wait(f"open the reconnected {candidate!r} page", lambda: self.page_header(candidate))
        self.ui.expect("Example Adapter 1", self.ui.row("Adapter"))
        (self.output / (self.stage + "-dismissal.json")).write_text(json.dumps({
            "automatic": automatic, "seconds_after_observation": elapsed,
            "original_session": session, "replacement_session": replacement,
        }, indent=2) + "\n")

    def forget(self, name, adapter):
        self.ui.wait(f"show the paired {name!r} page", lambda: self.page_header(name))
        row = self.ui.row("Adapter")
        self.ui.scroll_into_view(row)
        self.ui.expect(adapter, row)
        self.ui.dump(f"paired-{name.replace(' ', '-').lower()}")
        self.ui.screenshot(f"paired-{name.replace(' ', '-').lower()}")
        forget = self.ui.find("Forget Device" + ELLIPSIS, BUTTONS)
        self.page_end()
        self.ui.wait("show Forget Device in the page", lambda:
            forget.getState().contains(pyatspi.STATE_SHOWING))
        self.ui.click(node=forget)
        self.ui.click("Forget", root=self.modal())
        self.ui.wait(f"forget {name!r}", lambda: bool(self.ui.matches("Overview", {"list item"}))
            and not self.ui.matches(name, {"list item"}, self.navigation()))

    def pair_layout(self, dialog, name, actions):
        if dialog.name != "Add Device":
            raise AssertionError(f"Unexpected pairing dialog: {dialog.name!r}")
        if any(node.getState().contains(pyatspi.STATE_SHOWING)
               for node in self.ui.matches("Add Device", {"label"}, dialog)):
            raise AssertionError("The pairing step shows the scan header title")
        action_root = self.ui.find(actions[-1], BUTTONS, dialog).parent
        buttons = [self.ui.find(action, BUTTONS, action_root) for action in actions]
        shown = [node.name for node in self.ui.nodes(action_root)
                 if visible(node) and node.getState().contains(pyatspi.STATE_SHOWING)
                 and node.getRoleName() in BUTTONS]
        if shown != list(actions):
            raise AssertionError(f"Expected pairing answers {actions!r}, got {shown!r}")

        def settled():
            overlay = tuple(dialog.queryComponent().getExtents(pyatspi.XY_WINDOW))
            card = action_root
            while card.parent is not None and card.parent != dialog:
                parent_bounds = tuple(card.parent.queryComponent().getExtents(pyatspi.XY_WINDOW))
                if parent_bounds == overlay:
                    break
                card = card.parent
            dx, dy, dw, dh = card.queryComponent().getExtents(pyatspi.XY_WINDOW)
            bounds = [self.ui.button_bounds(button) for button in buttons]
            if not 0 < dw <= 440 or dh <= 0:
                return None
            for button, (x, y, width, height) in zip(buttons, bounds):
                if (not button.getState().contains(pyatspi.STATE_SHOWING)
                        or min(width, height) <= 0 or x < dx or y < dy
                        or x + width > dx + dw or y + height > dy + dh):
                    return None
            frame = dialog.parent
            while frame is not None and frame.getRoleName() not in {"frame", "window"}:
                frame = frame.parent
            if frame is None:
                raise AssertionError("The pairing dialog has no application window")
            fx, fy, fw, fh = frame.queryComponent().getExtents(pyatspi.XY_WINDOW)
            if dx < fx or dy < fy or dx + dw > fx + fw or dy + dh > fy + fh:
                return None
            # The dialog animates from the scan size. Wait for its answers to fit before
            # measuring the remaining space or recording evidence.
            if len(bounds) > 2:
                if any(a[1] + a[3] > b[1] for a, b in zip(bounds, bounds[1:])):
                    return None
            elif len(bounds) == 2:
                if (abs(bounds[0][1] - bounds[1][1]) > 4
                        or bounds[0][0] + bounds[0][2] > bounds[1][0]):
                    return None
            intervals = []
            for node in self.ui.nodes(dialog):
                if (visible(node) and node.getState().contains(pyatspi.STATE_SHOWING)
                        and node.getRoleName() in {"label", "text", "entry", "button", "image"}):
                    x, y, width, height = node.queryComponent().getExtents(pyatspi.XY_WINDOW)
                    if width > 0 and height > 0:
                        if (x < dx or y < dy or x + width > dx + dw
                                or y + height > dy + dh):
                            return None
                        intervals.append((max(dy, y), min(dy + dh, y + height)))
            occupied, end = 0, dy
            for top, bottom in sorted(intervals):
                occupied += max(0, bottom - max(end, top))
                end = max(end, bottom)
            if dh - occupied > max(180, occupied):
                return None
            return {"dialog": [dx, dy, dw, dh], "buttons": dict(zip(actions, bounds)),
                    "unused_vertical_space": dh - occupied}

        geometry = self.ui.wait("fit the compact pairing content and all answers", settled)
        (self.output / f"{name}-bounds.json").write_text(json.dumps(geometry, indent=2) + "\n")
        self.ui.dump(name)
        self.ui.screenshot(name)

    def pair_success(self, dialog, candidate, name, automatic=False):
        self.ui.find("Dismiss", BUTTONS, dialog)
        observed = time.monotonic()
        message = f"{candidate} is paired and connected."
        self.ui.expect(message, dialog)
        self.ui.find("Add Another", BUTTONS, dialog)
        shown = lambda: bool(self.ui.matches(message, {"label"}, dialog)
                             and self.ui.matches("Dismiss", BUTTONS, dialog))
        if automatic:
            self.ui.retain("keep pairing success visible before automatic dismissal", shown,
                           duration=1.5)
        else:
            self.ui.key("Tab")
            self.ui.retain("pause pairing success during keyboard interaction", shown,
                           duration=3.3)
            self.pair_layout(dialog, name, ("Add Another", "Dismiss"))
            self.ui.click("Dismiss", root=dialog)
        self.ui.wait("close the successful pairing dialog", lambda:
            not self.ui.matches("Add Device", {"dialog", "alert"}), timeout=5)
        self.ui.wait(f"open the paired {candidate!r} page", lambda: self.page_header(candidate))
        (self.output / f"{name}-dismissal.json").write_text(json.dumps({
            "automatic": automatic, "seconds_after_observation": time.monotonic() - observed,
        }, indent=2) + "\n")

    def pairing(self):
        self.nav("Overview")
        self.ui.absent("Add Device")
        dialog = self.start_pair("Example Keys Mini", "Example Adapter 2")
        self.ui.expect("Enter Passkey", dialog)
        self.ui.edit("Code", "123456", dialog)
        self.pair_layout(dialog, "pairing-enter-code", ("Reject", "Pair"))
        self.enabled("Pair", root=dialog)
        self.ui.click("Pair", root=dialog)
        self.pair_success(dialog, "Example Keys Mini", "pairing-enter-code-result")
        self.forget("Example Keys Mini", "Example Adapter 2")
        dialog = self.start_pair("Example Pebble", "Example Mouse")
        self.ui.expect("Compare Codes", dialog)
        self.ui.expect("1 2 3 4 5 6", dialog)
        self.pair_layout(dialog, "pairing-confirm-code", ("Codes Differ", "Codes Match"))
        self.ui.click("Codes Differ", root=dialog)
        self.ui.expect("Pairing Failed", dialog)
        self.ui.expect("Cordial couldn't pair with Example Pebble.", dialog)
        self.ui.retain("keep a pairing failure open", lambda:
            bool(self.ui.matches("Pairing Failed", {"label"}, dialog)
                 and self.ui.matches("Retry", BUTTONS, dialog)), duration=3.3)
        self.pair_layout(dialog, "pairing-failed", ("Close", "Retry"))
        self.ui.click("Retry", root=dialog)
        self.ui.click("Pair", root=self.ui.row("Example Pebble", dialog))
        self.ui.expect("Compare Codes", dialog)
        self.ui.expect("1 2 3 4 5 6", dialog)
        self.ui.click("Codes Match", root=dialog)
        self.pair_success(dialog, "Example Pebble", "pairing-confirm-code-result")
        self.forget("Example Pebble", "Smoke Adapter")
        dialog = self.start_pair("Example Display Keyboard", "Example Trackball", narrow=True)
        self.ui.expect("Type This Code", dialog)
        self.ui.expect("1 2 3 4 5 6", dialog)
        self.pair_layout(dialog, "pairing-show-code", ("Cancel Pairing",))
        self.ui.click("Cancel Pairing", root=dialog)
        self.ui.expect("Pairing Cancelled", dialog)
        self.ui.retain("keep cancelled pairing open", lambda:
            bool(self.ui.matches("Pairing Cancelled", {"label"}, dialog)
                 and self.ui.matches("Refresh", BUTTONS, dialog)), duration=3.3)
        self.pair_layout(dialog, "pairing-cancelled", ("Close", "Refresh"))
        self.ui.key("Escape")
        self.ui.wait("close the pairing dialog", lambda:
            not self.ui.matches("Add Device", {"dialog", "alert"}))
        self.ui.resize(980, 680)
        self.ui.wait("show the expanded sidebar", lambda: any(
            n.getState().contains(pyatspi.STATE_SHOWING)
            for n in self.ui.matches("Overview", {"list item"})))
        dialog = self.start_pair("Example Numpad", "Smoke Adapter")
        self.pair_success(dialog, "Example Numpad", "pairing-automatic-result", automatic=True)
        self.forget("Example Numpad", "Smoke Adapter")

    def download_dialog(self, target):
        files = self.modal()
        self.ui.click("Download" + ELLIPSIS, root=files)
        # GTK may use the session's native file chooser portal.
        def portal():
            try:
                pid = self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                    GLib.Variant("(s)", ("org.freedesktop.impl.portal.desktop.gtk",)),
                    GLib.VariantType.new("(u)"), Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
                self.ui.external_pids.add(pid)
            except GLib.Error:
                pass
            return any(visible(n) and n != files and n.getRoleName() in {
                "dialog", "file chooser"} for n in self.ui.nodes())
        self.ui.wait("open the native file chooser", portal)
        chooser = self.modal(exclude=files)
        self.ui.wait("show the file name field", lambda:
            self.ui.matches("Name:", root=chooser) or self.ui.matches("Name", root=chooser))
        label = "Name:" if self.ui.matches("Name:", root=chooser) else "Name"
        self.ui.edit(label, str(target), chooser)
        self.ui.click("Save", root=chooser)
        self.ui.wait("confirm replacing the existing file", lambda: self.ui.matches("Replace", BUTTONS))
        confirmation = self.ui.find("Replace", BUTTONS)
        while confirmation is not None and confirmation.getRoleName() not in {
                "dialog", "alert", "file chooser"}:
            confirmation = confirmation.parent
        if confirmation is None:
            raise AssertionError("The Replace button has no accessible confirmation")
        return chooser, confirmation

    def files(self):
        self.nav("Smoke Adapter")
        self.subpage("Diagnostics")
        self.adapter_menu("Files" + ELLIPSIS)
        self.ui.click(node=self.ui.row("examples/", self.modal()))
        self.ui.focus(self.ui.row("sample.txt", self.modal()))
        self.ui.key("Return")
        target = self.downloads / "sample.txt"
        target.write_bytes(b"Keep this file.\n")
        chooser, confirmation = self.download_dialog(target)
        self.ui.click("Cancel", root=confirmation)
        self.ui.absent("Replace")
        if chooser in self.ui.nodes():
            self.ui.click("Cancel", root=chooser)
        self.ui.absent("Name:")
        if target.read_bytes() != b"Keep this file.\n":
            raise AssertionError("Cancelling Replace changed the existing download")
        _, confirmation = self.download_dialog(target)
        self.ui.click("Replace", root=confirmation)
        self.ui.wait("download the complete sample", lambda:
            target.read_bytes() == b"Example device\n")
        self.ui.expect("Saved", self.modal())
        self.ui.screenshot("files-saved")
        self.ui.key("Escape")
        self.ui.absent("Download" + ELLIPSIS)
        self.back()

    def preferences_page(self, sidebar=False):
        if sidebar:
            self.nav("Settings")
        else:
            self.ui.key("Control_L", "comma")
        page = self.ui.settings_page()
        self.ui.wait("show the Settings content title", lambda: self.page_header("Settings"))
        return page

    def preferences_flow(self):
        pages = []
        for context in ("Smoke Adapter", "Example Mouse"):
            self.nav(context)
            pages.append(self.preferences_page(sidebar=True))
            self.ui.key("Control_L", "n")
            self.ui.retain("keep Settings without an adapter pairing target", lambda:
                not self.ui.matches("Add Device", {"dialog"})
                and self.page_header("Settings") is not None)
        if pages[0] != pages[1]:
            raise AssertionError("Settings did not reuse its content pane")
        self.nav("Example Mouse")
        self.ui.click("Add Device")
        self.ui.expect("Example Keys Mini", self.modal())
        self.ui.click("Cancel", root=self.modal())
        self.ui.wait("close Add Device", lambda: not self.ui.matches("Add Device", {"dialog"}))
        page = self.preferences_page()
        if page != pages[0]:
            raise AssertionError("Ctrl+, did not reuse the sidebar's Settings pane")
        for title in ("Start at Login", "Always Show Tray Icon", "Connections"):
            self.ui.switch(title, True, page)
        self.ui.switch("Low Battery", False, page)
        self.ui.number("Low Battery Level", 35, page)
        expected = {"start-at-login": True, "always-show-tray": True,
                    "notify-low-battery": False, "low-battery-percent": 35,
                    "notify-connections": True}
        self.ui.wait("save isolated GSettings preferences", lambda:
            saved_preferences(self.config) == expected)
        self.ui.wait("write isolated autostart entry", self.autostart.exists)
        entry = configparser.ConfigParser(interpolation=None)
        entry.read(self.autostart)
        desktop = entry["Desktop Entry"]
        args = shlex.split(desktop["Exec"])
        if [args[0].replace("%%", "%"), *args[1:]] != [str(self.binary), "desktop", "--hidden"]:
            raise AssertionError(f"Unexpected autostart command: {desktop['Exec']!r}")
        if desktop["Type"] != "Application" or desktop["Terminal"] != "false":
            raise AssertionError("The autostart entry is not a background desktop application")
        self.ui.screenshot("preferences-page")
        self.nav("Overview")
        self.ui.absent("Start at Login")

    def narrow(self):
        self.nav("Example Mouse")
        minimum_width, minimum_height = self.ui.x11.minimum_size(self.ui.process.pid)
        if minimum_width != 480:
            raise AssertionError(f"Expected a 480px minimum width, got {minimum_width}")
        height = max(720, minimum_height)
        tools = ("Restart", "Files" + ELLIPSIS, "Enter Bootloader" + ELLIPSIS)
        for width in (480, 661, 700):
            self.ui.resize(width, height)
            self.ui.wait("show the sidebar or collapsed mouse content", lambda:
                self.sidebar_visible() or self.navigation_content("Example Mouse"))
            if self.sidebar_visible():
                self.nav("Example Mouse")
            self.main_page()
            self.fit_header("Example Mouse", width, height)
            self.fit_controls(width, height, ("Add Device",))
            self.device_status("Example Mouse", "Connected")
            self.fit_device_menu(width, height, "Disconnect")
            self.fit_capacity("Battery", width, height)
            self.device_menu("Disconnect")
            self.device_status("Example Mouse", "Disconnected")
            self.fit_device_menu(width, height, "Connect")
            self.device_menu("Connect")
            self.device_status("Example Mouse", "Connected")
            for title in ("Use This Device", "Automatic Connections", "Logitech Features",
                          "Block Connections", "Pointer Sensitivity", "Pointer Sensitivity 2",
                          "Wheel Mode", "Reverse Vertical Scrolling"):
                self.fit_row(title, width, height)
            self.fit_marker("Pointer Sensitivity", "Saved", width, height)
            self.fit_marker("Pointer Sensitivity 2", "Saved", width, height)
            self.ui.screenshot(f"device-settings-{width}")
            self.page_end()
            self.fit_controls(width, height, ("Forget Device" + ELLIPSIS,))
            self.ui.screenshot(f"device-details-{width}")
            self.subpage("Profiles")
            self.fit_header("Profiles", width, height)
            if any(node.getState().contains(pyatspi.STATE_SHOWING)
                   for node in self.ui.matches("Device Menu", BUTTONS)):
                raise AssertionError("The device Profiles page exposes Device Menu")
            self.ui.expect("Typing")
            self.fit_controls(width, height, ("Add Profile",))
            self.layer_order(("Natural Scrolling", "Typing"))
            for title in ("Natural Scrolling", "Typing"):
                row = self.fit_row(title, width, height)
                self.fit_controls(width, height, ("Remove",), row)
            self.ui.screenshot(f"device-profiles-{width}")
            self.ui.click("Add Profile")
            picker = self.modal()
            self.all_profiles(picker)
            row = self.ui.find("Example Profile 11", {"list item"}, picker)
            self.ui.click(node=row)
            self.fit_node(row, width, height)
            self.fit_controls(width, height, ("Choose",), picker)
            self.ui.screenshot(f"profile-picker-{width}")
            self.ui.key("Escape")
            self.ui.wait("close the profile picker", lambda:
                not self.ui.matches("Choose", BUTTONS))
            self.back()
            self.subpage("Diagnostics")
            self.fit_header("Diagnostics", width, height)
            self.fit_device_menu(width, height, "Disconnect", page="diagnostics")
            self.device_menu("Refresh")
            self.ui.screenshot(f"device-diagnostics-{width}")
            self.back()
            self.expose_navigation("Example Mouse")
            self.nav("Smoke Adapter")
            self.fit_header("Smoke Adapter", width, height)
            self.fit_controls(width, height, ("Add Device", "Adapter Menu"))
            self.ui.absent("Status")
            self.ui.click("Adapter Menu")
            menu = self.ui.find("Adapter Menu", {"menu"})
            choices = ("Rename" + ELLIPSIS, "Disconnect")
            for choice in choices:
                self.fit_node(self.menu_item(menu, choice, choices), width, height)
            self.ui.key("Escape")
            self.ui.absent("Adapter Menu", root=menu)
            self.fit_row("Platform", width, height)
            for transport in ("Bluetooth Classic", "Bluetooth LE"):
                toggle = self.fit_row(transport, width, height)
                group = toggle.parent
                self.fit_capacity("Active Devices", width, height, group)
                if self.ui.matches("Connections", root=group):
                    self.fit_row("Connections", width, height, group)
            for title in ("Storage", "Interfaces"):
                if self.ui.matches(title, {"list item"}):
                    self.fit_row(title, width, height)
            self.ui.absent("New Pairings")
            self.ui.absent("Available")
            self.ui.screenshot(f"adapter-details-{width}")
            self.subpage("Profiles")
            self.fit_header("Profiles", width, height)
            self.all_profiles()
            for title in ("Example Profile 11", "Example Profile 12"):
                self.fit_node(self.profile_page(title), width, height)
            self.fit_controls(width, height, ("New Profile",))
            for title in ("VIA", "Vial"):
                row = self.fit_row(title, width, height)
                self.fit_node(self.ui.find(title, {"switch", "check box", "toggle button"}, row), width, height)
            self.ui.screenshot(f"adapter-profiles-{width}")
            self.back()
            self.subpage("Diagnostics")
            self.fit_header("Diagnostics", width, height)
            self.fit_controls(width, height, ("Adapter Menu",))
            self.ui.click("Adapter Menu")
            menu = self.ui.find("Adapter Menu", {"menu"})
            for name in tools:
                self.fit_node(self.menu_item(menu, name, tools), width, height)
            self.ui.screenshot(f"adapter-diagnostics-{width}")
            self.ui.key("Escape")
            self.ui.absent("Adapter Menu", root=menu)
            self.back()
            self.expose_navigation("Smoke Adapter")
            page = self.preferences_page(sidebar=True)
            self.fit_header("Settings", width, height)
            for title in ("Start at Login", "Always Show Tray Icon", "Low Battery",
                          "Low Battery Level", "Connections"):
                if title == "Low Battery Level":
                    control = self.ui.control(title, "queryValue", page)
                else:
                    control = self.ui.find(title, {"switch", "check box", "toggle button"}, page)
                self.ui.scroll_into_view(control)
                self.fit_node(control, width, height)
            self.ui.screenshot(f"preferences-{width}")
            self.expose_navigation("Settings")
        self.ui.resize(980, 680)

    def node_bounds(self, node, button=False):
        frame = node.parent
        while frame is not None and frame.getRoleName() not in {"frame", "window"}:
            frame = frame.parent
        if frame is None:
            raise AssertionError(f"{node.name!r} has no accessible window")
        frame_bounds = tuple(frame.queryComponent().getExtents(pyatspi.XY_WINDOW))
        raw = tuple(self.ui.button_bounds(node) if button else
                    node.queryComponent().getExtents(pyatspi.XY_WINDOW))
        x, y, width, height = raw
        relative = (x - frame_bounds[0], y - frame_bounds[1], width, height)
        origin = (self.stage, frame_bounds[:2])
        if origin not in self.frame_origins:
            self.frame_origins.add(origin)
            print(f"GEOMETRY {self.stage}: {node.name!r} raw={raw} "
                  f"frame={frame_bounds} relative={relative}", flush=True)
        return raw, frame_bounds, relative

    def fit_header(self, name, window_width, window_height):
        def header():
            node = self.page_header(name)
            if node is None:
                return None
            _, _, (x, y, width, height) = self.node_bounds(node)
            if (min(width, height) > 0 and min(x, y) >= 0
                    and x + width <= window_width and y + height <= window_height):
                return node
            return None
        self.ui.wait(f"show the {name!r} header inside the {window_width}px window", header)

    def fit_node(self, node, window_width, window_height, button=False):
        def bounds():
            return self.node_bounds(node, button)
        def fits():
            _, _, (x, y, width, height) = bounds()
            return (node.getState().contains(pyatspi.STATE_SHOWING)
                    and min(width, height) > 0 and min(x, y) >= 0
                    and x + width <= window_width and y + height <= window_height)
        try:
            self.ui.wait(f"fit {node.name!r} inside the {window_width}px window", fits)
        except AssertionError as error:
            raw, frame, relative = bounds()
            raise AssertionError(
                f"{node.name} is outside the {window_width}px window: "
                f"raw={raw}, frame={frame}, relative={relative}") from error

    def fit_row(self, title, window_width, window_height, root=None):
        row = self.ui.row(title, root)
        self.ui.scroll_into_view(row)
        self.fit_node(row, window_width, window_height)
        roles = BUTTONS | {"switch", "check box", "spin button", "combo box", "level bar"}
        for node in walk(row):
            if visible(node) and node.getRoleName() in roles:
                self.fit_node(node, window_width, window_height)
        return row

    def fit_marker(self, title, state, window_width, window_height):
        row = self.fit_row(title, window_width, window_height)
        marker = self.ui.find(f"{title}: {state}", BUTTONS, row)
        self.fit_node(marker, window_width, window_height)
        label = self.ui.find(title, {"label"}, row)
        mx, _, mw, _ = marker.queryComponent().getExtents(pyatspi.XY_WINDOW)
        lx, _, _, _ = label.queryComponent().getExtents(pyatspi.XY_WINDOW)
        if mx + mw > lx:
            raise AssertionError(f"The {title!r} state marker is not before its title")

    def fit_capacity(self, title, window_width, window_height, root=None):
        row = self.fit_row(title, window_width, window_height, root)
        bar = self.ui.find(title, {"level bar"}, row)
        label = self.ui.find(title, {"label"}, row)
        line = label.parent
        bx, by, bw, _ = bar.queryComponent().getExtents(pyatspi.XY_WINDOW)
        lx, ly, lw, lh = line.queryComponent().getExtents(pyatspi.XY_WINDOW)
        if bx != lx or bw != lw or by < ly + lh:
            raise AssertionError(f"The {title!r} capacity bar does not span the row below its text")

    def fit_controls(self, window_width, window_height, names, root=None):
        for name in names:
            button = self.ui.find(name, BUTTONS, root)
            self.ui.scroll_into_view(button)
            self.fit_node(button, window_width, window_height, button=True)

    def fit_device_menu(self, width, height, connection, page="details"):
        self.fit_controls(width, height, ("Device Menu",))
        self.ui.click("Device Menu")
        menu = self.ui.find("Device Menu", {"menu"})
        choices = ("Refresh", connection)
        for choice in choices:
            self.fit_node(self.menu_item(menu, choice, choices), width, height)
        self.ui.screenshot(f"device-{page}-menu-{connection.lower()}-{width}")
        self.ui.key("Escape")
        self.ui.wait("close Device Menu", lambda: not self.ui.matches("Device Menu", {"menu"}))

    def close_relaunch(self):
        self.ui.key("Control_L", "w")
        self.hidden()
        self.remote(hidden=True)
        self.hidden()
        self.remote()
        self.ui.expect("Smoke Adapter")
        self.nav("Example Mouse")
        self.ui.wait("retain settings while hidden", lambda:
            self.ui.number("Pointer Sensitivity") == 1200)

    def quit(self):
        self.ui.key("Control_L", "q")
        self.wait_quit()

    def wait_quit(self):
        self.ui.wait("quit cleanly", lambda: self.ui.process.poll() is not None, running=False)
        if self.ui.process.returncode != 0:
            raise AssertionError(f"Quit exited with {self.ui.process.returncode}")
        self.ui.wait("release the application name", lambda: self.owner() is None, running=False)

    def persistence(self):
        self.quit()
        self.ui.x11.close()
        self.ui = Desktop(self.launch(), self.output)
        self.ui.expect("Example Adapter 1")
        page = self.preferences_page(sidebar=True)
        for title in ("Start at Login", "Always Show Tray Icon", "Connections"):
            node = self.ui.find(title, {"switch", "check box", "toggle button"}, page)
            if not node.getState().contains(pyatspi.STATE_CHECKED):
                raise AssertionError(f"{title} was not restored")
        node = self.ui.find("Low Battery", {"switch", "check box", "toggle button"}, page)
        if node.getState().contains(pyatspi.STATE_CHECKED):
            raise AssertionError("Low Battery was not restored")
        if self.ui.number("Low Battery Level", root=page) != 35:
            raise AssertionError("Low Battery Level was not restored")
        self.ui.switch("Start at Login", False, page)
        self.ui.wait("remove isolated autostart entry", lambda: not self.autostart.exists())
        self.ui.wait("save Start at Login off", lambda:
            saved_preferences(self.config).get("start-at-login") is False)
        self.nav("Overview")
        self.ui.absent("Start at Login")

    def delayed_settings(self):
        self.quit()
        self.ui.x11.close()
        self.env["CORDIAL_SIM_WRITE_DELAY_MS"] = "800"
        self.ui = Desktop(self.launch(), self.output)
        self.ui.expect("Example Adapter 1")
        self.ui.expect("Example Trackball")
        self.nav("Example Mouse")
        self.ui.expect("Pointer Sensitivity")
        self.ui.scroll_into_view(self.ui.row("Pointer Sensitivity"))
        self.main_page()
        self.quiet()

    def quiet_profiles(self):
        self.delayed_settings()
        self.subpage("Profiles")
        self.ui.click("Add Profile")
        picker = self.modal()
        self.ui.click(node=self.ui.row("Typing", picker))
        self.apply(MOUSE_PROFILES, lambda: self.ui.click("Choose", root=picker))
        self.layer_order(("Natural Scrolling", "Typing"))
        self.quiet()
        before = len(self.writes(MOUSE_PROFILES))
        first = self.ui.drag(self.ui.row("Typing"), self.ui.row("Natural Scrolling"))
        self.layer_order(("Typing", "Natural Scrolling"))
        if len(self.writes(MOUSE_PROFILES)) != before:
            raise AssertionError("The delayed profile write completed before the next drag")
        second = self.ui.drag(self.ui.row("Typing"), self.ui.row("Natural Scrolling"))
        self.layer_order(("Natural Scrolling", "Typing"))
        self.quiet()
        self.ui.screenshot("device-profiles-pending")
        self.wait_write(MOUSE_PROFILES, before)
        self.ui.retain("keep profile edits quiet while writes drain", self.quiet)
        outcomes = self.writes(MOUSE_PROFILES)[before:]
        if not 1 <= len(outcomes) <= 2 or any(outcome != "ok" for outcome in outcomes):
            raise AssertionError(f"The delayed profile writes did not drain: {outcomes}")
        self.refresh_device()
        self.subpage("Profiles")
        readback = self.layer_order(("Natural Scrolling", "Typing"))
        self.ui.dump("device-profiles-delayed-readback")
        self.ui.screenshot("device-profiles-delayed-readback")
        (self.output / "device-profiles-delayed.json").write_text(json.dumps({
            "first_drag": first, "drag_during_write": second,
            "completed_writes_before_second_drag": 0,
            "completed_writes": outcomes, "refreshed_order": readback,
        }, indent=2) + "\n")

    def rapid_settings(self, values):
        control = self.ui.control("Pointer Sensitivity", "queryValue")
        self.quiet()
        before = len(self.writes(MOUSE_SETTINGS))
        for value in values:
            if not control.getState().contains(pyatspi.STATE_SENSITIVE):
                raise AssertionError("Pointer Sensitivity became disabled during apply")
            control.queryValue().currentValue = value
            if control.queryValue().currentValue != value:
                raise AssertionError(f"Pointer Sensitivity did not retain {value}")
        self.quiet()
        if len(self.writes(MOUSE_SETTINGS)) != before:
            raise AssertionError("Rapid edits did not finish before the delayed apply")
        return before

    def drained_settings(self, before):
        outcomes = self.writes(MOUSE_SETTINGS)[before:]
        if not 1 <= len(outcomes) <= 2 or any(outcome != "ok" for outcome in outcomes):
            raise AssertionError(f"Quit did not drain the coalesced settings jobs: {outcomes}")

    def quiet_close(self):
        self.delayed_settings()
        before = self.rapid_settings((1400, 1450, 1500))
        self.ui.key("Control_L", "w")
        self.hidden()
        self.wait_write(MOUSE_SETTINGS, before)
        self.remote(hidden=True)
        self.hidden()
        self.remote()
        self.nav("Example Mouse")
        self.ui.wait("retain the latest setting after reopening", lambda:
            self.ui.number("Pointer Sensitivity") == 1500)
        self.ui.retain("keep editing quiet after reopening", self.quiet)
        # Exit drains a possible follow-up job before a fresh simulated session starts.
        self.quit()
        self.drained_settings(before)

    def quiet_quit(self, rapid=False):
        self.ui.x11.close()
        self.ui = Desktop(self.launch(), self.output)
        self.ui.expect("Example Adapter 1")
        self.ui.expect("Example Trackball")
        self.nav("Example Mouse")
        self.ui.expect("Pointer Sensitivity")
        self.ui.scroll_into_view(self.ui.row("Pointer Sensitivity"))
        self.main_page()
        self.quiet()
        if rapid:
            before = self.rapid_settings((1400, 1450, 1500))
        else:
            entry = self.ui.control("Pointer Sensitivity", "queryEditableText")
            self.ui.focus(entry)
            before = len(self.writes(MOUSE_SETTINGS))
            if not entry.queryEditableText().setTextContents("1500"):
                raise AssertionError("Pointer Sensitivity stopped accepting text")
            if text(entry) != "1500" or not entry.getState().contains(pyatspi.STATE_FOCUSED):
                raise AssertionError("The final numeric edit did not retain its text and focus")
            self.quiet()
            if len(self.writes(MOUSE_SETTINGS)) != before:
                raise AssertionError("Typing committed the numeric edit before Quit")
        self.ui.key("Control_L", "q")
        self.quit_pending()
        self.wait_quit()
        if rapid:
            self.drained_settings(before)
        elif self.writes(MOUSE_SETTINGS)[before:] != ["ok"]:
            raise AssertionError("Quit did not commit and complete the focused numeric edit")

    def quit_pending(self, before=None):
        status = self.ui.find("Settings are being applied", {"status bar"})
        if before is not None and len(self.writes(MOUSE_SETTINGS)) != before:
            raise AssertionError("The failed write completed before Quit became pending")
        window = status.parent
        while window is not None and window.getRoleName() not in {"frame", "window", "dialog"}:
            window = window.parent
        if window is None or window.name != "Cordial":
            raise AssertionError("Quit progress has no separate Cordial window")
        for name in ("Continue Editing", "Quit Anyway"):
            if self.ui.matches(name, BUTTONS):
                raise AssertionError(f"Pending Quit exposes {name}")
        self.ui.dump(self.stage + "-pending")
        self.ui.screenshot(self.stage + "-pending")
        return window

    def failed_quit(self, continue_editing=True):
        self.ui.x11.close()
        self.env["CORDIAL_SIM_WRITE_DELAY_MS"] = "2000"
        self.env["CORDIAL_SIM_FAIL_SETTINGS_ONCE"] = "1"
        self.ui = Desktop(self.launch(), self.output)
        self.env["CORDIAL_SIM_WRITE_DELAY_MS"] = "800"
        self.env["CORDIAL_SIM_FAIL_SETTINGS_ONCE"] = "0"
        self.ui.expect("Example Adapter 1")
        self.ui.expect("Example Trackball")
        self.nav("Example Mouse")
        self.ui.expect("Pointer Sensitivity")
        self.ui.scroll_into_view(self.ui.row("Pointer Sensitivity"))
        self.main_page()
        self.quiet()
        before = len(self.writes(MOUSE_SETTINGS))
        self.ui.number("Pointer Sensitivity", 1500)
        self.quiet()
        self.ui.key("Control_L", "q")
        window = self.quit_pending(before)
        self.ui.find(SETTINGS_WRITE_FAILED, {"status bar"}, window)
        for name in ("Continue Editing", "Quit Anyway"):
            self.enabled(name, root=window)
        self.ui.wait("record the rejected settings write", lambda:
            self.writes(MOUSE_SETTINGS)[before:] == ["error"])
        self.ui.retain("keep failed Quit open without replaying the write", lambda:
            self.ui.matches(SETTINGS_WRITE_FAILED, {"status bar"}, window)
            and self.writes(MOUSE_SETTINGS)[before:] == ["error"])
        self.ui.dump(self.stage + "-failed")
        self.ui.screenshot(self.stage + "-failed")
        if continue_editing:
            self.ui.click("Continue Editing", root=window)
            for name in ("Continue Editing", "Quit Anyway", "Settings are being applied"):
                self.ui.absent(name)
            self.nav("Overview")
            self.nav("Example Mouse")
            note = "Couldn't Save: The adapter couldn't save the change"
            self.ui.expect(note)
            self.ui.wait("restore the rejected setting's saved value", lambda:
                self.ui.number("Pointer Sensitivity") == 1000)
            self.ui.retain("retain the same error after Continue Editing without replay", lambda:
                self.ui.matches(note)
                and self.writes(MOUSE_SETTINGS)[before:] == ["error"]
                and self.quiet())
            self.ui.dump(self.stage + "-editing")
            self.ui.screenshot(self.stage + "-editing")
            self.apply(MOUSE_SETTINGS, lambda: self.ui.number("Pointer Sensitivity", 1500))
            self.ui.absent(note)
            self.refresh_device()
            self.ui.wait("read the deliberately retried setting", lambda:
                self.ui.number("Pointer Sensitivity") == 1500)
            self.quit()
            expected = ["error", "ok"]
        else:
            self.ui.click("Quit Anyway", root=window)
            self.wait_quit()
            expected = ["error"]
        outcomes = self.writes(MOUSE_SETTINGS)[before:]
        if outcomes != expected:
            raise AssertionError(f"Failed Quit replayed or lost a settings write: {outcomes}")
        (self.output / (self.stage + ".json")).write_text(json.dumps({
            "failure_outcome": "no-effect", "delay_ms": 2000,
            "response": "Continue Editing" if continue_editing else "Quit Anyway",
            "completed_writes": outcomes,
        }, indent=2) + "\n")

    def run(self):
        self.startup()
        for name, action in [
            ("overview", self.overview), ("about", self.about),
            ("adapters", self.adapters), ("search", self.search_flow), ("details", self.details),
            ("settings", self.settings), ("diagnostics", self.diagnostics),
            ("profiles", self.profiles), ("pairing", self.pairing),
            ("files", self.files), ("preferences", self.preferences_flow),
            ("narrow", self.narrow), ("close-relaunch", self.close_relaunch),
            ("persistence", self.persistence),
            ("quiet-profiles", self.quiet_profiles),
            ("quiet-close", self.quiet_close),
            ("quiet-quit", lambda: self.quiet_quit(rapid=True)),
            ("focused-quit", self.quiet_quit),
            ("failed-quit-continue", self.failed_quit),
            ("failed-quit-anyway", lambda: self.failed_quit(continue_editing=False)),
        ]:
            self.step(name, action)
        self.stage = "quit"
        self.completed.append("quit")

    def close(self):
        for process in self.processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        # The document portal mounts under the temporary cache directory. Stop the private
        # session's portal processes before TemporaryDirectory removes that directory.
        for name in ("org.freedesktop.portal.Desktop", "org.freedesktop.impl.portal.desktop.gtk",
                     "org.freedesktop.portal.Documents"):
            try:
                pid = self.bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                    GLib.Variant("(s)", (name,)), GLib.VariantType.new("(u)"),
                    Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
                environ = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
                if os.fsencode(f"XDG_CONFIG_HOME={self.config}") in environ:
                    os.kill(pid, signal.SIGTERM)
            except (GLib.Error, FileNotFoundError, ProcessLookupError):
                pass
        mount = self.root / "cache/doc"
        mounts = [line.split()[4] for line in Path("/proc/self/mountinfo").read_text().splitlines()]
        if str(mount) in mounts:
            unmount = shutil.which("fusermount3") or shutil.which("fusermount")
            if not unmount:
                raise RuntimeError("Cannot unmount the isolated document portal: fusermount is missing")
            result = subprocess.run([unmount, "-uz", str(mount)], timeout=5,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            mounted = any(line.split()[4] == str(mount)
                          for line in Path("/proc/self/mountinfo").read_text().splitlines())
            if result.returncode and mounted:
                raise RuntimeError(f"Cannot unmount the isolated document portal: {result.stderr.decode()}")
        if self.ui:
            self.ui.x11.close()
        shutil.copyfile(self.trace, self.output / "writes.txt")
        if hasattr(self, "pairing_trace"):
            shutil.copyfile(self.pairing_trace, self.output / "pairing-reconnect.txt")
        self.log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    scenarios = parser.add_mutually_exclusive_group()
    scenarios.add_argument("--empty-states", action="store_true",
                           help="Check empty device Profiles and per-adapter Add Device rows.")
    scenarios.add_argument("--pairing-reconnect", action="store_true",
                           help="Check pairing results across a framed adapter reconnect.")
    args = parser.parse_args()
    binary = os.environ.get("CORDIAL_TEST_BINARY")
    if not binary or not Path(binary).is_file() or not os.access(binary, os.X_OK):
        parser.error("Set CORDIAL_TEST_BINARY to the executable built with the desktop feature.")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    bus = require_private_session()
    with tempfile.TemporaryDirectory(prefix="cordial-gtk-smoke-") as directory:
        smoke = Smoke(Path(binary).resolve(), Path(directory), output, bus)
        try:
            if args.empty_states:
                smoke.run_empty_states()
            elif args.pairing_reconnect:
                smoke.run_pairing_reconnect()
            else:
                smoke.run()
        except Exception as error:
            if smoke.ui:
                try:
                    smoke.ui.dump("failure")
                    smoke.ui.screenshot("failure")
                except Exception as capture_error:
                    print(f"Cannot capture failure: {capture_error}", file=sys.stderr)
            smoke.report("failed", str(error))
            traceback.print_exc()
            print(f"FAIL {smoke.stage}. See {output / 'application.log'} and failure.txt.",
                  file=sys.stderr)
            return 1
        finally:
            smoke.close()
    smoke.report("passed")
    print(f"PASS native GTK smoke. Artifacts: {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

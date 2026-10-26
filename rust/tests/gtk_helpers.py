"""AT-SPI controls and X11 window operations for the native desktop smoke fixture."""

import ctypes
import ctypes.util
from collections import deque
import os
from pathlib import Path
import subprocess
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib
import pyatspi


BUTTONS = {"push button", "button", "toggle button", "menu item"}
ROWS = {"list item", "table row"}


class WindowAttributes(ctypes.Structure):
    _fields_ = [("x", ctypes.c_int), ("y", ctypes.c_int),
        ("width", ctypes.c_int), ("height", ctypes.c_int),
        ("border_width", ctypes.c_int), ("depth", ctypes.c_int),
        ("visual", ctypes.c_void_p), ("root", ctypes.c_ulong),
        ("window_class", ctypes.c_int), ("bit_gravity", ctypes.c_int),
        ("win_gravity", ctypes.c_int), ("backing_store", ctypes.c_int),
        ("backing_planes", ctypes.c_ulong), ("backing_pixel", ctypes.c_ulong),
        ("save_under", ctypes.c_int), ("colormap", ctypes.c_ulong),
        ("map_installed", ctypes.c_int), ("map_state", ctypes.c_int),
        ("all_event_masks", ctypes.c_long), ("your_event_mask", ctypes.c_long),
        ("do_not_propagate_mask", ctypes.c_long), ("override_redirect", ctypes.c_int),
        ("screen", ctypes.c_void_p)]


def walk(root):
    pending = deque([root])
    seen = set()
    while pending:
        node = pending.popleft()
        if node in seen:
            continue
        seen.add(node)
        try:
            if node.getState().contains(pyatspi.STATE_DEFUNCT):
                continue
            children = [node.getChildAtIndex(i) for i in range(node.childCount)]
        except (GLib.Error, RuntimeError):
            continue
        yield node
        pending.extend(child for child in children if child is not None)


def visible(node):
    return node.getState().contains(pyatspi.STATE_VISIBLE)


def text(node):
    try:
        value = node.queryText()
        return value.getText(0, value.characterCount)
    except (NotImplementedError, GLib.Error):
        return node.name or ""


class X11:
    """Uses only windows belonging to the launched process."""

    def __init__(self):
        self.lib = ctypes.CDLL(ctypes.util.find_library("X11"))
        self.lib.XOpenDisplay.restype = ctypes.c_void_p
        self.display = self.lib.XOpenDisplay(None)
        if not self.display:
            raise RuntimeError("Cannot open the X11 display. Use xvfb-run.")
        signatures = {
            "XDefaultRootWindow": (ctypes.c_ulong, [ctypes.c_void_p]),
            "XInternAtom": (ctypes.c_ulong, [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]),
            "XQueryTree": (ctypes.c_int, [ctypes.c_void_p, ctypes.c_ulong,
                ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_ulong),
                ctypes.POINTER(ctypes.POINTER(ctypes.c_ulong)), ctypes.POINTER(ctypes.c_uint)]),
            "XGetWindowProperty": (ctypes.c_int, [ctypes.c_void_p, ctypes.c_ulong,
                ctypes.c_ulong, ctypes.c_long, ctypes.c_long, ctypes.c_int, ctypes.c_ulong,
                ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_int),
                ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_ulong),
                ctypes.POINTER(ctypes.POINTER(ctypes.c_ubyte))]),
            "XFree": (ctypes.c_int, [ctypes.c_void_p]),
            "XResizeWindow": (ctypes.c_int, [ctypes.c_void_p, ctypes.c_ulong,
                ctypes.c_uint, ctypes.c_uint]),
            "XGetWindowAttributes": (ctypes.c_int, [ctypes.c_void_p, ctypes.c_ulong,
                ctypes.POINTER(WindowAttributes)]),
            "XSetInputFocus": (ctypes.c_int, [ctypes.c_void_p, ctypes.c_ulong,
                ctypes.c_int, ctypes.c_ulong]),
            "XTranslateCoordinates": (ctypes.c_int, [ctypes.c_void_p, ctypes.c_ulong,
                ctypes.c_ulong, ctypes.c_int, ctypes.c_int, ctypes.POINTER(ctypes.c_int),
                ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_ulong)]),
            "XStringToKeysym": (ctypes.c_ulong, [ctypes.c_char_p]),
            "XKeysymToKeycode": (ctypes.c_ubyte, [ctypes.c_void_p, ctypes.c_ulong]),
            "XFlush": (ctypes.c_int, [ctypes.c_void_p]),
            "XCloseDisplay": (ctypes.c_int, [ctypes.c_void_p]),
        }
        for name, (result, args) in signatures.items():
            fn = getattr(self.lib, name)
            fn.restype, fn.argtypes = result, args
        self.pid_atom = self.lib.XInternAtom(self.display, b"_NET_WM_PID", False)

    def windows(self, pid):
        root = self.lib.XDefaultRootWindow(self.display)
        parent, root_return = ctypes.c_ulong(), ctypes.c_ulong()
        children = ctypes.POINTER(ctypes.c_ulong)()
        count = ctypes.c_uint()
        self.lib.XQueryTree(self.display, root, ctypes.byref(root_return),
                           ctypes.byref(parent), ctypes.byref(children), ctypes.byref(count))
        result = []
        try:
            for i in range(count.value):
                window = children[i]
                actual, size, remaining = ctypes.c_ulong(), ctypes.c_ulong(), ctypes.c_ulong()
                fmt = ctypes.c_int()
                data = ctypes.POINTER(ctypes.c_ubyte)()
                self.lib.XGetWindowProperty(self.display, window, self.pid_atom, 0, 1,
                    False, 0, ctypes.byref(actual), ctypes.byref(fmt), ctypes.byref(size),
                    ctypes.byref(remaining), ctypes.byref(data))
                try:
                    if data and fmt.value == 32 and size.value == 1:
                        if ctypes.cast(data, ctypes.POINTER(ctypes.c_ulong))[0] == pid:
                            attrs = WindowAttributes()
                            if (self.lib.XGetWindowAttributes(self.display, window, ctypes.byref(attrs))
                                    and attrs.map_state == 2 and not attrs.override_redirect):
                                result.append(window)
                finally:
                    if data:
                        self.lib.XFree(data)
        finally:
            if children:
                self.lib.XFree(children)
        return result

    def key(self, *keys):
        codes = []
        for key in keys:
            sym = self.lib.XStringToKeysym(key.encode("ascii"))
            code = self.lib.XKeysymToKeycode(self.display, sym)
            if not code:
                raise AssertionError(f"No X11 keycode for {key!r}")
            codes.append(code)
        try:
            for code in codes:
                pyatspi.Registry.generateKeyboardEvent(code, None, pyatspi.KEY_PRESS)
        finally:
            for code in reversed(codes):
                pyatspi.Registry.generateKeyboardEvent(code, None, pyatspi.KEY_RELEASE)

    def minimum_size(self, pid):
        window = self.windows(pid)[0]
        atom = self.lib.XInternAtom(self.display, b"WM_NORMAL_HINTS", False)
        actual, size, remaining = ctypes.c_ulong(), ctypes.c_ulong(), ctypes.c_ulong()
        fmt = ctypes.c_int()
        data = ctypes.POINTER(ctypes.c_ubyte)()
        self.lib.XGetWindowProperty(self.display, window, atom, 0, 18, False, 0,
            ctypes.byref(actual), ctypes.byref(fmt), ctypes.byref(size),
            ctypes.byref(remaining), ctypes.byref(data))
        try:
            if data and fmt.value == 32 and size.value >= 7:
                hints = ctypes.cast(data, ctypes.POINTER(ctypes.c_ulong))
                if hints[0] & (1 << 4):
                    return hints[5], hints[6]
            raise AssertionError("The window does not advertise a minimum size")
        finally:
            if data:
                self.lib.XFree(data)

    def close(self):
        if self.display:
            self.lib.XCloseDisplay(self.display)
            self.display = None


class Desktop:
    def __init__(self, process, output):
        Atspi.set_timeout(2000, 2000)
        self.process = process
        self.output = Path(output)
        self.x11 = X11()
        self.external_pids = set()

    def wait(self, description, predicate, timeout=10, running=True):
        end = time.monotonic() + timeout
        error = None
        while time.monotonic() < end:
            if running and self.process.poll() is not None:
                raise AssertionError(f"Cordial exited with {self.process.returncode} while {description}")
            try:
                result = predicate()
                if result:
                    return result
            except (GLib.Error, RuntimeError) as exc:
                error = exc
            # The main loop dispatches AT-SPI events between bounded predicate checks.
            loop = GLib.MainLoop()
            GLib.timeout_add(50, lambda: (loop.quit(), GLib.SOURCE_REMOVE)[1])
            loop.run()
        raise AssertionError(f"Timed out after {timeout}s: {description}. Last AT-SPI error: {error}")

    def application(self):
        desktop = pyatspi.Registry.getDesktop(0)
        for app in desktop:
            if app and app.get_process_id() == self.process.pid:
                return app
        return None

    def nodes(self, root=None):
        if root is not None:
            return list(walk(root))
        pids = self.external_pids | {self.process.pid}
        return [node for app in pyatspi.Registry.getDesktop(0)
                if app and app.get_process_id() in pids for node in walk(app)]

    def matches(self, name, roles=None, root=None):
        return [n for n in self.nodes(root) if visible(n) and n.name == name
                and (roles is None or n.getRoleName() in roles)]

    def find(self, name, roles=None, root=None):
        def one():
            matches = self.matches(name, roles, root)
            outer = []
            for node in matches:
                parent = node.parent
                while parent is not None and parent not in matches:
                    parent = parent.parent
                if parent is None:
                    outer.append(node)
            matches = outer
            if len(matches) > 1:
                raise AssertionError(f"Ambiguous {name!r}, roles {roles}: {len(matches)} matches")
            return matches[0] if matches else None
        return self.wait(f"find {name!r}, roles {roles}", one)

    def expect(self, name, root=None):
        return self.wait(f"show {name!r}", lambda: self.matches(name, root=root))

    def settings_page(self):
        def shown():
            for control in self.matches("Start at Login", {"switch", "check box", "toggle button"}):
                node = control.parent
                while node is not None:
                    if node.getRoleName() in {"grouping", "panel"} and node.name == "Settings":
                        return node
                    node = node.parent
            return None
        page = self.wait("show Settings in the content pane", shown)
        if any(visible(n) and n.getRoleName() in {"dialog", "alert"} for n in self.nodes()):
            raise AssertionError("Settings opened a modal dialog")
        frame = page.parent
        while frame is not None and frame.getRoleName() not in {"frame", "window"}:
            frame = frame.parent
        if frame is None:
            raise AssertionError("Settings has no accessible window")
        if any(n.getState().contains(pyatspi.STATE_SHOWING)
               for n in self.matches("Overview", {"list item"})):
            self.wait("select Settings in the sidebar", lambda: any(
                n.getState().contains(pyatspi.STATE_SHOWING)
                and n.getState().contains(pyatspi.STATE_SELECTED)
                for n in self.matches("Settings", {"list item"})))
        else:
            self.find("Back", BUTTONS)
        for title in ("Start at Login", "Always Show Tray Icon", "Low Battery", "Connections"):
            self.find(title, {"switch", "check box", "toggle button"}, page)
        self.control("Low Battery Level", "queryValue", page)
        return page

    def absent(self, name, root=None):
        self.wait(f"remove {name!r}", lambda: not self.matches(name, root=root))

    def copy_link(self, node):
        gi.require_version("Gdk", "4.0")
        gi.require_version("Gtk", "4.0")
        from gi.repository import Gdk, Gio, Gtk

        Gdk.set_allowed_backends("x11")
        Gtk.init()
        display = Gdk.Display.open(os.environ["DISPLAY"])
        if display is None:
            raise AssertionError("Cannot open the fixture's X11 display for clipboard access")
        cancellable = Gio.Cancellable()
        result = []

        def read(clipboard, pending):
            try:
                result.append(clipboard.read_text_finish(pending))
            except GLib.Error as error:
                result.append(error)

        try:
            action = node.queryAction()
            copies = [i for i in range(action.nActions) if action.getName(i) == "link.copy"]
            if len(copies) != 1 or not action.doAction(copies[0]):
                raise AssertionError(f"Cannot copy the link destination for {node.name!r}")
            display.get_clipboard().read_text_async(cancellable, read)
            self.wait(f"read the copied link destination for {node.name!r}",
                      lambda: result, timeout=5)
            if isinstance(result[0], GLib.Error):
                raise AssertionError(f"Cannot read the copied link: {result[0]}")
            return result[0]
        finally:
            cancellable.cancel()
            display.close()

    def click(self, name=None, roles=BUTTONS, root=None, node=None):
        node = node if node is not None else self.find(name, roles, root)
        self.scroll_into_view(node)
        self.wait(f"enable {node.name!r}", lambda:
                  node.getState().contains(pyatspi.STATE_SENSITIVE))
        preferred = ("click", "activate", "press", "toggle")
        for control in walk(node):
            if control != node and (control.name != node.name
                                    or control.getRoleName() not in BUTTONS):
                continue
            action = control.queryAction()
            for wanted in preferred:
                for i in range(action.nActions):
                    if action.getName(i).lower() == wanted:
                        if not action.doAction(i):
                            raise AssertionError(f"AT-SPI refused {wanted} on {control.name!r}")
                        return
        self.mouse(node)

    def pointer_point(self, node, left=False):
        self.scroll_into_view(node)
        component = node.queryComponent()
        # GTK's screen coordinates are unavailable on some versions; window coordinates work.
        x, y, width, height = component.getExtents(pyatspi.XY_WINDOW)
        if width <= 0 or height <= 0 or not node.getState().contains(pyatspi.STATE_SHOWING):
            raise AssertionError(f"{node.name!r} has no visible accessible bounds")
        pid = node.get_process_id()
        if pid not in self.external_pids | {self.process.pid}:
            raise AssertionError("The accessible object is outside the fixture's processes")
        window = self.wait("find the accessible object's X11 window", lambda: self.x11.windows(pid))[-1]
        dx, dy, child = ctypes.c_int(), ctypes.c_int(), ctypes.c_ulong()
        self.x11.lib.XTranslateCoordinates(self.x11.display, window,
            self.x11.lib.XDefaultRootWindow(self.x11.display), 0, 0,
            ctypes.byref(dx), ctypes.byref(dy), ctypes.byref(child))
        px = dx.value + x + (min(24, width // 4) if left else width // 2)
        return px, dy.value + y + height // 2

    def mouse(self, node):
        px, py = self.pointer_point(node)
        pyatspi.Registry.generateMouseEvent(px, py, "abs")
        pyatspi.Registry.generateMouseEvent(px, py, "b1c")

    def drag(self, source, target, capture=None):
        self.scroll_into_view(source)
        self.scroll_into_view(target)
        start = self.pointer_point(source, left=True)
        finish = self.pointer_point(target, left=True)
        if source == target or start == finish:
            raise AssertionError("A drag needs two distinct rows")
        pyatspi.Registry.generateMouseEvent(*start, "abs")
        pyatspi.Registry.generateMouseEvent(*start, "b1p")
        try:
            time.sleep(0.05)
            for step in range(1, 13):
                point = tuple(round(a + (b - a) * step / 12) for a, b in zip(start, finish))
                pyatspi.Registry.generateMouseEvent(*point, "abs")
                time.sleep(0.01)
            if capture:
                self.screenshot(capture)
        finally:
            pyatspi.Registry.generateMouseEvent(*finish, "b1r")
        return {"start": start, "drop": finish}

    def row(self, title, root=None):
        def find():
            candidates = set()
            for node in self.matches(title, root=root):
                while node is not None:
                    if (node.getRoleName() in ROWS | {"combo box"}
                            or (node.getRoleName() == "switch" and node.parent is not None
                                and node.parent.getRoleName() == "list")):
                        candidates.add(node)
                        break
                    node = node.parent
            if len(candidates) > 1:
                raise AssertionError(f"Ambiguous accessible row {title!r}: {len(candidates)} matches")
            return next(iter(candidates), None)
        return self.wait(f"find accessible row {title!r}", find)

    def control(self, title, interface, root=None):
        def query(node):
            try:
                getattr(node, interface)()
                return True
            except (NotImplementedError, GLib.Error):
                return False
        def find():
            named = [n for n in self.matches(title, root=root) if query(n)]
            if len(named) == 1:
                if interface == "queryEditableText":
                    fields = [n for n in walk(named[0]) if visible(n) and query(n)
                              and n.getRoleName() in {"text", "entry", "password text"}]
                    if fields:
                        return fields[-1]
                return named[0]
            for anchor in self.matches(title, root=root):
                node = anchor
                for _ in range(4):
                    controls = [n for n in walk(node) if visible(n) and query(n)]
                    if len(controls) == 1:
                        return controls[0]
                    if node.parent is None:
                        break
                    node = node.parent
            return None
        return self.wait(f"find {interface} control {title!r}", find)

    def edit(self, title, value, root=None):
        node = self.control(title, "queryEditableText", root)
        self.focus(node)
        if not node.queryEditableText().setTextContents(str(value)):
            raise AssertionError(f"Cannot edit {title!r}")
        self.wait(f"read back {title!r} = {value!r}", lambda: text(node) == str(value))
        return node

    def number(self, title, value=None, root=None):
        node = self.control(title, "queryValue", root)
        if value is not None:
            self.scroll_into_view(node)
            node.queryValue().currentValue = value
            self.wait(f"read back {title!r} = {value}",
                      lambda: node.queryValue().currentValue == value)
        return node.queryValue().currentValue

    def switch(self, title, on, root=None):
        def switches():
            named = self.matches(title, {"switch", "check box", "toggle button"}, root)
            for node in reversed(named):
                if node.queryAction().nActions:
                    return node
            for anchor in self.matches(title, root=root):
                for n in walk(anchor):
                    if (n.getRoleName() in {"switch", "check box", "toggle button"}
                            and n.queryAction().nActions):
                        return n
            return None
        node = self.wait(f"find switch {title!r}", switches)
        checked = lambda: node.getState().contains(pyatspi.STATE_CHECKED)
        if checked() != on:
            self.click(node=node)
        self.wait(f"{title!r} is {on}", lambda: checked() == on)

    def choose(self, title, value, root=None):
        row = self.row(title, root)
        self.click(node=row)
        option = self.find(value, {"list item", "radio button", "menu item", "label"}, root=row)
        while option.getRoleName() == "label" or option.getRoleName() == "panel":
            option = option.parent
        if option.getRoleName() not in {"list item", "radio button", "menu item"}:
            raise AssertionError(f"{value!r} is not a selectable choice for {title!r}")
        self.key("Home")
        for _ in range(option.getIndexInParent()):
            self.key("Down")
        self.key("Return")
        self.wait(f"close the choices for {title!r} and show {value!r}", lambda:
            row.getState().contains(pyatspi.STATE_FOCUSED)
            and not row.getState().contains(pyatspi.STATE_EXPANDED)
            and self.matches(value, {"label"}, root=row))

    def focus(self, node):
        self.scroll_into_view(node)
        pid = node.get_process_id()
        if pid not in self.external_pids | {self.process.pid}:
            raise AssertionError("The accessible object is outside the fixture's processes")
        window = self.wait("find the accessible object's X11 window", lambda: self.x11.windows(pid))[-1]
        self.x11.lib.XSetInputFocus(self.x11.display, window, 2, 0)
        self.x11.lib.XFlush(self.x11.display)
        focused = lambda: node.getState().contains(pyatspi.STATE_FOCUSED)
        if focused():
            return
        try:
            if node.queryComponent().grabFocus():
                self.wait(f"focus {node.name!r}", focused)
                return
        except GLib.Error:
            pass
        self.mouse(node)
        self.wait(f"focus {node.name!r}", focused)

    def scroll_into_view(self, node):
        pane = node.parent
        while pane is not None and pane.getRoleName() != "scroll pane":
            pane = pane.parent
        if pane is None:
            return

        def reveal():
            _, top, _, height = pane.queryComponent().getExtents(pyatspi.XY_WINDOW)
            _, y, _, h = node.queryComponent().getExtents(pyatspi.XY_WINDOW)
            if h > 0 and top <= y and y + h <= top + height:
                return node.getState().contains(pyatspi.STATE_SHOWING)
            bars = [n for n in walk(pane) if visible(n) and n.getRoleName() == "scroll bar"
                    and (n.getState().contains(pyatspi.STATE_VERTICAL)
                         or n.queryComponent().getSize()[1] > n.queryComponent().getSize()[0])]
            if len(bars) != 1:
                raise AssertionError(f"Cannot find the vertical scrollbar for {node.name!r}")
            scroll = bars[0].queryValue()
            target = scroll.currentValue + y + h / 2 - top - height / 2
            scroll.currentValue = max(scroll.minimumValue, min(scroll.maximumValue, target))
            return False

        # GTK does not implement AT-SPI Component.scrollTo; its scrollbar Value interface
        # controls the enclosing scrolled window's adjustment without moving keyboard focus.
        self.wait(f"scroll {node.name!r} into view", reveal)

    def key(self, *keys):
        self.x11.key(*keys)

    def screenshot(self, name):
        subprocess.run(["import", "-window", "root", str(self.output / f"{name}.png")],
                       check=True, timeout=10, stdout=subprocess.DEVNULL,
                       stderr=subprocess.PIPE)

    def button_bounds(self, button):
        return button.queryComponent().getExtents(pyatspi.XY_WINDOW)

    def retain(self, description, predicate, duration=1.1):
        end = time.monotonic() + duration
        def holds():
            if not predicate():
                raise AssertionError(f"State changed while {description}")
            return time.monotonic() >= end
        self.wait(description, holds, timeout=duration + 2)

    def dump(self, name):
        lines = []
        for node in self.nodes():
            try:
                states = node.getState()
                flags = [label for flag, label in [
                    (pyatspi.STATE_VISIBLE, "visible"),
                    (pyatspi.STATE_SHOWING, "showing"),
                    (pyatspi.STATE_SENSITIVE, "enabled"),
                    (pyatspi.STATE_FOCUSED, "focused"),
                    (pyatspi.STATE_CHECKED, "checked"),
                    (pyatspi.STATE_SELECTED, "selected"),
                ] if states.contains(flag)]
                try:
                    action = node.queryAction()
                    actions = [action.getName(i) for i in range(action.nActions)]
                except (NotImplementedError, GLib.Error):
                    actions = []
                lines.append(f"{node.getRoleName()} {node.name!r} {flags} "
                             f"actions={actions!r} text={text(node)!r}")
            except (GLib.Error, RuntimeError):
                continue
        (self.output / f"{name}.txt").write_text("\n".join(lines) + "\n")

    def resize(self, width, height):
        windows = self.wait("find Cordial's X11 window", lambda: self.x11.windows(self.process.pid))
        self.x11.lib.XResizeWindow(self.x11.display, windows[0], width, height)
        self.x11.lib.XFlush(self.x11.display)
        self.wait(f"window resized to {width} by {height}", lambda: any(
            n.getRoleName() in {"frame", "window"}
            and tuple(n.queryComponent().getSize()) == (width, height)
            for n in self.nodes()))

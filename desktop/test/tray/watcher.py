#!/usr/bin/env python3
"""A minimal StatusNotifierWatcher on a private bus. Records registered items
and prints each item's icon size, tooltip and menu labels as JSON lines."""
import json, sys
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

XML = """<node><interface name="org.kde.StatusNotifierWatcher">
<method name="RegisterStatusNotifierItem"><arg type="s" direction="in"/></method>
<method name="RegisterStatusNotifierHost"><arg type="s" direction="in"/></method>
<property name="RegisteredStatusNotifierItems" type="as" access="read"/>
<property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
<property name="ProtocolVersion" type="i" access="read"/>
<signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
</interface></node>"""

bus = Gio.bus_get_sync(Gio.BusType.SESSION)
items = []
seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 20

def labels(service, path):
    try:
        r = bus.call_sync(service, path, "com.canonical.dbusmenu", "GetLayout",
                          GLib.Variant("(iias)", (0, -1, [])), None, 0, 2000, None)
    except Exception as e:
        return [f"error: {e}"]
    out = []
    def walk(node, depth):
        _id, props, children = node
        if "label" in props:
            out.append(("  " * depth) + props["label"])
        for c in children:
            walk(c, depth + 1)
    walk(r.unpack()[1], -1)
    return out

def report():
    for sender, path in items:
        def get(name):
            try:
                return bus.call_sync(sender, path, "org.freedesktop.DBus.Properties", "Get",
                                     GLib.Variant("(ss)", ("org.kde.StatusNotifierItem", name)),
                                     None, 0, 2000, None).unpack()[0]
            except Exception as e:
                return f"error: {e}"
        pix = get("IconPixmap")
        menu = get("Menu")
        tip = get("ToolTip")
        print(json.dumps({
            "item": f"{sender}{path}",
            "status": get("Status"),
            "icon_sizes": [[w, h] for (w, h, _) in pix] if isinstance(pix, list) else pix,
            "tooltip": tip if isinstance(tip, str) else (list(tip)[2:] if tip else None),
            "menu": labels(sender, menu) if isinstance(menu, str) else menu,
        }), flush=True)
    return True

def method(conn, sender, path, iface, name, params, inv):
    if name == "RegisterStatusNotifierItem":
        arg = params.unpack()[0]
        items.append((sender, arg if arg.startswith("/") else "/StatusNotifierItem"))
        conn.emit_signal(None, "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher",
                         "StatusNotifierItemRegistered", GLib.Variant("(s)", (sender,)))
    inv.return_value(None)

def prop(conn, sender, path, iface, name):
    return {"RegisteredStatusNotifierItems": GLib.Variant("as", [f"{s}{p}" for s, p in items]),
            "IsStatusNotifierHostRegistered": GLib.Variant("b", True),
            "ProtocolVersion": GLib.Variant("i", 0)}[name]

info = Gio.DBusNodeInfo.new_for_xml(XML)
bus.register_object("/StatusNotifierWatcher", info.interfaces[0], method, prop, None)
Gio.bus_own_name_on_connection(bus, "org.kde.StatusNotifierWatcher", 0, None, None)
loop = GLib.MainLoop()
GLib.timeout_add(int(seconds * 1000) - 500, report)
GLib.timeout_add(int(seconds * 1000), loop.quit)
print("watcher ready", flush=True)
loop.run()

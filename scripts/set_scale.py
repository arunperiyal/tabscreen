#!/usr/bin/python3
"""Temporarily set the scale of one monitor (not saved to monitors.xml).

Usage: set_scale.py CONNECTOR SCALE      e.g. set_scale.py Meta-1 2
Picks the supported scale nearest to SCALE and prints it.
"""
import sys

from gi.repository import Gio, GLib

DC = ("org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig", "org.gnome.Mutter.DisplayConfig")
TEMPORARY = 1

connector, wanted = sys.argv[1], float(sys.argv[2])
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
serial, monitors, logical, _props = bus.call_sync(*DC, "GetCurrentState", None, None, 0, -1, None).unpack()

current_mode = {}
for (conn, *_), modes, _mprops in monitors:
    for mode_id, _w, _h, _refresh, _preferred_scale, supported_scales, mode_props in modes:
        if mode_props.get("is-current"):
            current_mode[conn] = (mode_id, supported_scales)

config, applied = [], None
for x, y, scale, transform, primary, mons, _lprops in logical:
    conns = [m[0] for m in mons]
    if connector in conns:
        scale = min(current_mode[connector][1], key=lambda s: abs(s - wanted))
        applied = scale
    config.append((x, y, scale, transform, primary, [(c, current_mode[c][0], {}) for c in conns]))
if applied is None:
    sys.exit(f"no active monitor {connector}")

bus.call_sync(*DC, "ApplyMonitorsConfig",
              GLib.Variant("(uua(iiduba(ssa{sv}))a{sv})", (serial, TEMPORARY, config, {})),
              None, 0, -1, None)
print(f"{connector} scale {applied}")

#!/usr/bin/python3
"""Fullscreen GTK4 window on the monitor of a given size; prints touch/click events as JSON lines.

Usage: touch_target.py WIDTH HEIGHT
"""
import json
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk  # noqa: E402

WIDTH, HEIGHT = int(sys.argv[1]), int(sys.argv[2])


def emit(**kw):
    try:
        print(json.dumps(kw), flush=True)
    except BrokenPipeError:
        sys.exit(0)


def find_monitor(display):
    monitors = display.get_monitors()
    found = None
    for i in range(monitors.get_n_items()):
        m = monitors.get_item(i)
        g = m.get_geometry()
        emit(event="monitor", connector=m.get_connector(), x=g.x, y=g.y, w=g.width, h=g.height,
             scale=m.get_scale_factor())
        if (g.width, g.height) == (WIDTH, HEIGHT):
            found = m
    return found


def on_event(controller, _event):
    event = controller.get_current_event()
    t = event.get_event_type()
    names = {
        Gdk.EventType.TOUCH_BEGIN: "touch_begin",
        Gdk.EventType.TOUCH_UPDATE: "touch_update",
        Gdk.EventType.TOUCH_END: "touch_end",
        Gdk.EventType.BUTTON_PRESS: "button_press",
        Gdk.EventType.BUTTON_RELEASE: "button_release",
    }
    if t in names:
        ok, x, y = event.get_position()
        emit(event=names[t], x=round(x, 1), y=round(y, 1))
    return False


def on_activate(app):
    win = Gtk.ApplicationWindow(application=app, title="tabscreen touch target")
    area = Gtk.DrawingArea()
    win.set_child(area)
    controller = Gtk.EventControllerLegacy()
    controller.connect("event", on_event)
    win.add_controller(controller)

    monitor = find_monitor(win.get_display())
    if monitor is None:
        emit(event="error", message=f"no monitor of size {WIDTH}x{HEIGHT}")
        app.quit()
        return
    win.fullscreen_on_monitor(monitor)
    win.present()

    def ready():
        emit(event="ready", connector=monitor.get_connector(), w=win.get_width(), h=win.get_height())
        return False

    GLib.timeout_add(1500, ready)


app = Gtk.Application(application_id="dev.tabscreen.TouchTarget")
app.connect("activate", on_activate)
app.run([])

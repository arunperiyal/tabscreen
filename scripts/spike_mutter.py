#!/usr/bin/python3
"""M0 spike, fallback path: virtual monitor + touch through Mutter's own D-Bus APIs.

The portal path (spike_virtual.py) creates the virtual monitor but rejects every
touch with "Invalid position": xdg-desktop-portal validates absolute positions
against the stream size, and virtual streams don't report one. Mutter's
org.gnome.Mutter.RemoteDesktop / ScreenCast APIs (used by gnome-remote-desktop)
validate against the real virtual monitor and need no permission dialog.

Run with the system Python:  /usr/bin/python3 scripts/spike_mutter.py
"""
import argparse
import json
import sys
import time

import gi

gi.require_version("Gst", "1.0")
from gi.repository import Gio, GLib, Gst  # noqa: E402

from spike_virtual import log, mutter_monitors, touch_test  # noqa: E402

RD_BUS, RD_PATH, RD = "org.gnome.Mutter.RemoteDesktop", "/org/gnome/Mutter/RemoteDesktop", "org.gnome.Mutter.RemoteDesktop"
SC_BUS, SC_PATH, SC = "org.gnome.Mutter.ScreenCast", "/org/gnome/Mutter/ScreenCast", "org.gnome.Mutter.ScreenCast"
CURSOR_EMBEDDED = 1  # Mutter: 0 hidden, 1 embedded, 2 metadata


class Mutter:
    def __init__(self):
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION)
        self.rd_session = self.sc_session = self.stream = None
        self.node_id = None

    def call(self, bus_name, path, iface, method, params=None, reply=None):
        return self.bus.call_sync(bus_name, path, iface, method, params, reply, Gio.DBusCallFlags.NONE, -1, None)

    def methods(self, bus_name, path, iface):
        xml = self.call(bus_name, path, "org.freedesktop.DBus.Introspectable", "Introspect").unpack()[0]
        info = Gio.DBusNodeInfo.new_for_xml(xml).lookup_interface(iface)
        return [f"{m.name}({','.join(a.signature for a in m.in_args)})" for m in info.methods]

    def start(self, width, height):
        self.rd_session = self.call(RD_BUS, RD_PATH, RD, "CreateSession").unpack()[0]
        log("mutter", f"RemoteDesktop session {self.rd_session}: "
                      f"{self.methods(RD_BUS, self.rd_session, RD + '.Session')}")
        session_id = self.call(RD_BUS, self.rd_session, "org.freedesktop.DBus.Properties", "Get",
                               GLib.Variant("(ss)", (RD + ".Session", "SessionId"))).unpack()[0]

        self.sc_session = self.call(SC_BUS, SC_PATH, SC, "CreateSession", GLib.Variant(
            "(a{sv})", ({"remote-desktop-session-id": GLib.Variant("s", session_id)},))).unpack()[0]
        log("mutter", f"ScreenCast session {self.sc_session}: {self.methods(SC_BUS, self.sc_session, SC + '.Session')}")
        self.stream = self.call(SC_BUS, self.sc_session, SC + ".Session", "RecordVirtual", GLib.Variant(
            "(a{sv})", ({"cursor-mode": GLib.Variant("u", CURSOR_EMBEDDED)},))).unpack()[0]

        def on_stream_added(_conn, _sender, _path, _iface, _signal, params):
            self.node_id = params.unpack()[0]

        self.bus.signal_subscribe(SC_BUS, SC + ".Stream", "PipeWireStreamAdded", self.stream, None,
                                  Gio.DBusSignalFlags.NONE, on_stream_added)
        self.call(RD_BUS, self.rd_session, RD + ".Session", "Start")
        ctx = GLib.MainContext.default()
        deadline = time.monotonic() + 5
        while self.node_id is None and time.monotonic() < deadline:
            ctx.iteration(False)
            time.sleep(0.01)
        if self.node_id is None:
            raise RuntimeError("no PipeWireStreamAdded signal within 5s")
        return self.node_id

    def touch(self, method, *args):
        """Same call shape as spike_virtual.Portal.touch; the stream argument is ignored (we use our own)."""
        if method in ("NotifyTouchDown", "NotifyTouchMotion"):
            _stream, slot, x, y = args
            params = GLib.Variant("(sudd)", (self.stream, slot, x, y))
        else:
            params = GLib.Variant("(u)", args)
        self.call(RD_BUS, self.rd_session, RD + ".Session", method, params)

    def stop(self):
        if self.rd_session:
            try:
                self.call(RD_BUS, self.rd_session, RD + ".Session", "Stop")
            except GLib.Error as e:
                # Mutter tears the session down itself when the stream's consumer goes away.
                if "UnknownMethod" not in e.message:
                    log("mutter", f"Stop: {e.message}")
            self.rd_session = None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--size", default="1600x1000")
    ap.add_argument("--preview", action="store_true", help="show the stream in a window")
    ap.add_argument("--hold", type=int, default=15, help="seconds to keep the monitor alive at the end")
    args = ap.parse_args()
    width, height = map(int, args.size.split("x"))
    results = {}

    Gst.init(None)
    mutter = Mutter()
    before = {m[0] for m in mutter_monitors(mutter.bus)}
    log("mutter", f"monitors before: {sorted(before)}")
    node_id = mutter.start(width, height)
    log("mutter", f"stream {mutter.stream} → PipeWire node {node_id}")

    sink = "videoconvert ! autovideosink sync=false" if args.preview else "fakesink sync=false"
    desc = (f"pipewiresrc name=src path={node_id} do-timestamp=true keepalive-time=1000 "
            f"! video/x-raw,width={width},height={height} ! {sink}")
    log("gst", desc)
    pipeline = Gst.parse_launch(desc)
    frames = {"n": 0, "caps": None}

    def probe(pad, _info):
        frames["n"] += 1
        if frames["caps"] is None:
            frames["caps"] = pad.get_current_caps().to_string()
        return Gst.PadProbeReturn.OK

    pipeline.get_by_name("src").get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, probe)
    pipeline.set_state(Gst.State.PLAYING)
    gst_bus = pipeline.get_bus()

    def pump(seconds):
        end = time.monotonic() + seconds
        ctx = GLib.MainContext.default()
        while time.monotonic() < end:
            while ctx.pending():
                ctx.iteration(False)
            msg = gst_bus.timed_pop_filtered(50 * Gst.MSECOND, Gst.MessageType.ERROR)
            if msg:
                err, dbg = msg.parse_error()
                raise RuntimeError(f"pipeline error: {err.message}\n{dbg}")

    try:
        pump(3)
        log("gst", f"frames in first 3s: {frames['n']}; negotiated caps: {frames['caps']}")
        results["frames"] = frames["n"] > 0
        new = [m for m in mutter_monitors(mutter.bus) if m[0] not in before]
        for m in new:
            log("mutter", "new monitor: connector={} size={}x{} at ({},{}) scale={}".format(*m))
        results["monitor_added"] = bool(new)
        results["monitor_size"] = any((m[1], m[2]) == (width, height) for m in new)
        if results["monitor_size"]:
            results["touch"] = touch_test(mutter, None, width, height, pump)
        log("hold", f"keeping the virtual monitor for {args.hold}s (check Settings → Displays)")
        pump(args.hold)
    except (RuntimeError, GLib.Error) as e:
        log("error", str(e))
        results["no_errors"] = False
    finally:
        pipeline.set_state(Gst.State.NULL)
        mutter.stop()

    time.sleep(1)
    results["teardown"] = not [m for m in mutter_monitors(mutter.bus) if m[0] not in before]
    log("mutter", f"virtual monitor removed after stop: {results['teardown']}")
    print("\nRESULTS " + json.dumps(results))
    return 0 if all(results.values()) else 1


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/python3
"""M0 spike: virtual monitor + touch through the xdg-desktop-portal RemoteDesktop/ScreenCast APIs.

Checks, in order:
  1. A RemoteDesktop session with a VIRTUAL screencast source and a TOUCHSCREEN device starts.
  2. Mutter reports a new monitor of the requested size (size driven by pipewiresrc caps).
  3. Frames arrive from the PipeWire stream.
  4. Injected touches land on the virtual monitor at the expected coordinates.
  5. The restore token is saved; run again to check the permission dialog is skipped.

Run with the system Python (conda's lacks PyGObject):  /usr/bin/python3 scripts/spike_virtual.py
"""
import argparse
import json
import random
import subprocess
import sys
import threading
import time
from pathlib import Path

import gi

gi.require_version("Gst", "1.0")
from gi.repository import Gio, GLib, Gst  # noqa: E402

PORTAL_BUS = "org.freedesktop.portal.Desktop"
PORTAL_PATH = "/org/freedesktop/portal/desktop"
RD = "org.freedesktop.portal.RemoteDesktop"
SC = "org.freedesktop.portal.ScreenCast"
DEVICE_TOUCHSCREEN = 4
SOURCE_VIRTUAL = 4
CURSOR_EMBEDDED = 2
PERSIST_UNTIL_REVOKED = 2
TOKEN_FILE = Path.home() / ".config/tabscreen/restore_token"
HERE = Path(__file__).resolve().parent


def log(step, msg):
    print(f"[{time.strftime('%H:%M:%S')}] {step:<8} {msg}", flush=True)


class Portal:
    def __init__(self):
        self.bus = Gio.bus_get_sync(Gio.BusType.SESSION)
        self.sender = self.bus.get_unique_name()[1:].replace(".", "_")
        self.session = None

    def request(self, iface, method, args, options, signature):
        """Call a portal method that answers through a Request object and wait for its Response."""
        token = f"tabscreen{random.randrange(1 << 30)}"
        path = f"{PORTAL_PATH}/request/{self.sender}/{token}"
        result = {}

        def on_response(_conn, _sender, _path, _iface, _signal, params):
            result["code"], result["results"] = params.unpack()

        sub = self.bus.signal_subscribe(PORTAL_BUS, "org.freedesktop.portal.Request", "Response", path,
                                        None, Gio.DBusSignalFlags.NONE, on_response)
        options = {"handle_token": GLib.Variant("s", token), **options}
        self.bus.call_sync(PORTAL_BUS, PORTAL_PATH, iface, method, GLib.Variant(signature, (*args, options)),
                           None, Gio.DBusCallFlags.NONE, -1, None)
        ctx = GLib.MainContext.default()
        while "code" not in result:
            ctx.iteration(True)
        self.bus.signal_unsubscribe(sub)
        if result["code"] != 0:
            raise RuntimeError(f"{method}: portal response {result['code']} (1 = cancelled, 2 = failed)")
        return result["results"]

    def prop(self, iface, name):
        return self.bus.call_sync(PORTAL_BUS, PORTAL_PATH, "org.freedesktop.DBus.Properties", "Get",
                                  GLib.Variant("(ss)", (iface, name)), None, Gio.DBusCallFlags.NONE, -1,
                                  None).unpack()[0]

    def start(self, restore_token):
        res = self.request(RD, "CreateSession", (), {
            "session_handle_token": GLib.Variant("s", f"tabscreen{random.randrange(1 << 30)}"),
        }, "(a{sv})")
        self.session = res["session_handle"]

        opts = {"types": GLib.Variant("u", DEVICE_TOUCHSCREEN),
                "persist_mode": GLib.Variant("u", PERSIST_UNTIL_REVOKED)}
        if restore_token:
            opts["restore_token"] = GLib.Variant("s", restore_token)
        self.request(RD, "SelectDevices", (self.session,), opts, "(oa{sv})")

        self.request(SC, "SelectSources", (self.session,), {
            "types": GLib.Variant("u", SOURCE_VIRTUAL),
            "cursor_mode": GLib.Variant("u", CURSOR_EMBEDDED),
        }, "(oa{sv})")

        return self.request(RD, "Start", (self.session, ""), {}, "(osa{sv})")

    def open_pipewire_remote(self):
        ret, fds = self.bus.call_with_unix_fd_list_sync(
            PORTAL_BUS, PORTAL_PATH, SC, "OpenPipeWireRemote", GLib.Variant("(oa{sv})", (self.session, {})),
            GLib.VariantType("(h)"), Gio.DBusCallFlags.NONE, -1, None, None)
        return fds.get(ret.unpack()[0])

    def touch(self, method, *args):
        signature = "(oa{sv}uudd)" if method in ("NotifyTouchDown", "NotifyTouchMotion") else "(oa{sv}u)"
        self.bus.call_sync(PORTAL_BUS, PORTAL_PATH, RD, method, GLib.Variant(signature, (self.session, {}, *args)),
                           None, Gio.DBusCallFlags.NONE, -1, None)

    def close(self):
        if self.session:
            self.bus.call_sync(PORTAL_BUS, self.session, "org.freedesktop.portal.Session", "Close", None, None,
                               Gio.DBusCallFlags.NONE, -1, None)
            self.session = None


def mutter_monitors(bus):
    """[(connector, width, height, x, y, scale)] for active monitors, from Mutter's DisplayConfig."""
    serial, monitors, logical, _props = bus.call_sync(
        "org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig", "org.gnome.Mutter.DisplayConfig",
        "GetCurrentState", None, None, Gio.DBusCallFlags.NONE, -1, None).unpack()
    sizes = {}
    for (connector, *_), modes, _mprops in monitors:
        for _id, w, h, *_rest, mode_props in modes:
            if mode_props.get("is-current"):
                sizes[connector] = (w, h)
    out = []
    for x, y, scale, _transform, _primary, mons, _lprops in logical:
        for connector, *_ in mons:
            out.append((connector, *sizes.get(connector, (0, 0)), x, y, scale))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--size", default="1600x1000", help="requested virtual monitor size (default 1600x1000)")
    ap.add_argument("--no-size-caps", action="store_true", help="don't constrain width/height in caps")
    ap.add_argument("--preview", action="store_true", help="show the stream in a window")
    ap.add_argument("--no-touch-test", action="store_true")
    ap.add_argument("--forget", action="store_true", help="ignore the saved restore token")
    ap.add_argument("--hold", type=int, default=15, help="seconds to keep the monitor alive at the end")
    args = ap.parse_args()
    width, height = map(int, args.size.split("x"))
    results = {}

    Gst.init(None)
    portal = Portal()
    log("portal", f"ScreenCast sources={portal.prop(SC, 'AvailableSourceTypes')} "
                  f"cursor_modes={portal.prop(SC, 'AvailableCursorModes')} version={portal.prop(SC, 'version')}; "
                  f"RemoteDesktop devices={portal.prop(RD, 'AvailableDeviceTypes')} version={portal.prop(RD, 'version')}")
    before = {m[0] for m in mutter_monitors(portal.bus)}
    log("mutter", f"monitors before: {sorted(before)}")

    token = None if args.forget or not TOKEN_FILE.exists() else TOKEN_FILE.read_text().strip()
    log("portal", f"starting session ({'with' if token else 'without'} restore token) — approve the dialog if shown")
    t0 = time.monotonic()
    started = portal.start(token)
    elapsed = time.monotonic() - t0
    log("portal", f"Start returned after {elapsed:.1f}s: devices={started.get('devices')} "
                  f"streams={started.get('streams')}")
    results["restore_token_reused"] = bool(token) and elapsed < 1.0
    if token and not started.get("devices", 0) & DEVICE_TOUCHSCREEN:
        log("portal", "restored session has no touchscreen; closing and asking again — "
                      "enable 'Allow Remote Interaction' in the dialog")
        portal.close()
        started = portal.start(None)
        log("portal", f"Start returned: devices={started.get('devices')} streams={started.get('streams')}")
    if started.get("restore_token"):
        TOKEN_FILE.parent.mkdir(parents=True, exist_ok=True)
        TOKEN_FILE.write_text(started["restore_token"])
        log("portal", f"saved restore token to {TOKEN_FILE}")
    results["session"] = bool(started.get("streams")) and bool(started.get("devices", 0) & DEVICE_TOUCHSCREEN)

    node_id = started["streams"][0][0]
    fd = portal.open_pipewire_remote()
    caps = "video/x-raw" if args.no_size_caps else f"video/x-raw,width={width},height={height}"
    sink = "videoconvert ! autovideosink sync=false" if args.preview else "fakesink name=sink sync=false"
    desc = f"pipewiresrc name=src fd={fd} path={node_id} do-timestamp=true keepalive-time=1000 ! {caps} ! {sink}"
    log("gst", desc)
    pipeline = Gst.parse_launch(desc)

    frames = {"n": 0, "caps": None}

    def probe(pad, info):
        frames["n"] += 1
        if frames["caps"] is None:
            frames["caps"] = pad.get_current_caps().to_string()
        return Gst.PadProbeReturn.OK

    src = pipeline.get_by_name("src")
    src.get_static_pad("src").add_probe(Gst.PadProbeType.BUFFER, probe)
    pipeline.set_state(Gst.State.PLAYING)

    gst_bus = pipeline.get_bus()

    def pump(seconds):
        """Run the GLib loop and watch the pipeline bus for errors for a while."""
        end = time.monotonic() + seconds
        ctx = GLib.MainContext.default()
        while time.monotonic() < end:
            while ctx.pending():
                ctx.iteration(False)
            msg = gst_bus.timed_pop_filtered(50 * Gst.MSECOND, Gst.MessageType.ERROR)
            if msg:
                err, dbg = msg.parse_error()
                raise RuntimeError(f"pipeline error: {err.message}\n{dbg}")

    pump(3)
    log("gst", f"frames in first 3s: {frames['n']}; negotiated caps: {frames['caps']}")
    results["frames"] = frames["n"] > 0

    new = [m for m in mutter_monitors(portal.bus) if m[0] not in before]
    for m in new:
        log("mutter", "new monitor: connector={} size={}x{} at ({},{}) scale={}".format(*m))
    results["monitor_added"] = bool(new)
    results["monitor_size"] = any((m[1], m[2]) == (width, height) for m in new)

    try:
        if not args.no_touch_test and results["monitor_size"]:
            results["touch"] = touch_test(portal, node_id, width, height, pump)
        elif not args.no_touch_test:
            log("touch", "skipped: no monitor of the requested size to target")

        log("hold", f"keeping the virtual monitor for {args.hold}s (check Settings → Displays)")
        pump(args.hold)
    except (RuntimeError, GLib.Error) as e:
        log("error", str(e))
        results["no_errors"] = False

    pipeline.set_state(Gst.State.NULL)
    portal.close()
    time.sleep(1)
    gone = not [m for m in mutter_monitors(portal.bus) if m[0] not in before]
    log("mutter", f"virtual monitor removed after close: {gone}")
    results["teardown"] = gone

    print("\nRESULTS " + json.dumps(results))
    return 0 if all(results.values()) else 1


def touch_test(portal, node_id, width, height, pump):
    target = subprocess.Popen(["/usr/bin/python3", str(HERE / "touch_target.py"), str(width), str(height)],
                              stdout=subprocess.PIPE, text=True)
    events, ready = [], threading.Event()

    def reader():
        for line in target.stdout:
            ev = json.loads(line)
            events.append(ev)
            if ev["event"] in ("ready", "error"):
                ready.set()

    threading.Thread(target=reader, daemon=True).start()
    pump(0)
    deadline = time.monotonic() + 10
    while not ready.is_set() and time.monotonic() < deadline:
        pump(0.2)
    status = next((e for e in events if e["event"] in ("ready", "error")), None)
    log("touch", f"target: {status}")
    if not status or status["event"] != "ready":
        target.terminate()
        return False

    # The first touch on a freshly mapped window can be swallowed, so send an
    # unchecked warm-up tap first.
    warmup = (0.1, 0.9)
    points = [(0.5, 0.5), (0.25, 0.25), (0.75, 0.8)]
    try:
        for slot, (fx, fy) in enumerate([warmup, *points]):
            x, y = fx * width, fy * height
            portal.touch("NotifyTouchDown", node_id, slot, x, y)
            pump(0.05)
            portal.touch("NotifyTouchMotion", node_id, slot, x + 1, y + 1)
            pump(0.05)
            portal.touch("NotifyTouchUp", slot)
            pump(0.3)
        pump(0.5)
    finally:
        target.terminate()
        target.wait()

    begins = [e for e in events if e["event"] in ("touch_begin", "button_press")]
    log("touch", f"received presses: {[(e['event'], e['x'], e['y']) for e in begins]}")
    ok = True
    for fx, fy in points:
        exp = (fx * width, fy * height)
        hit = next((e for e in begins if abs(e["x"] - exp[0]) <= 2 and abs(e["y"] - exp[1]) <= 2), None)
        ok &= hit is not None
        log("touch", f"sent ({exp[0]:.0f},{exp[1]:.0f}) → "
                     f"{'got ' + hit['event'] + ' OK' if hit else 'NOT RECEIVED'}")
    return ok


if __name__ == "__main__":
    try:
        sys.exit(main())
    except RuntimeError as e:
        log("error", str(e))
        sys.exit(2)

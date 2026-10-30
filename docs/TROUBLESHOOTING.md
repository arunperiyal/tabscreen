# Troubleshooting

Start with the two built-in diagnostics:

- **The page's status line** (top-left on the tablet): resolution, fps, bitrate,
  round-trip time (`rtt`) and decode time per frame.
- **The host's log**: connections, `hello` sizes, session start/stop with the
  connector name and scale, and every 5 s `capture:` (frames from PipeWire, copy
  time) and `encoder:` (frames in/out, bitrate, encode latency) lines.

## Connecting

**The page says "WebCodecs unavailable"**
Chrome only decodes video on secure pages. Open `http://localhost:8765` (USB) or
the `https://…` URL printed by `--wifi`. Plain `http://<computer-ip>:8765` won't
work.

**The page says "cannot connect: use the exact URL (with #t=…)"**
The Wi‑Fi token is missing or wrong (the host logs `rejected …: missing or wrong
token`). Copy the full URL from the host's output, or scan the QR code. If you
deleted `~/.config/tabscreen/token`, a new one was issued — the old bookmark no
longer works.

**USB: the page doesn't load on the tablet**
- `adb devices` must list the tablet as `device` (not `unauthorized`: accept the
  prompt on the tablet).
- Start the host with `--usb`; `adb reverse --list` should show
  `tcp:8765 tcp:8765`.
- With several devices connected, the host forwards the port on each.

**Wi‑Fi: the page doesn't load on the tablet**
- Both devices must be on the same network (guest networks often isolate clients).
- A firewall may block the port. With ufw:
  `sudo ufw allow from 192.168.0.0/24 to any port 8766 proto tcp` (use your subnet).
- The printed address is the one on the default route. If you have VPNs or
  several networks, try the computer's other addresses.

**Certificate warning every time / "Your connection is not private"**
Expected once: the certificate is self-signed. Tap **Advanced → Proceed**. If it
comes back, the certificate was regenerated (the computer's IP changed, or
`~/.config/tabscreen/cert.pem` was deleted).

**"Address already in use"**
Another `tabscreen-host` is running (`pgrep -a tabscreen-host`). Stop it or use
`--port` / `--wifi-port`.

## Display

**No new monitor appears**
The monitor is created when a tablet connects, not at startup. Check the host
log for `server: hello` and `session: virtual monitor … active`. TabScreen needs
GNOME on Wayland; it doesn't work on X11 or other desktops.

**Everything on the tablet is tiny**
Set a scale for the tablet's monitor in **Settings → Displays** (200 % suits
high-resolution tablets). Touch stays accurate at any scale.

**The monitor is in the wrong place**
Drag it in **Settings → Displays**. GNOME usually remembers the arrangement.

**Rotating the tablet does nothing**
Chrome must allow rotation (system auto-rotate on). The client re-sends its size
on rotation and the host logs a new `hello`; the monitor is then recreated in the
new orientation — but only if this is the only client. With other viewers
connected the size is kept (`keeping WxH for N clients` in the log) and the video
is letterboxed instead.

**A viewer's screen shows black bars**
Expected with several viewers: the monitor has the first viewer's shape, and
other screens fit it inside theirs.

**The monitor stays for a few seconds after disconnecting**
By design: it's removed 10 s after the last tablet leaves, so reloads don't move
your windows.

**Mirror: viewers see "waiting for video"**
A mirrored monitor only sends frames when its content changes. The mirror runs
from startup, so this only happens until the screen first changes after the host
starts — change the slide or move a window on that screen.

**Mirror: "no active monitor" warning, or the stream doesn't start**
Check the connector name with `tabscreen-host --list-monitors`; the monitor must
be connected and enabled. If the projector is plugged in later, the stream starts
when the next viewer connects.

**A link opens the wrong stream, or "missing or wrong token"**
Each stream has its own link and token; the audience link can't open the tablet
stream. Use the exact URL printed for that stream (`/s/<name>/#t=…`).

## Touch and cursor

**Touch does nothing**
The host may be running with `--no-touch` (its startup line says
`view only (touch off)`).

**Touches land in the wrong place**
Check the host log for `display: Meta-N scale …` after you change the scale. If
you use `--size`, the tablet's aspect ratio may differ from the monitor's; the
page letterboxes the video and maps touches inside the video area only.

**No mouse cursor on the tablet**
The cursor is drawn by the page and is only shown while the pointer is on the
tablet's monitor. Move the mouse onto it (past the edge you arranged in Settings).

## Performance

**Laggy**
1. Prefer USB or direct Wi‑Fi (`--wifi`) over *wireless adb*, which tunnels the
   stream through adb and adds noticeable delay.
2. Look at `rtt` on the status line: high or jumpy means the network.
3. Look at `decode`: high means the tablet's decoder; try a lower `--bitrate`.
4. Look at the host's `encoder:` line: latency above ~20 ms or `fps in` below
   your screen activity means the computer is struggling; with x264, try
   `--encoder nvenc` if you have an NVIDIA GPU.

**One viewer (e.g. a phone) lags further and further behind, especially during
animations**
Known issue (see PLAN, *M2b*): the host doesn't yet drop frames for a viewer whose
connection can't keep up, so video queues up and the delay grows. Check `rtt` on
that device's status line — hundreds of ms during animations means queueing. Until
per-client flow control lands, lower `--bitrate` (e.g. `6000`), move the device
closer to the router or onto 5 GHz Wi‑Fi, and prefer USB for the presenter's
tablet.

**Low fps when the screen is busy**
Frames are only sent when something changes, so a static screen shows low fps —
that's normal. During constant motion, compare `capture:` fps with `encoder:`
fps in the host log to see where frames are lost.

**Blocky video in fast motion**
Raise `--bitrate` (default 12000 kbit/s); USB and good Wi‑Fi handle 20–30 Mbit/s.

## Building

**`pkg-config` can't find `libpipewire-0.3`, or linking fails with
`unable to find library -lpipewire-0.3`**
Install the build dependencies (see [DEVELOPMENT.md](DEVELOPMENT.md)). If you
use conda/miniconda, its `pkg-config` and `cc` shadow the system ones;
`.cargo/config.toml` pins `/usr/bin/pkg-config` and `/usr/bin/cc` — build from
inside the repository so it applies.

**`no element "x264enc"` / `"h264parse"`**
Install `gstreamer1.0-plugins-ugly` (x264enc) and `gstreamer1.0-plugins-bad`
(h264parse, nvh264enc). With conda, check with `/usr/bin/gst-inspect-1.0`, not
conda's copy.

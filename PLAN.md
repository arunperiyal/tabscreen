# TabScreen — Plan

Use an Android tablet as a **real secondary monitor with touch input** for a
Linux (GNOME / Wayland) desktop. The tablet *extends* the desktop — it is not a
mirror.

## Goals

- A new virtual monitor appears in GNOME (Settings → Displays) while a tablet is
  connected, sized to the tablet's resolution, and disappears on disconnect.
- Low-latency video of that monitor on the tablet (target < 60 ms over USB,
  < 100 ms over Wi‑Fi).
- Multi-touch on the tablet maps to touch events on that monitor.
- Works over USB (`adb reverse`) and over Wi‑Fi on the same LAN.
- No root, no kernel modules, no compositor patches — GNOME's own D-Bus APIs only.

## Non-goals (for now)

- Pen pressure / tilt (Mutter touch has no pressure; see *Stretch*).
- Audio, file transfer, clipboard (GSConnect covers these).
- X11, KDE, wlroots compositors. Design leaves room for them, but GNOME first.
- Multiple simultaneous tablets.

## Why this is feasible

Verified on the dev machine (Ubuntu 26.04, GNOME Shell 50.1, Wayland):

| Capability | Source | Status |
|---|---|---|
| Virtual monitor capture | `org.gnome.Mutter.ScreenCast` `Session.RecordVirtual` (portal also lists **virtual**, source types `7`) | ✅ M0 |
| Touch injection | `org.gnome.Mutter.RemoteDesktop` `Session.NotifyTouch*` (`SupportedDeviceTypes` `7` incl. **touchscreen**) | ✅ M0 |
| Capture into GStreamer | `pipewiresrc` | present |
| H.264 encode | `nvh264enc` (NVIDIA T600), `x264enc`, `openh264enc` | present |
| H.264 encode (Intel VA) | `vah264enc` / `vah264lpenc` | **missing** — optional, see Setup |

Deskreen and similar tools can't do this: Electron's `desktopCapturer` only
captures existing outputs, so they can only mirror.

## Architecture

```
 Linux host (tabscreen-host, Python)                     Tablet
 ───────────────────────────────────                     ──────
 Mutter RemoteDesktop session
   ├─ ScreenCast Session.RecordVirtual ────────┐
   └─ touch injection on the same session      │
                                               ▼
 PipeWire node ─► pipewiresrc ─► encoder (nvh264enc | x264enc)
                                   │  H.264 Annex B, no B-frames
                                   ▼
                          WebSocket server ═══ USB (adb reverse) / Wi‑Fi ═══► client
                                   ▲                                         decode → fullscreen
 RD NotifyTouch* ◄─────────────┘◄──────────── touch events ◄──────────── MotionEvent / PointerEvent
```

### Host (`host/`, Python 3 + PyGObject + GStreamer)

1. **Mutter session** (`mutter.py`, `Gio` D-Bus calls — see `scripts/spike_mutter.py`).
   *M0 showed the portal can't inject touch on virtual streams; see M0 results.*
   - `org.gnome.Mutter.RemoteDesktop.CreateSession()` → RD session; read its `SessionId`
   - `org.gnome.Mutter.ScreenCast.CreateSession({remote-desktop-session-id})` →
     `Session.RecordVirtual({cursor-mode: 1 (embedded)})` → stream object path
   - Subscribe to `Stream.PipeWireStreamAdded(u node_id)`, then `RD Session.Start()`
   - No permission dialog and no restore token. Mutter ends the session when
     the D-Bus connection or the stream consumer goes away.
2. **Pipeline** (`pipeline.py`)
   - `pipewiresrc path=<node_id> do-timestamp=true keepalive-time=1000`
     `! video/x-raw,width=W,height=H,max-framerate=60/1`
     `! videoconvert ! <encoder> ! h264parse config-interval=-1`
     `! video/x-h264,stream-format=byte-stream,alignment=au ! appsink`
   - The caps `width/height` set the virtual monitor size (confirmed in M0).
   - Encoder presets: low latency, no B-frames, constrained-baseline or main
     profile, CBR/VBR around 8–15 Mbit/s, IDR on demand.
     - `nvh264enc preset=low-latency-hq zerolatency=true bframes=0 gop-size=-1`
     - `x264enc tune=zerolatency speed-preset=ultrafast key-int-max=600 bframes=0`
   - Force a keyframe (`GstForceKeyUnit` event) when a client connects or asks for one.
   - `keepalive-time` makes sure frames keep coming when nothing on screen changes.
     Mutter only sends frames when the screen changes.
3. **Server** (`server.py`, `aiohttp` on GLib loop or a thread)
   - `GET /` serves the web client (M1/M2).
   - `WS /ws` handles the protocol below. One active client at a time.
4. **Input** (`input.py`)
   - Map normalized touch `(x, y) ∈ [0,1]` to monitor pixels `(x·W, y·H)` →
     RD `Session.NotifyTouchDown(stream_path, slot, x, y)`,
     `NotifyTouchMotion(stream_path, slot, x, y)`, `NotifyTouchUp(slot)`.
   - Slot = Android pointer id. Track active slots; release all on disconnect.
5. **CLI** (`__main__.py`): `tabscreen-host [--port 8765] [--encoder auto|nvenc|x264]
   [--size 2000x1200] [--usb]`. `--usb` runs `adb reverse tcp:8765 tcp:8765`.

### Clients

- **Phase 1 — web client** (`web/`): plain HTML/JS. WebCodecs `VideoDecoder`
  (H.264) → `<canvas>` fullscreen; Pointer Events → touch messages. Needs no
  Android build, so we can test the host end to end first.
  - WebCodecs needs a *secure context*: `http://localhost` is fine (USB via
    `adb reverse`). Wi‑Fi needs HTTPS → host generates a self-signed cert (M2).
- **Phase 2 — Android app** (`android/`, Kotlin): `MediaCodec` (H.264, low-latency
  flag on API 30+) decoding into a `SurfaceView`; immersive fullscreen; keep
  screen on; `MotionEvent` → touch messages; OkHttp WebSocket. Same protocol,
  no TLS needed.

### Protocol (WebSocket, binary)

All messages: 1-byte type + payload. Little-endian.

| Dir | Type | Name | Payload |
|---|---|---|---|
| C→S | `0x01` | hello | UTF-8 JSON `{"w":2000,"h":1200,"dpi":240,"codecs":["h264"],"version":1}` |
| S→C | `0x02` | config | UTF-8 JSON `{"w":2000,"h":1200,"codec":"avc1.42E01F"}` |
| S→C | `0x03` | frame | `u8 flags` (bit0 = keyframe) · `u64 pts_us` · Annex B access unit |
| C→S | `0x04` | touch | `u8 action` (0 down, 1 move, 2 up, 3 cancel) · `u8 slot` · `f32 x` · `f32 y` |
| C→S | `0x05` | keyframe request | — |
| both | `0x06` | ping/pong | `u64 t_us` (latency measurement) |

`move` messages may batch: repeat `slot,x,y` triples after the action byte.
Full spec goes in `docs/protocol.md` once M1 settles it.

## Repository layout

```
tabscreen/
├── PLAN.md
├── README.md
├── docs/protocol.md
├── host/
│   ├── pyproject.toml
│   └── tabscreen_host/{__main__,mutter,pipeline,server,input}.py
├── web/{index.html,client.js}
├── android/            # Gradle project (Kotlin), added in M3
└── scripts/            # spikes, setup helpers
```

## Milestones

### M0 — Spike: prove the platform ✅ (2026-10-29)

Scripts: `scripts/spike_virtual.py` (portal), `scripts/spike_mutter.py` (Mutter API),
`scripts/touch_target.py` (fullscreen GTK4 window on the virtual monitor that logs touches).

| Check | Portal path | Mutter API path |
|---|---|---|
| Virtual monitor appears (`Meta-0`, right of existing outputs) | ✅ | ✅ |
| Caps `width/height` set its size (1600×1000, 2560×1600) | ✅ | ✅ |
| Frames arrive (`BGRx`, damage-driven, max 60 fps) | ✅ | ✅ |
| Touch lands at exact pixel coordinates | ❌ `Invalid position` | ✅ (3/3, repeated runs) |
| Monitor removed on stop | ✅ | ✅ |
| Permission dialog | every run (see below) | none |

Findings:
- **Portal touch is unusable for virtual sources.** `xdg-desktop-portal` (1.21.1)
  checks absolute positions against the stream size, and virtual streams report no
  `size`, so every position is rejected.
- **Portal restore tokens drop touch.** A restored session comes back with
  `devices=0` (screen only), so the dialog would be needed on every run anyway.
- **Decision: the host uses Mutter's D-Bus API** (`org.gnome.Mutter.RemoteDesktop`
  + `org.gnome.Mutter.ScreenCast.RecordVirtual`), like gnome-remote-desktop does.
  Trade-offs: GNOME-only private API (may change between GNOME releases), and no
  user consent prompt. The host must make it obvious when a session is active
  (log line now, indicator/notification in M4).
- The first touch on a just-mapped window can be dropped; harmless for real use.

### M1 — Host + web client over USB (1–2 days)
- [ ] Host package with mutter, pipeline, server, input modules.
- [ ] Web client: decode, render, send touch.
- [ ] `--usb` sets up `adb reverse`; open `http://localhost:8765` in tablet Chrome.
- [ ] Keyframe on connect; clean teardown (monitor disappears) on disconnect / Ctrl‑C.

**Exit:** drag a window to the tablet and use it with touch.

### M2 — Robustness + Wi‑Fi (1 day)
- [ ] Size the monitor from the client's `hello` (restart the pipeline if the size changes).
- [ ] Self-signed HTTPS for the web client over Wi‑Fi; show URL + QR in the terminal.
- [ ] Reconnect handling, ping/latency stats, encoder auto-detect and fallback.
- [ ] HiDPI: let the user set scale for the virtual monitor in GNOME; check touch mapping still uses stream coordinates.

### M3 — Native Android app (2–3 days)
- [ ] Gradle project (JDK 17, Kotlin, minSdk 26).
- [ ] Connect screen (host:port, remembers last; "USB" shortcut = `localhost`).
- [ ] MediaCodec decoder → SurfaceView, immersive mode, keep screen on.
- [ ] Multi-touch → protocol; handle rotation (send new `hello`).
- [ ] Compare latency against the web client.

### M4 — Polish
- [ ] mDNS (`_tabscreen._tcp`) discovery so the app finds the host automatically.
- [ ] `systemd --user` service; optional GNOME Shell indicator or tray icon.
- [ ] Auto `adb reverse` when a USB device is plugged in (udev/adb watcher).
- [ ] Packaging: `pipx`-installable host, APK release build.

### Stretch
- Pen pressure/tilt: separate `uinput` tablet device. Mapping it to the virtual
  output on GNOME is the hard part (no public API for virtual outputs).
- Hardware-cursor mode (`cursor_mode=metadata`), with the cursor drawn client-side to cut latency.
- H.265 / AV1 when both ends support it.
- Other compositors: KDE (`kwin` virtual outputs), wlroots (`wlr-output-management` + headless outputs).

## Risks & open questions

| Risk | Mitigation |
|---|---|
| Mutter's private D-Bus API changes in a future GNOME | Keep it isolated in `mutter.py`; spike scripts double as a regression check after upgrades |
| Touch coordinates off with fractional scaling | Test at 100 % / 150 % / 200 % in M2; stream coords are logical px |
| No frames when screen is static → client stalls on join | `keepalive-time` + forced IDR on connect |
| NVIDIA hybrid graphics: DMA‑BUF import / `nvh264enc` quirks | Start with `videoconvert ! x264enc` (CPU) path; add GPU path after it works |
| WebCodecs unavailable over plain HTTP on LAN | USB/localhost first; HTTPS in M2; native app in M3 |

## Dev environment notes

- **Use system Python and GStreamer**, not conda: `miniconda3/bin` is earlier on
  `PATH` and shadows `python3` (no `gi`) and `gst-inspect-1.0` (no plugins).
  Use `/usr/bin/python3`, and a venv created with `--system-site-packages` for PyGObject.
- Optional Intel VA encoder: `sudo apt install intel-media-va-driver-non-free`
  (then check `/usr/bin/gst-inspect-1.0 vah264enc`).
- Android: SDK at `~/Android/Sdk`, `adb` on PATH, use JDK 17
  (`/usr/lib/jvm/java-17-openjdk-amd64`) for Gradle; default `java` is 11.

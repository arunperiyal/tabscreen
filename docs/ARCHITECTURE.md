# Architecture

TabScreen has two parts: **`tabscreen-host`**, a Rust program on the Linux
desktop, and a **web client** that runs in Chrome on the tablet (embedded in the
host binary). They talk over one WebSocket.

```
 Linux host (tabscreen-host)                                          Tablet (Chrome)
 ───────────────────────────                                          ───────────────
 Mutter RemoteDesktop session ──┐
   └─ ScreenCast RecordVirtual   │ virtual monitor (Meta-N)
      (cursor-mode = metadata)   ▼
 PipeWire stream ──► capture ──► frames (BGRx) ──► encoder (NVENC / x264) ──┐
                         │                                                  │ H.264 Annex B
                         └──► cursor position + image (60 Hz) ──────────────┤
                                                                            ▼
                                       server (axum WebSocket) ═══ USB / Wi‑Fi ═══► decode (WebCodecs)
                                            ▲                                       + draw cursor
 Mutter NotifyTouch* ◄── input ◄────────────┘◄──────────── touch events ◄────────── pointer events
```

## Components

| Module | Role |
|---|---|
| `main.rs` | CLI, starts the HTTP (USB) and optional HTTPS (Wi‑Fi) servers |
| `session.rs` | Owns the current virtual monitor and everything tied to it; creates, resizes and removes it |
| `mutter.rs` | Mutter D-Bus session: `RemoteDesktop.CreateSession` + `ScreenCast.RecordVirtual`, touch injection |
| `display.rs` | Mutter `DisplayConfig`: finds our connector (`Meta-N`) and tracks its scale |
| `capture.rs` | PipeWire consumer: frames with pixels → encoder; `SPA_META_Cursor` → cursor channels |
| `encoder.rs` | GStreamer pipeline `appsrc ! … ! h264parse ! appsink`; keyframes on demand; stats |
| `input.rs` | Touch injection thread: normalized coordinates → logical pixels → Mutter |
| `server.rs` | Serves the web client, speaks the WebSocket protocol, checks the Wi‑Fi token |
| `usb.rs` | Keeps `adb reverse` set up for every connected device |
| `wifi.rs` | Self-signed certificate, access token, LAN address, QR code |
| `web/client.js` | Decoder setup and fallback, cursor overlay, touch, reconnect, status line |

### Threads

- **tokio runtime**: HTTP/WebSocket servers, one task per client.
- **pipewire** (per session): PipeWire main loop; copies frames out of shared
  memory and reads cursor metadata.
- **GStreamer** streaming threads: conversion/encoding; the appsink callback
  pushes encoded frames onto a broadcast channel.
- **input** (per session): blocking D-Bus touch calls, so a slow call never
  stalls the server.
- **adb** (with `--usb`): polls `adb devices` every 2 s.
- **scale-watch** (per session): waits on `DisplayConfig.MonitorsChanged`.

Encoded frames go to clients through a `tokio::sync::broadcast` channel (8
frames). A client that falls behind skips to the next keyframe instead of
queueing stale video. Cursor state uses `watch` channels: only the latest
position and image matter.

## Session lifecycle

1. A client connects and sends `hello` with its screen size in device pixels.
2. If no monitor exists, or it has another size, the host creates one:
   Mutter session → PipeWire stream → the consumer negotiates a **fixed size**,
   which is what sets the virtual monitor's resolution.
3. The host finds the new connector in `DisplayConfig` and watches its scale.
4. Every client gets `config` and a keyframe. A rotated tablet sends a new
   `hello` and the monitor is recreated in the new orientation.
5. When the last client leaves, the monitor is removed after **10 s**, so a page
   reload or a quick replug doesn't shuffle windows around.

Only one virtual monitor exists at a time. Several clients can watch it: the
encoded stream is shared, so extra viewers cost bandwidth but no encoding. The
first client's `hello` decides the size; while more than one client is
connected, a `hello` with another size is ignored (the host logs `keeping WxH
for N clients`) so viewers don't resize each other's monitor. A lone client may
still resize it (rotation).

## Protocol

WebSocket at `/ws`, binary messages: 1 type byte + payload, little-endian.

| Dir | Type | Name | Payload |
|---|---|---|---|
| C→S | `0x01` | hello | UTF-8 JSON `{"w":2944,"h":1840,"dpr":2,"codecs":["h264"],"version":1}` — screen in device pixels. First message; sent again on rotation |
| S→C | `0x02` | config | UTF-8 JSON `{"w":2944,"h":1840,"touch":true}` — sent when a session (re)starts; the client resets its decoder. `touch: false` (`--no-touch`): the client sends no touches and shows the viewer's own pointer |
| S→C | `0x03` | frame | `u8 flags` (bit 0 = keyframe) · `u64 pts_us` · H.264 Annex B access unit (SPS/PPS before every keyframe) |
| C→S | `0x04` | touch | `u8 action` (0 down, 1 move, 2 up, 3 cancel) · `u8 slot` · `f32 x` · `f32 y`, `x, y` normalized to `[0, 1]`; ignored with `--no-touch` |
| C→S | `0x05` | keyframe request | — |
| both | `0x06` | ping / pong | `u64 t_us`; the host echoes it back (round-trip time) |
| S→C | `0x07` | cursor move | `i32 x` · `i32 y` in device pixels; outside the monitor = hidden |
| S→C | `0x08` | cursor image | `u16 w` · `u16 h` · `u16 hot_x` · `u16 hot_y` · RGBA pixels; `w = 0` = hidden |

Over Wi‑Fi the WebSocket URL carries the token: `/ws?t=<token>`.

## Video path

- **Capture**: PipeWire shared-memory buffers (BGRx), copied into a tightly
  packed `gst::Buffer` (~3 ms at 2944×1840). Mutter sends frames only on damage,
  so a static screen produces none.
- **Encoding** (`--encoder auto` picks NVENC if the `nvh264enc` element exists):
  - NVENC: `nvh264enc preset=p1 tune=ultra-low-latency zerolatency=true bframes=0`,
    fed BGRx directly (colour conversion on the GPU).
  - x264: `videoconvert ! x264enc tune=zerolatency speed-preset=ultrafast
    bframes=0 sliced-threads=true`.
  - Both: constrained-baseline profile, `h264parse config-interval=-1`, byte
    stream, one access unit per buffer. `appsrc` holds at most 2 frames and
    drops the oldest if the encoder falls behind.
- **Keyframes**: a new client, a decoder reset or a lag needs a keyframe even
  when nothing changes on screen. The host keeps the last raw frame and
  re-encodes it with a forced key unit. Requests are throttled (host 250 ms,
  client 500 ms).
- **Decoding**: WebCodecs `VideoDecoder`, `optimizeForLatency`, hardware
  preferred with software fallback (checked with `isConfigSupported`); frames
  are drawn on a `<canvas>` scaled to fit.

## Cursor

The cursor is not part of the video. Mutter runs in `cursor-mode = metadata`
and attaches `SPA_META_Cursor` (position, and the bitmap when it changes) to
buffers, including buffers with no pixels when only the pointer moves. The host
forwards these at ~60 Hz, and the client draws the cursor on a second canvas.
Cursor movement therefore never waits for video encoding.

## Touch and display scaling

The client sends normalized coordinates. Mutter's `NotifyTouch*` expects
**logical** pixels of the stream, so the host converts with
`x · (width − 1) / scale`, using the monitor's current scale from
`DisplayConfig` (updated live on `MonitorsChanged`). Cursor metadata, in
contrast, is in device pixels, matching the video.

## Connections and security

| | USB | Wi‑Fi (`--wifi`) |
|---|---|---|
| Listener | HTTP on `127.0.0.1:8765` (`--bind`, `--port`) | HTTPS on `[::]:8766` (`--wifi-port`) |
| Path | Tablet `localhost:8765` → `adb reverse` → host | Direct TCP over the LAN |
| Secure context for WebCodecs | `localhost` counts as secure | HTTPS with a self-signed certificate |
| Access control | Only local connections reach the port | 128-bit token required from every non-loopback client |

- The token travels in the page URL's fragment (`#t=…`, never sent with the page
  request) and in the WebSocket query. Wrong or missing token → HTTP 401.
- Certificate (names: `localhost`, `127.0.0.1`, LAN IP) and token are stored in
  `~/.config/tabscreen/` with mode 0600; the certificate is regenerated when the
  LAN IP changes.
- Every client with access can also touch, unless the host runs with
  `--no-touch`; then the URL only grants viewing.
- Mutter's D-Bus API shows no consent dialog; anyone who can run programs in your
  session could do the same. The host logs every connection and session.

## Design decisions

| Decision | Why |
|---|---|
| Mutter D-Bus API, not the xdg-desktop-portal | The portal rejects every touch on virtual streams ("Invalid position": it checks against a stream size virtual streams don't report), and restored portal sessions drop touch permission |
| Rust with its own PipeWire consumer, not Python + `pipewiresrc` | Pointer-only movement produces no frames in embedded cursor mode, and `pipewiresrc` drops the cursor-only buffers of metadata mode — the cursor updated once a second |
| Client-drawn cursor | 60 Hz cursor independent of video latency |
| `60/1` framerate in the encoder caps | With a variable rate (`0/1`) x264 held back one frame, so the last change before the screen went static never arrived |
| Monitor sized by the client's `hello` | No configuration; follows rotation; one binary for any tablet |
| Web client first, native app later | Proved the whole path without an Android build |

Detailed measurements and the history behind these decisions are in
[PLAN.md](PLAN.md).

## Performance

Measured at 2944×1840 with a 60 fps test window (Intel i9-11950H, NVIDIA T600):

| Stage | NVENC | x264 |
|---|---|---|
| Frames from PipeWire | 59–60 fps | 57–60 fps |
| Copy out of shared memory | 3.3 ms | 3.3 ms |
| Convert + encode | ~14 ms | ~8.5 ms |
| Host CPU | ~80 % | ~240 % |
| Bitrate | ~13 Mbit/s | ~12 Mbit/s |

The host adds roughly 12–17 ms; transport and the tablet's decoder add the
rest. The page's status line shows fps, bitrate, round-trip time and decode
time; the host logs capture and encoder statistics every 5 s.

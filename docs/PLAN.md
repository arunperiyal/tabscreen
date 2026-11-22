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
- (Several tablets as separate monitors: possible now with several `virtual` streams; untested.)

## Why this is feasible

Verified on the dev machine (Ubuntu 26.04, GNOME Shell 50.1, Wayland):

| Capability | Source | Status |
|---|---|---|
| Virtual monitor capture | `org.gnome.Mutter.ScreenCast` `Session.RecordVirtual` (portal also lists **virtual**, source types `7`) | ✅ M0 |
| Touch injection | `org.gnome.Mutter.RemoteDesktop` `Session.NotifyTouch*` (`SupportedDeviceTypes` `7` incl. **touchscreen**) | ✅ M0 |
| Frames + cursor metadata from PipeWire | pipewire-rs consumer (`host/examples/cursor_spike.rs`) | ✅ M0 |
| H.264 encode | `nvh264enc` (NVIDIA T600), `x264enc`, `openh264enc` | present |
| H.264 encode (Intel VA) | `vah264enc` / `vah264lpenc` | missing — optional |

Deskreen and similar tools can't do this: Electron's `desktopCapturer` only
captures existing outputs, so they can only mirror.

## Design

How the system works today — components, protocol, security model and the
design decisions — is in [ARCHITECTURE.md](ARCHITECTURE.md). Building and
testing: [DEVELOPMENT.md](DEVELOPMENT.md). This file tracks the roadmap and the
findings behind each step.

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

**Follow-up: slow cursor on the virtual monitor** (found while testing `--preview`):

| Cursor mode / consumer | Frames while only the pointer moves | Cursor updates |
|---|---|---|
| embedded, `pipewiresrc` | 1/s (keep-alive only) | baked into those frames |
| metadata, `pipewiresrc` | 1/s; ~100/s cursor-only buffers dropped as empty | none |
| metadata, own PipeWire consumer (Rust) | only on real damage | **60/s** position + bitmap (24×24 RGBA, hotspot) |

- **Decision: Rust host with its own PipeWire consumer, cursor-mode metadata,
  cursor drawn by the client.** Cursor latency no longer depends on video encoding.

### M1 — Host + web client over USB ✅ (2026-10-30)
- [x] Rust host: `mutter`, `capture`, `encoder`, `input`, `server` modules.
- [x] Web client: WebCodecs decode (hardware, falling back to software), cursor
      overlay, touch → protocol, reconnect, fullscreen + wake lock on first tap.
- [x] Keyframe on connect even when the screen is static (last frame re-encoded).
- [x] Clean teardown: Ctrl+C removes the virtual monitor.
- [x] `--usb` runs `adb reverse`.
- [x] On the tablet (Motorola XT2571, 2944×1840, Chrome): touch, mouse cursor and
      moving windows between displays work. Wireless adb lags noticeably; USB is
      better.

Verified on the dev machine (2026-10-30):
- `host/examples/ws_check.rs` (scripted client): config, cursor messages,
  first frame is a keyframe on a static screen, touches land at exact pixels.
- Desktop Chrome: 58 fps decoded at 1600×1000 under constant damage (software
  decoder), synthetic pointer events on the scaled canvas land at exact pixels.

Bugs found and fixed while testing:
- x264 held back one frame with a variable frame rate (`0/1`) in the caps, so
  the last change before the screen went static never arrived. Fixed with
  `60/1` caps.
- A decoder config error caused a keyframe request loop (54 Mbit/s of
  keyframes). Fixed by checking `isConfigSupported`, falling back to software
  decoding, and throttling keyframe requests (client 500 ms, host 250 ms).

**Exit:** drag a window to the tablet and use it with touch. ✅

**Latency budget at 2944×1840** (host, GL test window at 60 fps, 2026-10-30):

| Stage | x264 (`ultrafast`) | NVENC (T600, `p1`) |
|---|---|---|
| Frames from PipeWire | 57–60 fps | 59–60 fps |
| Copy out of PipeWire SHM | 3.3 ms | 3.3 ms |
| Convert + encode | 8.5 ms avg, 11 max | 13.8 ms avg, 16 max |
| Host CPU | ~240 % | ~80 % |
| Bitrate | ~12 Mbit/s | ~13 Mbit/s |

The host adds ~12–17 ms. The rest is transport and the tablet; the lag first
noticed came mostly from the *wireless adb* tunnel (USB and direct Wi‑Fi are
better). The client status line shows fps, Mbit/s, ping RTT and decode time.

### M2 — Robustness ⏳ (essentials done 2026-10-30)

Essentials:
- [x] **Monitor follows the client.** No monitor exists until a client sends
      `hello`; it's created at the client's screen size (device pixels, even),
      recreated when a hello asks for another size (rotation), and removed 10 s
      after the last client leaves. `--size` overrides. (`session.rs`)
- [x] **Scaling.** Mutter's `NotifyTouch*` take *logical* pixels, so at 200 % a
      touch aimed at the centre landed in the bottom-right corner. The host now
      finds its connector (`Meta-N`) in DisplayConfig, tracks its scale through
      `MonitorsChanged`, and divides by it. Verified at 100/125/167/200 %, with
      live changes. Cursor metadata stays in device pixels (checked at 200 %),
      so the client needs no change. (`display.rs`, `scripts/set_scale.py`)
- [x] **Encoder auto-select.** `--encoder auto` (default) picks NVENC when the
      nvcodec plugin is present; NVENC now takes BGRx directly (no CPU convert).
- [x] **USB replug.** `--usb` polls `adb devices` every 2 s and re-runs
      `adb reverse` for any device that (re)appears. (`usb.rs`)
- [ ] On the tablet: replug the cable; rotate the tablet; set the tablet's
      monitor to 150–200 % in Settings and check touch.

Later:
- [x] **Wi‑Fi without adb** (`--wifi`, 2026-10-30): HTTPS on `[::]:8766` with a
      self-signed certificate (rcgen; names: localhost, 127.0.0.1, LAN IP;
      regenerated when the IP changes) and a 128-bit token required from
      non-loopback clients (`?t=` on the WebSocket; the page URL carries it as
      `#t=…`). Both stored in `~/.config/tabscreen/` (0600). URL + QR printed at
      startup. Verified: 401 without/with a wrong token, 101 with it; headless
      Chrome decodes over HTTPS with the untrusted certificate.
- [x] On the tablet over Wi‑Fi: works and looks good (2026-10-30). ufw on the
      dev machine needed `allow … port 8766`.
- [x] **Several viewers / projection** (2026-10-30): the first client's `hello`
      sizes the monitor; while more clients are connected other sizes are ignored.
      `--no-touch` drops touch from all clients and tells them (`config.touch`).
      Connection counts in the log. Verified with `examples/multi_check.rs`.
- [x] **Mirror and several streams** (2026-10-30): `--mirror CONNECTOR`
      (`RecordMonitor`) and `--stream NAME=virtual|mirror:CONNECTOR[,view-only]
      [,size=WxH]`, each stream with its own manager, page (`/s/<name>/`), Wi‑Fi
      token and URL/QR. `--list-monitors`. Scenario: tablet = virtual monitor with
      touch, audience = projector mirror, view-only. Finding: Mutter sends **no
      initial frame** for a static mirrored monitor (0 frames in 3 s, also with
      `pipewiresrc`), so mirrors start with the host and stay running; the stored
      last frame serves late joiners. Verified: both streams concurrently, mirror
      at the monitor's size regardless of the client, token isolation (401 across
      streams), late joiner on a static screen gets a keyframe.
- [x] On the real setup (2026-10-30): tablet showing the presenter's notes
      (`tab=virtual`, touch), a 1920×1080 HDMI monitor as the projector, and a
      phone as audience (`audience=mirror:HDMI-1,view-only`), all over Wi‑Fi.
      Works. **The audience stream lags noticeably, especially during slide
      animations; the tablet stream is better.** See *M2b — Per-client flow control*.
      (`--usb` was not needed: it only maintains `adb reverse` for cable connections.)
- [x] **Reach viewers outside the LAN** (2026-11-22): `--public-host HOST[:PORT]`
      puts the router's public address (and external port) in the printed links and
      the certificate instead of the LAN IP. Finding: the router's WAN address on the
      dev network is `10.103.21.146`, a private address, so a port-forward there only
      reaches devices on the upstream network; real internet viewers need a public IP
      from the ISP, a forward on the upstream router, or a tunnel (Tailscale,
      Cloudflare Tunnel). Not yet tested from outside.
- [ ] Encoder fallback at runtime (NVENC present but failing → x264).
- [ ] Ping/latency stats in the host log.

### M2b — Per-client flow control (in progress)

**Problem.** Audience viewers lag far behind during slide animations, while the
tablet stream stays responsive.

**Likely cause.** The server never drops frames for a slow client: each frame is
awaited on `socket.send`, so when a viewer's link is slower than the video, frames
queue in the TCP send buffer (several MB = seconds of video) and latency grows
instead of the frame rate dropping. The broadcast channel only reports lag after
8 frames, and by then the backlog is already in the kernel. Animations make it
worse: a mirrored projector repaints most of the screen at 60 fps, so bitrate
spikes far above the presenter-notes stream, which is mostly static.
Contributing factors to rule out: phone on 2.4 GHz Wi‑Fi, software decoding on
the phone, encoder rate control letting bursts exceed `--bitrate`.

**Measure first** (during an animation, phone and tablet side by side):
- [ ] Phone and tablet status lines: fps, Mbit/s, `rtt`, decode. Pings share the
      TCP stream with video, so `rtt` rising to hundreds of ms confirms queueing.
- [ ] Host log: `encoder:` lines (Mbit/s during animations) and any
      `audience: client fell behind` lines.

**Fix, by branch** (a stack: each branch is based on the one before it, see
`.claude/rules/git-workflow.md`):

`m2b/frame-ack` (base: `main`) — the client reports what it received.
- [ ] Protocol: C→S `0x09` *frame ack* (`u64 pts_us`), sent by the client when a
      frame arrives (before decoding). Document it in `ARCHITECTURE.md`.
- [ ] Client sends the ack; host parses it per client and tracks bytes and frames
      in flight (sent − acked), with a test for the bookkeeping.

`m2b/skip-slow-client` (base: `m2b/frame-ack`) — slow viewers drop frames, not time.
- [ ] Above a budget (≈100 ms of video or 3 frames in flight), skip delta frames
      for *that client only*, then resume at the next keyframe (request one,
      throttled as today). Other clients are unaffected.
- [ ] Count skipped frames per client; log them and show them in the status line.

`m2b/rate-control` (base: `main`; independent of the two above, can run in parallel)
- [x] Constant bitrate with a small VBV buffer (150 ms) (`nvh264enc rc-mode=cbr
      vbv-buffer-size=…`, x264 `vbv-buf-capacity`) so animations can't burst far
      above `--bitrate`.
- [x] Per-stream `bitrate=KBPS` option, e.g. a lower bitrate for `audience`.

`m2b/retest` (base: the others merged)
- [ ] Re-test the real setup: phone latency during animations should stay close
      to the tablet's. Record the numbers here and tick M2b.

Later (if needed): adaptive bitrate per stream from the clients' ack timing;
smaller keyframes (intra refresh) so resyncs don't spike.

Workaround until then: `--bitrate 6000` lowers the peak data rate (softer image in motion).

### M2c — Why is it slower than AnyDesk? (analysis)

**Problem.** Side by side with AnyDesk on the same machine and network, TabScreen
feels laggier, in motion (slide animations, window drags) and in the audience view.
Goal: find out where the time goes and what AnyDesk does differently, then decide
what to adopt. Analysis first; no fixes in this task.

`m2c/measure-tabscreen` (base: `main`)
- [ ] Define the test: same machine, same Wi‑Fi, same content (a slide animation, a
      dragged window, scrolling text). Record screen-to-screen latency by filming a
      running millisecond clock on the host and the viewer with a phone at 240 fps.
- [ ] Measure TabScreen per stage: capture → encode → send → receive → decode →
      render, using the client status line (fps, Mbit/s, rtt, decode) plus
      timestamps added to the host log. Compare LAN Wi‑Fi, USB and `--public-host`.
- [ ] Check whether frames are skipped or delayed on the host (PipeWire damage
      pacing, 60 fps cap, the 1 fps keep-alive when static) and how the client
      presents frames (render on arrival, not on a vsync queue).

`m2c/measure-anydesk` (base: `main`)
- [ ] Measure AnyDesk in the same scenes: bitrate (router or `nethogs`), frame rate,
      and the latency from the filmed test; note its codec and settings (quality
      vs speed, resolution).

`m2c/compare` (base: both measurement branches; docs only)
- [ ] Compare the transport: TabScreen is WebSocket over TCP (head-of-line blocking,
      queues instead of dropping, see M2b); AnyDesk uses UDP with its own loss
      handling. Test TCP vs a lossy link (`tc netem`) to see how much this matters.
- [ ] Compare the video pipeline: rate control (CBR/VBV vs VBR bursts), GOP and
      keyframe size, B-frames and lookahead off, encoder latency tuning, resolution
      scaling under load, and software vs hardware decode in the browser.
- [ ] Write the findings here: a table of stage latencies, the largest contributors,
      and a ranked list of fixes (likely candidates: per-client flow control from
      M2b, CBR with a small buffer, lower-latency transport such as WebRTC or
      WebTransport, native decoder in M3).

### M3 — Native Android app (2–3 days)
- [ ] Gradle project (JDK 17, Kotlin, minSdk 26).
- [ ] Connect screen (host:port, remembers last; "USB" shortcut = `localhost`).
- [ ] MediaCodec decoder → SurfaceView, immersive mode, keep screen on.
- [ ] Multi-touch → protocol; handle rotation (send new `hello`).
- [ ] Compare latency against the web client.

### Release binary ✅ (2026-10-30; can be revisited in any phase)

A prebuilt `tabscreen-host` that runs without a Rust toolchain. The web client
is already embedded, so it is a single file plus system libraries.
- [x] `cargo build --release` with `strip = true`, `lto = "thin"` in `[profile.release]`.
- [x] Runtime deps (`ldd` + elements used): `libpipewire-0.3-0t64`, GLib,
      GStreamer core and `gstreamer1.0-plugins-base` (appsrc/appsink, videoconvert),
      `-bad` (`h264parse`, `nvh264enc`), `-ugly` (`x264enc`).
- [x] `.github/workflows/release.yml`: on `v*` tags, build on Ubuntu 24.04, check
      tag = Cargo version, smoke-test, attach `tabscreen-host-<version>-linux-x86_64.tar.gz`
      (binary, README, LICENSE, docs) and its SHA-256 to a GitHub Release. Manual runs
      only upload an artifact.
- [ ] Optional `.deb` (`cargo deb`) declaring the runtime packages above.
- [ ] Once M3 exists: signed release APK attached to the same release.

### M4 — Polish
- [ ] mDNS (`_tabscreen._tcp`) discovery so the app finds the host automatically.
- [ ] `systemd --user` service; optional GNOME Shell indicator or tray icon.
- [x] Auto `adb reverse` when a USB device is plugged in (done in M2: `usb.rs`).
- [ ] Packaging beyond the release binary: distro packages, Flatpak?

### Stretch
- Pen pressure/tilt: separate `uinput` tablet device. Mapping it to the virtual
  output on GNOME is the hard part (no public API for virtual outputs).
- DMA‑BUF import from PipeWire straight into the GPU encoder (no CPU copy).
- H.265 / AV1 when both ends support it.
- Other compositors: KDE (`kwin` virtual outputs), wlroots (`wlr-output-management` + headless outputs).

## Risks & open questions

| Risk | Mitigation |
|---|---|
| Mutter's private D-Bus API changes in a future GNOME | Keep it isolated in `mutter.rs`; spike scripts double as a regression check after upgrades |
| Touch coordinates off with fractional scaling | Resolved in M2: touch takes logical px; host divides by the tracked scale |
| No frames when screen is static → client stalls on join | Keep the last raw frame; re-encode it as an IDR on connect / keyframe request |
| NVIDIA hybrid graphics: DMA‑BUF import / `nvh264enc` quirks | Start with SHM frames → `x264enc` (CPU); add GPU path after it works |
| CPU cost of copying BGRx frames (2560×1600×4 B at 60 fps ≈ 1 GB/s) | Fine for M1; DMA‑BUF path later |
| WebCodecs unavailable over plain HTTP on LAN | Resolved: `--wifi` serves HTTPS with a self-signed certificate |

## Dev environment notes

See [DEVELOPMENT.md](DEVELOPMENT.md) (conda shadowing system tools, build
dependencies). Not yet needed: Intel VA encoder (`intel-media-va-driver-non-free`
for `vah264enc`); Android SDK at `~/Android/Sdk`, JDK 17 for Gradle (M3).

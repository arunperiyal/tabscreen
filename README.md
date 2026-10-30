# TabScreen

Use an Android tablet as a **touch-enabled second monitor** for a GNOME (Wayland)
desktop. TabScreen creates a real virtual monitor — it *extends* your desktop,
it doesn't mirror it — streams it to the tablet as H.264 video, and sends the
tablet's touches back.

```
 Linux (GNOME, Wayland)                                         Android tablet
 ┌────────────────────────────────────────────┐                ┌──────────────────┐
 │ Mutter virtual monitor ─► PipeWire          │   USB (adb     │ Chrome           │
 │   ─► H.264 (NVENC / x264) ─► WebSocket ─────┼── reverse) ───►│  decode + cursor │
 │ Mutter touch injection ◄─────────────────────┼◄── touches ───┤  touch events    │
 └────────────────────────────────────────────┘                └──────────────────┘
```

## Status

| Milestone | State |
|---|---|
| M0 — prove virtual monitor + touch on GNOME | ✅ |
| M1 — Rust host + browser client over USB | ✅ verified on a 2944×1840 tablet |
| M2 — monitor follows the tablet, display scaling, auto encoder, USB replug | ✅ essentials (tablet checks pending) |
| Wi‑Fi without adb (HTTPS + access token) | ✅ host side; tablet check pending |
| M3 — native Android app | planned |
| Release binary | ✅ GitHub Releases (Linux x86_64) |

Details, measurements and decisions are in [PLAN.md](PLAN.md).

## Features

- **Real extended display**: a monitor appears in Settings → Displays; arrange it
  and set its scale like any other screen.
- **Sized by the tablet**: the monitor matches the tablet's screen, follows
  rotation, and is removed 10 s after the tablet disconnects.
- **Touch** at exact positions, multi-touch, at any display scale.
- **Smooth mouse cursor**: drawn by the tablet from cursor updates at 60 Hz,
  independent of video.
- **Low latency**: hardware encoding with NVENC when available (x264 otherwise),
  hardware decoding on the tablet; the host adds ~12–17 ms at 2944×1840.
- **No dialogs, no root**: uses Mutter's own D-Bus APIs.

## Requirements

**Host**
- Linux with GNOME on Wayland (tested: Ubuntu 26.04, GNOME 50)
- PipeWire and GStreamer plugins: `gstreamer1.0-plugins-base`,
  `gstreamer1.0-plugins-bad` (`h264parse`, and `nvh264enc` for NVIDIA GPUs) and
  `gstreamer1.0-plugins-ugly` (`x264enc`). An NVIDIA GPU is used automatically
  when `nvh264enc` is available.
- `adb` (`sudo apt install adb`)
- To build: Rust (stable) and
  ```sh
  sudo apt install libpipewire-0.3-dev libspa-0.2-dev libgstreamer1.0-dev \
                   libgstreamer-plugins-base1.0-dev libclang-dev
  ```

**Tablet**
- Android with Chrome
- Developer options → **USB debugging** enabled, and this computer authorized
  (`adb devices` lists it as `device`)

## Install

**Prebuilt binary** (Linux x86_64): download the latest
`tabscreen-host-*-linux-x86_64.tar.gz` from
[Releases](https://github.com/arunperiyal/tabscreen/releases), then

```sh
sudo apt install libpipewire-0.3-0t64 gstreamer1.0-plugins-base \
                 gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly adb
tar xzf tabscreen-host-*-linux-x86_64.tar.gz
./tabscreen-host-*-linux-x86_64/tabscreen-host --usb
```

**From source:**

```sh
git clone https://github.com/arunperiyal/tabscreen.git
cd tabscreen/host
cargo build --release
```

The binary is `host/target/release/tabscreen-host`; the web client is embedded
in it.

## Usage

1. Connect the tablet with a **USB cable** (wireless adb works but adds
   noticeable lag).
2. Start the host:
   ```sh
   tabscreen-host --usb
   ```
3. On the tablet, open **`http://localhost:8765`** in Chrome and tap to start
   (goes fullscreen and keeps the screen on).
4. In **Settings → Displays**, drag the new monitor to where the tablet sits and
   pick a scale — at high tablet resolutions 200 % is usually comfortable.

Stop with Ctrl+C; the virtual monitor disappears.

### Over Wi‑Fi (no cable)

```sh
tabscreen-host --wifi
```

The host prints a URL like `https://192.168.0.10:8766/#t=<token>` and a QR code.
On the tablet (same network), open that URL — or scan the QR code — and accept
the certificate warning once (**Advanced → Proceed**): the host uses a
self-signed certificate because Chrome only decodes video on HTTPS pages.

- The token in the URL is required for every non-local connection; anyone with
  the URL can see and control the screen. The certificate and token live in
  `~/.config/tabscreen/`; delete `token` to issue a new one.
- If a firewall is active, open the port for your network, e.g.
  `sudo ufw allow from 192.168.0.0/24 to any port 8766 proto tcp`.
- `--wifi` and `--usb` can be used together; USB stays on `http://localhost:8765`.

### Options

| Option | Default | |
|---|---|---|
| `--usb` | off | Keep `adb reverse` set up for USB devices, including after a replug |
| `--size WxH` | tablet's screen | Fixed monitor size instead of following the tablet |
| `--encoder auto\|nvenc\|x264` | `auto` | `auto` picks NVENC when available |
| `--bitrate KBPS` | `12000` | Target video bitrate |
| `--port PORT` | `8765` | HTTP/WebSocket port |
| `--bind ADDR` | `127.0.0.1` | Listen address of the HTTP (USB) server; the default only accepts local connections |
| `--wifi` | off | Also serve HTTPS on all interfaces for Wi‑Fi, protected by a token |
| `--wifi-port PORT` | `8766` | HTTPS port for `--wifi` |

The page's status line shows resolution, fps, bitrate, round-trip time and
decode time; the host logs capture and encoder statistics every 5 s.

## Troubleshooting

- **"WebCodecs unavailable"** — open `http://localhost:8765` (USB) or the
  `https://` URL printed by `--wifi`. Chrome only decodes video on secure
  origins, so plain `http://<ip>` doesn't work.
- **"cannot connect: use the exact URL"** — the token is missing or wrong;
  copy the URL (including `#t=…`) from the host's output.
- **Address already in use** — another `tabscreen-host` is running; stop it or
  use `--port`.
- **Tablet can't connect** — check `adb devices` shows it as `device` and
  `adb reverse --list` includes the port.
- **Laggy** — use USB rather than wireless adb; try `--encoder nvenc` if the CPU
  is busy.
- **conda/miniconda installed** — it shadows the system `pkg-config` and `cc`;
  `.cargo/config.toml` pins the system ones for builds. Use `/usr/bin/python3`
  for the scripts in `scripts/`.

## Development

```
host/              Rust host (tabscreen-host)
  src/main.rs        CLI
  src/session.rs     one virtual monitor per tablet size; lifecycle
  src/mutter.rs      Mutter RemoteDesktop/ScreenCast D-Bus session
  src/display.rs     DisplayConfig: our connector and its scale
  src/capture.rs     PipeWire consumer: frames + cursor metadata
  src/encoder.rs     GStreamer H.264 encoding
  src/input.rs       touch injection
  src/server.rs      HTTP + WebSocket protocol
  src/usb.rs         adb reverse watcher
  src/wifi.rs        Wi‑Fi: self-signed certificate, access token, QR code
  examples/          ws_check (end-to-end test), cursor_spike
web/               browser client (embedded into the binary at build time)
scripts/           M0 Python spikes, touch target, set_scale helper
PLAN.md            roadmap, protocol, measurements
```

Checks:
- `cargo run --release --example ws_check -- [PORT] [WxH]` — end-to-end test
  against a running host: video, cursor and touch (uses `scripts/touch_target.py`;
  works on scaled monitors).
- `scripts/set_scale.py CONNECTOR SCALE` — set a monitor's scale temporarily.
- `cargo run --example cursor_spike` — cursor update rate from PipeWire.

### Releasing

Bump `version` in `host/Cargo.toml`, commit, then tag and push:

```sh
git tag v0.2.0 && git push origin v0.2.0
```

`.github/workflows/release.yml` builds on Ubuntu 24.04, checks the tag matches
the version, and publishes a GitHub Release with the tarball and its SHA-256.
Running the workflow manually (Actions → Release → Run workflow) only builds and
uploads the tarball as an artifact.

## Roadmap

See [PLAN.md](PLAN.md): native Android app (M3) and polish (auto-discovery,
service, indicator).

## License

[MIT](LICENSE)

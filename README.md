# TabScreen

Use an Android tablet as a **touch-enabled second monitor** for a GNOME (Wayland)
desktop. TabScreen creates a real virtual monitor — it *extends* your desktop,
it doesn't mirror it — streams it to the tablet as H.264 video, and sends the
tablet's touches back. Connect over a USB cable or Wi‑Fi; the tablet only needs
Chrome.

```
 Linux (GNOME, Wayland)                                  Android tablet (Chrome)
 ┌──────────────────────────────────────────┐            ┌──────────────────────┐
 │ virtual monitor ─► H.264 (NVENC / x264) ─┼── USB ────►│ video + cursor       │
 │ touch injection ◄────────────────────────┼◄─ Wi‑Fi ───┤ touches              │
 └──────────────────────────────────────────┘            └──────────────────────┘
```

## Features

- **Real extended display** — appears in Settings → Displays; arrange it and set
  its scale like any other screen.
- **Sized by the tablet** — matches the tablet's resolution, follows rotation,
  and is removed shortly after the tablet disconnects.
- **Touch** — multi-touch at exact positions, at any display scale.
- **Smooth cursor** — drawn on the tablet at 60 Hz, independent of video.
- **Low latency** — hardware encoding (NVENC) when available, hardware decoding
  on the tablet.
- **USB or Wi‑Fi** — USB via `adb reverse`; Wi‑Fi over HTTPS with an access token.
- **No dialogs, no root** — uses GNOME's own D-Bus APIs.

## Requirements

**Computer**
- Linux with GNOME on Wayland (tested on Ubuntu 26.04, GNOME 50)
- `libpipewire-0.3`, GStreamer with `gstreamer1.0-plugins-base`,
  `gstreamer1.0-plugins-bad` and `gstreamer1.0-plugins-ugly`
- `adb` for USB connections
- Optional: an NVIDIA GPU (used automatically through `nvh264enc`)

**Tablet**
- Android with Chrome
- For USB: Developer options → **USB debugging**, with this computer authorized

## Install

Download `tabscreen-host-*-linux-x86_64.tar.gz` from
[Releases](https://github.com/arunperiyal/tabscreen/releases):

```sh
sudo apt install libpipewire-0.3-0t64 gstreamer1.0-plugins-base \
                 gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly adb
tar xzf tabscreen-host-*-linux-x86_64.tar.gz
cd tabscreen-host-*-linux-x86_64
./tabscreen-host --usb
```

Or build from source — see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Usage

### Over USB

1. Connect the tablet with a USB cable.
2. Run `tabscreen-host --usb`.
3. On the tablet, open **`http://localhost:8765`** in Chrome and tap to start
   (fullscreen, screen stays on).

### Over Wi‑Fi

1. Run `tabscreen-host --wifi`. It prints a URL like
   `https://192.168.0.10:8766/#t=<token>` and a QR code.
2. On the tablet (same network), open the URL or scan the QR code.
3. Accept the certificate warning once (**Advanced → Proceed**) and tap to start.

The host uses a self-signed certificate because Chrome only decodes video on
HTTPS pages. The token in the URL is required for every Wi‑Fi connection —
anyone with the URL can see and control the screen. If a firewall is active,
open the port, e.g. `sudo ufw allow from 192.168.0.0/24 to any port 8766 proto tcp`.

`--usb` and `--wifi` can be combined.

### Then

In **Settings → Displays**, drag the new monitor to where the tablet sits and
pick a scale (200 % suits high-resolution tablets). Press Ctrl+C to stop; the
monitor disappears.

### Options

| Option | Default | |
|---|---|---|
| `--usb` | off | Keep `adb reverse` set up for USB devices, including after a replug |
| `--wifi` | off | Also accept tablets over Wi‑Fi (HTTPS + token) |
| `--size WxH` | tablet's screen | Fixed monitor size instead of following the tablet |
| `--encoder auto\|nvenc\|x264` | `auto` | `auto` picks NVENC when available |
| `--bitrate KBPS` | `12000` | Target video bitrate |
| `--port PORT` | `8765` | HTTP port (USB) |
| `--wifi-port PORT` | `8766` | HTTPS port (Wi‑Fi) |
| `--bind ADDR` | `127.0.0.1` | Listen address of the HTTP server |

The page's status line shows resolution, fps, bitrate, round-trip time and
decode time.

## Documentation

- [Troubleshooting](docs/TROUBLESHOOTING.md) — connection, display, touch and
  performance problems
- [Architecture](docs/ARCHITECTURE.md) — how it works, protocol, security model,
  design decisions
- [Development](docs/DEVELOPMENT.md) — building, testing, releasing
- [Plan](docs/PLAN.md) — roadmap and the findings behind each step

## License

[MIT](LICENSE)

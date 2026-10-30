# TabScreen

Use an Android tablet as a **touch-enabled second monitor** for a GNOME (Wayland)
desktop. TabScreen creates a real virtual monitor — it *extends* your desktop —
streams it to the tablet as H.264 video, and sends the tablet's touches back. It
can also **mirror** an existing screen, such as a projector, to any number of
phones and laptops. Connect over a USB cable or Wi‑Fi; clients only need Chrome.

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
- **Several viewers** — any number of browsers can watch the same stream;
  `--no-touch` / `view-only` makes it view-only.
- **Mirror** an existing monitor (e.g. the projector) to an audience.
- **Several streams at once** — e.g. your tablet as a second monitor *and* the
  projector mirrored to the audience, each with its own link.
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

`--usb` and `--wifi` can be combined. `--usb` is only needed for devices on a
cable; when everyone uses Wi‑Fi, `--wifi` alone is enough.

### Sharing with several viewers

```sh
tabscreen-host --wifi --no-touch
```

Share the printed URL; everyone who opens it sees the same monitor, scaled to
their screen. The first viewer's screen decides the virtual monitor's size, and
later viewers don't change it. With `--no-touch` the URL only lets people watch.
(Without it, every viewer can touch.)

To show an **existing** screen instead of a new virtual one, mirror it:

```sh
tabscreen-host --list-monitors          # find the connector, e.g. HDMI-1
tabscreen-host --wifi --mirror HDMI-1 --no-touch
```

### Presenting: tablet + projector to the audience

Several streams can run at once, each with its own page, link and token:

```sh
tabscreen-host --wifi \
  --stream tab=virtual \
  --stream audience=mirror:HDMI-1,view-only
```

- **tab** — a virtual monitor for your tablet (touch), e.g. for the presenter's
  notes: its own Wi‑Fi link (or `http://localhost:8765/` over USB with `--usb`).
- **audience** — whatever the projector on `HDMI-1` shows, view-only, at
  `…/s/audience/`. Share its link or QR code; it can't open the `tab` stream.

Stream syntax: `NAME=virtual[,view-only][,size=WxH]` or
`NAME=mirror:CONNECTOR[,view-only]`. The first stream is also served at `/`.
A mirror starts with the host and stays running; until the mirrored screen
first changes after startup, viewers see "waiting for video". Known issue: a
viewer on a slow connection can fall behind during animations — see
[Troubleshooting](docs/TROUBLESHOOTING.md).

### Then

In **Settings → Displays**, drag the new monitor to where the tablet sits and
pick a scale (200 % suits high-resolution tablets). Press Ctrl+C to stop; the
monitor disappears.

### Options

| Option | Default | |
|---|---|---|
| `--usb` | off | Keep `adb reverse` set up for USB devices, including after a replug |
| `--wifi` | off | Also accept tablets over Wi‑Fi (HTTPS + token) |
| `--stream SPEC` | one virtual stream | Define a stream (repeatable); see above |
| `--mirror CONNECTOR` | — | Mirror an existing monitor instead of creating one |
| `--no-touch` | off | View only: ignore touch from every client |
| `--size WxH` | tablet's screen | Fixed monitor size instead of following the tablet |
| `--list-monitors` | | List monitors and their connector names, then exit |
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

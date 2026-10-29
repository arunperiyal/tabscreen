# TabScreen

Use an Android tablet as a touch-enabled second monitor for GNOME on Wayland.
The host creates a real virtual monitor (it extends the desktop, it doesn't
mirror), streams it as H.264 to the tablet, and sends the tablet's touches back.

Status: M1 done, M2 essentials done — see the plan for what's next.
See [PLAN.md](PLAN.md).

Use a USB cable rather than wireless adb: `adb reverse` over Wi‑Fi adds noticeable lag.

## Requirements

- GNOME on Wayland (uses Mutter's RemoteDesktop/ScreenCast D-Bus APIs)
- Build deps: `sudo apt install libpipewire-0.3-dev libspa-0.2-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libclang-dev`
- Runtime: GStreamer `x264enc` (or `nvh264enc` with `--encoder nvenc`), `adb` for USB
- Tablet: Chrome, USB debugging enabled

## Run (USB)

```sh
cd host
cargo build --release
./target/release/tabscreen-host --usb
```

Then open `http://localhost:8765` in Chrome on the tablet and tap to start.
A monitor matching the tablet's screen appears in Settings → Displays; arrange
it next to your screens and pick a scale (e.g. 200 %). It follows rotation,
and is removed 10 s after the tablet disconnects (or on Ctrl+C). `--usb` keeps
the port forwarded across cable replugs.

Options: `--size WxH` (fixed size), `--port`, `--bitrate` (kbit/s),
`--encoder auto|x264|nvenc`, `--bind` (default `127.0.0.1`, so only USB/local
connections are accepted).

## Checks

- `cargo run --release --example ws_check -- [PORT] [WxH]` — scripted end-to-end
  check against a running host (video, cursor and touch, using `scripts/touch_target.py`;
  works on scaled monitors). `scripts/set_scale.py CONNECTOR SCALE` changes a monitor's scale.
- `cargo run --example cursor_spike` — cursor metadata rate from PipeWire.
- `scripts/` — the M0 Python spikes; run with `/usr/bin/python3`.

Conda users: `.cargo/config.toml` pins the system `pkg-config` and `cc`.

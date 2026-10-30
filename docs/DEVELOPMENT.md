# Development

## Building

Requirements: Rust (stable, edition 2024) and

```sh
sudo apt install libpipewire-0.3-dev libspa-0.2-dev libgstreamer1.0-dev \
                 libgstreamer-plugins-base1.0-dev libclang-dev
```

```sh
cd host
cargo build --release       # target/release/tabscreen-host
```

The web client (`web/`) is embedded at compile time with `include_str!`, so
rebuild the host after changing it.

**conda/miniconda users:** conda's `bin` directory shadows the system
`pkg-config`, `cc`, `python3` and `gst-inspect-1.0`. `.cargo/config.toml` pins
`/usr/bin/pkg-config` and `/usr/bin/cc` for builds; run the scripts with
`/usr/bin/python3` and inspect GStreamer with `/usr/bin/gst-inspect-1.0`.

## Repository layout

```
host/                 Rust crate tabscreen-host
  src/                  see ARCHITECTURE.md for what each module does
  examples/
    ws_check.rs         end-to-end test client
    cursor_spike.rs     cursor metadata rate from PipeWire
web/                  browser client (index.html, client.js)
scripts/              Python helpers (run with /usr/bin/python3)
  touch_target.py       fullscreen window on a monitor that logs touches
  set_scale.py          set a monitor's scale temporarily
  spike_*.py            M0 experiments (portal vs Mutter API)
docs/                 PLAN, ARCHITECTURE, TROUBLESHOOTING, DEVELOPMENT
.github/workflows/    release workflow
.cargo/config.toml    pins system pkg-config and cc
```

## Testing

There's no unit test suite yet; the end-to-end check exercises the real stack
(Mutter, PipeWire, GStreamer, WebSocket, touch injection) on a GNOME session.

```sh
cd host
./target/release/tabscreen-host --port 8799 &
cargo run --release --example ws_check -- 8799 2000x1250
```

`ws_check` connects as a client with the given screen size and checks:
- `config` and cursor messages arrive;
- the first frame is a keyframe even on a static screen;
- three touches land at the expected pixels on `scripts/touch_target.py`, which
  it opens fullscreen on the virtual monitor.

It works on scaled monitors too:

```sh
../scripts/set_scale.py Meta-1 2      # connector from the host log
cargo run --release --example ws_check -- 8799 2000x1250
```

Use a port and size different from a tablet session you have running, so the
test doesn't attach to your real monitor.

For the browser side, open `http://localhost:8799` in desktop Chrome; the status
line and console (`decoder configured …`) show whether decoding works. Desktop
Chrome on Linux usually decodes in software.

Other tools:
- `cargo run --example cursor_spike -- 1600x1000 10` — cursor updates and
  frames per second straight from PipeWire.
- `cargo clippy --release --all-targets` — kept warning-free.

## Measuring performance

The host logs every 5 s while frames flow:

```
capture: 59 fps from PipeWire, copy avg 3.3 ms
encoder: 57 fps in, 57 fps out, 13.1 Mbit/s, encode latency avg 13.8 ms, max 16.4 ms
```

Encode latency is measured from `appsrc`'s output to the `appsink` (a pad probe
plus a FIFO; encoders rewrite timestamps). To load the virtual monitor with
constant motion, open a window that redraws every frame on it — a CPU-drawn
window may itself only reach ~20 fps at high resolutions, so use a GPU-drawn one.

## Releasing

1. Bump `version` in `host/Cargo.toml` and commit.
2. Tag and push:
   ```sh
   git tag v0.2.0 && git push origin v0.2.0
   ```

`.github/workflows/release.yml` builds on Ubuntu 24.04 (so the binary runs there
and on newer systems), checks the tag matches the crate version, runs
`tabscreen-host --help`, and publishes a GitHub Release with
`tabscreen-host-<version>-linux-x86_64.tar.gz` (binary, README, LICENSE, docs)
and its SHA-256. Running the workflow manually (Actions → Release → Run
workflow) only uploads the tarball as an artifact.

Runtime dependencies of the binary: `libpipewire-0.3`, GLib, GStreamer core and
`gstreamer1.0-plugins-base` (appsrc/appsink, videoconvert), `-bad` (h264parse,
nvh264enc), `-ugly` (x264enc).

## Branches

Work happens on feature branches; `main` is updated by fast-forward once a
change is verified on a tablet.

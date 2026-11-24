# Latency test

How to measure screen-to-screen latency the same way for TabScreen and AnyDesk, so the
numbers can be compared (M2c in [PLAN.md](PLAN.md)). Change one thing at a time and keep
everything else identical between runs.

## What is measured

Latency = the clock shown on the viewer minus the clock shown on the reference, in the
same filmed frame. It includes capture, encode, network, decode and the viewer's display.
The reference is the laptop's own panel; its display delay is the same in every run, so
it cancels when you compare tools.

## Setup (identical for every run)

- Same computer, same Wi‑Fi (same access point and band), same viewer device. Note them:
  host model, GNOME version, GPU and encoder, viewer device and browser, band (2.4/5 GHz),
  distance to the router.
- Close other heavy apps. Mains power on the host, screen brightness on the viewer fixed.
- Resolution: note the viewer's size (the virtual monitor follows it) and use the same for
  AnyDesk if it lets you choose.
- Bitrate: TabScreen default (`--bitrate 12000`) unless the run says otherwise.

## The clock

`scripts/latency_clock.html` shows milliseconds from the system clock. Open it in
**two windows**: one on the laptop panel (reference) and one on the monitor that the
viewer shows (the virtual monitor, or the projector for a mirror). Both read the same
clock, so a difference between them is only delay.

## Scenes

1. **Clock only** (idle desktop around it): the baseline, mostly static scene.
2. **Slide animation**: a presentation with a full-screen animation on the streamed
   monitor, the clock window on top in a corner.
3. **Window drag**: drag a window around on the streamed monitor continuously.
4. **Scrolling text**: scroll a long page at a steady speed.

For scenes 2–4 keep the clock visible on both screens in a corner.

## Filming

- Phone at 240 fps (slow-motion mode), fixed on a stand, framing the laptop panel and the
  viewer together, both clocks readable. 10 s per scene.
- In the clip, pick 10 frames spread over the clip and write down both readings; latency
  per frame = reference − viewer. Report the median and the spread (min–max).

## Commands

```sh
# Tablet (virtual monitor), Wi‑Fi:
tabscreen-host --wifi
# Tablet over USB:
tabscreen-host --usb
# Phone watching the projector:
tabscreen-host --wifi --stream audience=mirror:HDMI-1,view-only
```

Keep the host log (stderr) of each run: the `capture:`, `encoder:` and per-viewer lines
give the stage times, and the viewer's status line gives fps, Mbit/s, `rtt` and decode.

## Results table (fill one per run)

| Run | Tool | Link | Scene | Latency median (ms) | Min–max | Viewer fps | Mbit/s | rtt | decode (ms) | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| | | | | | | | | | | |

Test matrix for TabScreen: tablet over USB, tablet over Wi‑Fi, phone over Wi‑Fi
(mirror), and with `--public-host` if you can; each in scenes 1–4. For AnyDesk: the same
viewer devices and scenes.

## AnyDesk runs

AnyDesk is measured the same way (same clock page, scenes, filming, devices) so the
rows are comparable. Differences to write down in the Notes column:

- **What it shares.** On a Wayland session AnyDesk shares an existing screen through the
  desktop's share dialog; it does not create an extra monitor. So it mirrors the
  laptop panel (or the projector), and touch maps onto that screen. Compare it with
  TabScreen's **mirror** scenes (phone/tablet watching a mirror), and note that a
  TabScreen virtual monitor is a different (extended) setup.
- **Versions and settings.** Host AnyDesk version (`anydesk --version`), the tablet app
  version, and the display quality setting used (record the choices exactly as the app
  names them: quality/speed preset, codec if shown, resolution scaling). Run once at
  its default and once at its fastest setting.
- **Resolution.** Note the size AnyDesk streams at; it may scale down. If it does, also
  run TabScreen with `--size` set to the same.
- **Bitrate.** Measure the host's network rate while the scene runs, for both tools:

  ```sh
  ip -br link          # find the Wi‑Fi interface, on this machine wlp0s20f3
  scripts/net_rate.sh wlp0s20f3 10
  ```

  `tx` is what the host sends; subtract the idle value (run it once with nothing
  streaming) if the network is busy. Do it during the scene, not before it.
- **Other connections.** AnyDesk may relay through its servers or connect directly; the
  session information in the app tells which. Direct (same LAN) is the fair comparison;
  note it if it is relayed.

Fill one row per tool, scene and setting in the same table as above.

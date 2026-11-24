# Real-device test checklist

One session that clears every open **Real-device test (you)** box in
[PLAN.md](PLAN.md). Report the results (pass/fail, log lines, numbers) and they are
recorded next to each box. Nothing here is ticked until you have run it.

## Setup

- Tablet (USB and Wi‑Fi), phone (Wi‑Fi), optionally the projector on `HDMI-1`.
  Find the connector with `tabscreen-host --list-monitors`.
- Build the current `main`: `cd host && cargo build --release`.
- Run the host with the real setup, keeping the log visible (it goes to stderr):

  ```
  tabscreen-host --usb --wifi \
    --stream tab=virtual \
    --stream audience=mirror:HDMI-1,view-only
  ```
- Open the printed URL (with `#t=…`) on each device. The status line at the top of
  the page shows fps, Mbit/s, `rtt` and decode time.
- Test content: a slide deck with animations, or a full-screen video.

## 1. M2: robustness (tablet)

| # | Steps | Expect |
|---|---|---|
| 1.1 | Pull the USB cable, plug it back in. | Within about 2 s the page reconnects and shows video; log has `usb: <serial> disconnected` and no manual `adb reverse` was needed. |
| 1.2 | Rotate the tablet. | The virtual monitor is recreated at the new size; touch still lands under the finger. |
| 1.3 | In Settings → Displays set the tablet's monitor to 150 %, then 200 %. | Log shows `display: Meta-N scale …`; a tap in the centre and in each corner lands under the finger. |

## 2. `m2b/frame-ack`

| # | Steps | Expect |
|---|---|---|
| 2.1 | Connect tablet and phone on a good link, play an animation for a minute. | Normal playback; no `viewer was slow` lines; `rtt` stays low (tens of ms). |
| 2.2 | Keep a phone tab open from before the update (not reloaded). | It still plays video (it sends no acks). |

## 3. `m2b/skip-slow-client`

| # | Steps | Expect |
|---|---|---|
| 3.1 | Make the phone's link slow during an animation (walk far from the router, or throttle with `tc qdisc add dev <wifi-if> root netem rate 2mbit` and remove it afterwards with `tc qdisc del dev <wifi-if> root`). | Log: `audience: viewer was slow, skipped N frames`; phone fps drops; its `rtt` stays low; the tablet stream is unaffected. |
| 3.2 | Remove the throttle. | The phone's picture recovers at the next keyframe. |

## 4. `m2b/rate-control`

| # | Steps | Expect |
|---|---|---|
| 4.1 | Run the audience with `audience=mirror:HDMI-1,view-only,bitrate=4000`, play the same animation, note phone fps, Mbit/s, `rtt` and sharpness. | Audience Mbit/s stays near 4; the tablet stream keeps full quality. |
| 4.2 | Repeat with the default bitrate. | Compare against 4.1. Say which you would present with. |

## 5. `m2b/retest` (only after 2–4 pass)

| # | Steps | Expect |
|---|---|---|
| 5.1 | Present a whole talk: tablet with notes, projector, phone audience. | Phone latency during animations stays close to the tablet's. Send the status-line numbers. |

## What to send back

For each row: pass or fail, plus any host log lines (`capture:`, `encoder:`,
`viewer was slow`) and the status-line numbers where asked. Failures are the useful
part: send the log around them.

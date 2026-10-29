//! End-to-end check of a running tabscreen-host, without a browser.
//!
//! Connects to ws://localhost:PORT/ws, checks the config, cursor and video
//! messages, then sends touches onto `scripts/touch_target.py` (fullscreen on
//! the virtual monitor) and checks where they land.
//!
//!   cargo run --example ws_check -- [PORT] [WIDTHxHEIGHT]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[derive(Default, Debug)]
struct Seen {
    config: Option<String>,
    cursor_images: u32,
    cursor_moves: u32,
    frames: u32,
    keyframes: u32,
    first_frame_key: Option<bool>,
    bytes: usize,
}

fn note(seen: &mut Seen, data: &[u8]) {
    match data[0] {
        0x02 => seen.config = Some(String::from_utf8_lossy(&data[1..]).into_owned()),
        0x03 => {
            let key = data[1] & 1 == 1;
            seen.first_frame_key.get_or_insert(key);
            seen.frames += 1;
            seen.keyframes += key as u32;
            seen.bytes += data.len();
        }
        0x07 => seen.cursor_moves += 1,
        0x08 => seen.cursor_images += 1,
        _ => {}
    }
}

fn touch(action: u8, slot: u8, x: f32, y: f32) -> Message {
    let mut m = vec![0x04, action, slot];
    m.extend_from_slice(&x.to_le_bytes());
    m.extend_from_slice(&y.to_le_bytes());
    Message::Binary(m.into())
}

/// Read and tally messages for `d`.
async fn read_for<S>(ws: &mut S, seen: &mut Seen, d: Duration) -> Result<()>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let end = tokio::time::Instant::now() + d;
    while let Ok(Some(msg)) = tokio::time::timeout_at(end, ws.next()).await {
        if let Message::Binary(data) = msg? {
            note(seen, &data);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let port = std::env::args().nth(1).unwrap_or_else(|| "8765".into());
    let size = std::env::args().nth(2).unwrap_or_else(|| "1600x1000".into());
    let (hello_w, hello_h) = size.split_once('x').context("size must be WIDTHxHEIGHT")?;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://localhost:{port}/ws"))
        .await
        .context("connecting; is tabscreen-host running?")?;
    let mut seen = Seen::default();
    let hello = format!(r#"{{"w":{hello_w},"h":{hello_h},"codecs":["h264"],"version":1}}"#);
    ws.send(Message::Binary([&[0x01], hello.as_bytes()].concat().into())).await?;

    read_for(&mut ws, &mut seen, Duration::from_secs(2)).await?;
    println!("after connect (static screen): {seen:?}");
    let config: serde_json::Value = serde_json::from_str(seen.config.as_deref().context("no config message")?)?;
    let (w, h) = (config["w"].as_u64().unwrap(), config["h"].as_u64().unwrap());
    if seen.first_frame_key != Some(true) {
        bail!("first frame was not a keyframe (or no frame at all)");
    }

    // Touch target fullscreen on the virtual monitor (also produces damage → frames).
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/touch_target.py");
    let mut target = Command::new("/usr/bin/python3")
        .args([script, &w.to_string(), &h.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
    let stdout = target.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str(&line) {
                let _ = tx.send(v);
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    // Window size in logical pixels: smaller than w×h when the monitor is scaled.
    let logical_scale = loop {
        read_for(&mut ws, &mut seen, Duration::from_millis(100)).await?;
        match rx.try_recv() {
            Ok(v) if v["event"] == "ready" => break v["w"].as_f64().unwrap() / w as f64,
            Ok(v) if v["event"] == "error" => bail!("touch target: {v}"),
            _ if Instant::now() > deadline => bail!("touch target never became ready"),
            _ => {}
        }
    };
    if logical_scale != 1.0 {
        println!("monitor is scaled: 1 logical px = {:.3} device px", 1.0 / logical_scale);
    }

    let points = [(0.1f32, 0.9f32), (0.5, 0.5), (0.25, 0.25), (0.75, 0.8)]; // first is a warm-up
    for (slot, &(x, y)) in points.iter().enumerate() {
        ws.send(touch(0, slot as u8, x, y)).await?;
        ws.send(touch(1, slot as u8, x + 0.001, y + 0.001)).await?;
        ws.send(touch(2, slot as u8, x, y)).await?;
        read_for(&mut ws, &mut seen, Duration::from_millis(300)).await?;
    }
    read_for(&mut ws, &mut seen, Duration::from_millis(500)).await?;
    target.kill()?;
    let events: Vec<_> = rx.try_iter().filter(|v| v["event"] == "touch_begin").collect();

    let mut ok = true;
    for &(x, y) in &points[1..] {
        let (ex, ey) = (x as f64 * (w - 1) as f64 * logical_scale, y as f64 * (h - 1) as f64 * logical_scale);
        let hit = events.iter().any(|v| {
            (v["x"].as_f64().unwrap() - ex).abs() <= 2.0 && (v["y"].as_f64().unwrap() - ey).abs() <= 2.0
        });
        println!("touch ({ex:.0},{ey:.0}): {}", if hit { "OK" } else { "NOT RECEIVED" });
        ok &= hit;
    }
    println!("totals: {seen:?}");
    if !ok {
        bail!("touch check failed; received {events:?}");
    }
    println!("ws_check: PASS");
    Ok(())
}

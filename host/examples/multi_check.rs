//! Several viewers on one host: the first client's size sticks, everyone gets
//! video, and touch is accepted or ignored according to `--no-touch`.
//!
//!   cargo run --example multi_check -- PORT touch|no-touch

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, Stream, StreamExt};
use tokio_tungstenite::tungstenite::{Error, Message};

#[derive(Default, Debug)]
struct Seen {
    configs: Vec<serde_json::Value>,
    frames: u32,
    keyframes: u32,
}

async fn read_for<S>(ws: &mut S, seen: &mut Seen, d: Duration) -> Result<()>
where
    S: Stream<Item = Result<Message, Error>> + Unpin,
{
    let end = tokio::time::Instant::now() + d;
    while let Ok(Some(msg)) = tokio::time::timeout_at(end, ws.next()).await {
        if let Message::Binary(data) = msg? {
            match data[0] {
                0x02 => seen.configs.push(serde_json::from_slice(&data[1..])?),
                0x03 => {
                    seen.frames += 1;
                    seen.keyframes += (data[1] & 1) as u32;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn hello(w: u32, h: u32) -> Message {
    let json = format!(r#"{{"w":{w},"h":{h},"codecs":["h264"],"version":1}}"#);
    Message::Binary([&[0x01], json.as_bytes()].concat().into())
}

fn touch(action: u8, x: f32, y: f32) -> Message {
    let mut m = vec![0x04, action, 0];
    m.extend_from_slice(&x.to_le_bytes());
    m.extend_from_slice(&y.to_le_bytes());
    Message::Binary(m.into())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let port = args.next().unwrap_or_else(|| "8765".into());
    let expect_touch = match args.next().as_deref() {
        Some("touch") => true,
        Some("no-touch") => false,
        _ => bail!("usage: multi_check PORT touch|no-touch"),
    };
    let url = format!("ws://localhost:{port}/ws");

    let (mut a, _) = tokio_tungstenite::connect_async(&url).await.context("connecting A")?;
    a.send(hello(2000, 1250)).await?;
    let mut seen_a = Seen::default();
    read_for(&mut a, &mut seen_a, Duration::from_secs(2)).await?;
    let config_a = seen_a.configs.first().context("A got no config")?.clone();
    println!("A (2000x1250) got config {config_a}");

    // B has a portrait screen; it must not resize A's monitor.
    let (mut b, _) = tokio_tungstenite::connect_async(&url).await.context("connecting B")?;
    b.send(hello(1250, 2000)).await?;
    let mut seen_b = Seen::default();
    let (ra, rb) = tokio::join!(
        read_for(&mut a, &mut seen_a, Duration::from_secs(2)),
        read_for(&mut b, &mut seen_b, Duration::from_secs(2))
    );
    ra?;
    rb?;
    let config_b = seen_b.configs.first().context("B got no config")?.clone();
    println!("B (1250x2000) got config {config_b}");
    ensure!(config_b["w"] == 2000 && config_b["h"] == 1250, "B resized the monitor");
    ensure!(seen_a.configs.len() == 1, "A saw the session restart: {:?}", seen_a.configs);
    ensure!(seen_b.keyframes >= 1, "B got no keyframe");
    ensure!(config_b["touch"] == expect_touch, "config says touch={}", config_b["touch"]);

    // Touch from B onto a window that logs what it receives.
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/touch_target.py");
    let mut target = Command::new("/usr/bin/python3")
        .args([script, "2000", "1250"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = target.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str(&line) {
                let _ = tx.send(v);
            }
        }
    });
    loop {
        let (ra, rb) = tokio::join!(
            read_for(&mut a, &mut seen_a, Duration::from_millis(100)),
            read_for(&mut b, &mut seen_b, Duration::from_millis(100))
        );
        ra?;
        rb?;
        match rx.try_recv() {
            Ok(v) if v["event"] == "ready" => break,
            Ok(v) if v["event"] == "error" => bail!("touch target: {v}"),
            _ => {}
        }
    }
    for (x, y) in [(0.1, 0.9), (0.5, 0.5), (0.25, 0.25)] {
        b.send(touch(0, x, y)).await?;
        b.send(touch(2, x, y)).await?;
        read_for(&mut b, &mut seen_b, Duration::from_millis(300)).await?;
    }
    read_for(&mut b, &mut seen_b, Duration::from_millis(500)).await?;
    target.kill()?;
    let touches = rx.try_iter().filter(|v| v["event"] == "touch_begin").count();
    println!("touches delivered from B: {touches}; frames A={} B={}", seen_a.frames, seen_b.frames);
    ensure!((touches > 0) == expect_touch, "touch delivery doesn't match --no-touch setting");
    println!("multi_check: PASS");
    Ok(())
}

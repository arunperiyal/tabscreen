//! Connect to one stream like a client would and report what arrives.
//!
//!   cargo run --example probe -- ws://localhost:8765/s/audience/ws 1280x800 [SECONDS]

use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().context("usage: probe WS_URL WxH [SECONDS]")?;
    let size = args.next().unwrap_or_else(|| "1280x800".into());
    let seconds: u64 = args.next().map_or(Ok(4), |s| s.parse())?;
    let (w, h) = size.split_once('x').context("size must be WxH")?;

    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.with_context(|| format!("connecting to {url}"))?;
    let hello = format!(r#"{{"w":{w},"h":{h},"codecs":["h264"],"version":1}}"#);
    ws.send(Message::Binary([&[0x01], hello.as_bytes()].concat().into())).await?;

    let (mut frames, mut keyframes, mut bytes, mut cursor) = (0, 0, 0, 0);
    let end = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while let Ok(Some(msg)) = tokio::time::timeout_at(end, ws.next()).await {
        let Message::Binary(data) = msg? else { continue };
        match data[0] {
            0x02 => println!("config {}", String::from_utf8_lossy(&data[1..])),
            0x03 => {
                frames += 1;
                keyframes += (data[1] & 1) as u32;
                bytes += data.len();
            }
            0x07 | 0x08 => cursor += 1,
            _ => {}
        }
    }
    println!("{frames} frames ({keyframes} keyframes, {} KiB), {cursor} cursor messages", bytes / 1024);
    Ok(())
}

//! HTTP + WebSocket server: serves the web client and speaks the protocol in PLAN.md.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use tokio::sync::broadcast;

use crate::capture::CursorImage;
use crate::encoder::EncodedFrame;
use crate::input::{InputCmd, TouchAction};
use crate::session::{Manager, Session};

const MSG_HELLO: u8 = 0x01;
const MSG_CONFIG: u8 = 0x02;
const MSG_FRAME: u8 = 0x03;
const MSG_TOUCH: u8 = 0x04;
const MSG_KEYFRAME_REQUEST: u8 = 0x05;
const MSG_PING: u8 = 0x06;
const MSG_CURSOR_MOVE: u8 = 0x07;
const MSG_CURSOR_IMAGE: u8 = 0x08;

/// Largest monitor we create; H.264 encoders top out around 4096 per side.
const MAX_SIDE: u32 = 4096;

pub async fn serve(addr: SocketAddr, manager: Arc<Manager>) -> Result<()> {
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("../../web/index.html")) }))
        .route(
            "/client.js",
            get(|| async { ([("content-type", "text/javascript")], include_str!("../../web/client.js")) }),
        )
        .route("/ws", get(ws_handler))
        .with_state(manager);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn ws_handler(ws: WebSocketUpgrade, State(manager): State<Arc<Manager>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        eprintln!("server: client connected");
        manager.client_connected();
        if let Err(e) = client(socket, &manager).await {
            // A tab closed or a cable pulled resets the connection; not worth an error.
            let reset = e.chain().any(|c| c.to_string().contains("without closing handshake"));
            if !reset {
                eprintln!("server: client error: {e:#}");
            }
        }
        if let Some(session) = manager.subscribe().borrow().as_ref() {
            let _ = session.input.send(InputCmd::ReleaseAll);
        }
        manager.client_disconnected();
        eprintln!("server: client disconnected");
    })
}

fn frame_message(frame: &EncodedFrame) -> Message {
    let mut msg = Vec::with_capacity(10 + frame.data.len());
    msg.push(MSG_FRAME);
    msg.push(frame.keyframe as u8);
    msg.extend_from_slice(&frame.pts_us.to_le_bytes());
    msg.extend_from_slice(&frame.data);
    Message::Binary(msg.into())
}

fn cursor_move_message((x, y): (i32, i32)) -> Message {
    let mut msg = vec![MSG_CURSOR_MOVE];
    msg.extend_from_slice(&x.to_le_bytes());
    msg.extend_from_slice(&y.to_le_bytes());
    Message::Binary(msg.into())
}

fn cursor_image_message(image: &CursorImage) -> Message {
    let mut msg = vec![MSG_CURSOR_IMAGE];
    for v in [image.width, image.height, image.hot_x, image.hot_y] {
        msg.extend_from_slice(&v.to_le_bytes());
    }
    msg.extend_from_slice(&image.rgba);
    Message::Binary(msg.into())
}

fn parse_touch(payload: &[u8]) -> Option<InputCmd> {
    let [action, slot, rest @ ..] = payload else { return None };
    let x = f32::from_le_bytes(rest.get(0..4)?.try_into().ok()?);
    let y = f32::from_le_bytes(rest.get(4..8)?.try_into().ok()?);
    let action = match action {
        0 => TouchAction::Down,
        1 => TouchAction::Move,
        2 => TouchAction::Up,
        3 => TouchAction::Cancel,
        _ => return None,
    };
    Some(InputCmd::Touch { action, slot: *slot as u32, x, y })
}

/// Screen size from a hello message, rounded down to even numbers (4:2:0 video).
fn parse_hello(payload: &[u8]) -> Result<(u32, u32)> {
    let hello: serde_json::Value = serde_json::from_slice(payload).context("hello is not JSON")?;
    eprintln!("server: hello {hello}");
    let side = |key| {
        let v = hello[key].as_u64().with_context(|| format!("hello has no {key}"))?;
        anyhow::Ok((v as u32).clamp(64, MAX_SIDE) & !1)
    };
    Ok((side("w")?, side("h")?))
}

async fn acquire(manager: &Arc<Manager>, size: (u32, u32)) -> Result<Arc<Session>> {
    let manager = manager.clone();
    tokio::task::spawn_blocking(move || manager.acquire(size)).await?
}

/// Tell the client about a (new) session and get it a keyframe.
async fn start_session(socket: &mut WebSocket, session: &Session) -> Result<()> {
    let config = serde_json::json!({ "w": session.width, "h": session.height });
    let mut msg = vec![MSG_CONFIG];
    msg.extend_from_slice(config.to_string().as_bytes());
    socket.send(Message::Binary(msg.into())).await?;
    session.encoder.request_keyframe();
    Ok(())
}

async fn client(mut socket: WebSocket, manager: &Arc<Manager>) -> Result<()> {
    // The client says how big its screen is before we create a monitor for it.
    let wanted = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match socket.recv().await {
                Some(Ok(Message::Binary(data))) if data.first() == Some(&MSG_HELLO) => break parse_hello(&data[1..]),
                Some(Ok(_)) => continue,
                Some(Err(e)) => break Err(e.into()),
                None => bail!("closed before hello"),
            }
        }
    })
    .await
    .context("no hello within 10s")??;
    let mut wanted = wanted;

    let mut sessions = manager.subscribe();
    let mut session = acquire(manager, wanted).await?;
    sessions.borrow_and_update();
    let mut frames = manager.frames.subscribe();
    let mut position = manager.cursor.position.subscribe();
    let mut image = manager.cursor.image.subscribe();

    start_session(&mut socket, &session).await?;
    let (current_image, current_position) = (image.borrow_and_update().clone(), *position.borrow_and_update());
    socket.send(cursor_image_message(&current_image)).await?;
    socket.send(cursor_move_message(current_position)).await?;
    // Decoding can only start at a keyframe.
    let mut need_keyframe = true;

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(incoming) = incoming else { return Ok(()) };
                let Message::Binary(data) = incoming? else { continue };
                match data.first() {
                    Some(&MSG_TOUCH) => {
                        if let Some(cmd) = parse_touch(&data[1..]) {
                            let _ = session.input.send(cmd);
                        }
                    }
                    Some(&MSG_KEYFRAME_REQUEST) => {
                        need_keyframe = true;
                        session.encoder.request_keyframe();
                    }
                    Some(&MSG_PING) => socket.send(Message::Binary(data)).await?,
                    // Sent again when the tablet rotates.
                    Some(&MSG_HELLO) => {
                        wanted = parse_hello(&data[1..])?;
                        let next = acquire(manager, wanted).await?;
                        sessions.borrow_and_update();
                        if !Arc::ptr_eq(&next, &session) {
                            session = next;
                            need_keyframe = true;
                            start_session(&mut socket, &session).await?;
                        }
                    }
                    _ => {}
                }
            }
            Ok(()) = sessions.changed() => {
                // Another client resized the monitor, or Mutter ended the stream.
                let current = sessions.borrow_and_update().clone();
                let next = match current {
                    Some(next) => next,
                    None => acquire(manager, wanted).await?,
                };
                if !Arc::ptr_eq(&next, &session) {
                    session = next;
                    need_keyframe = true;
                    start_session(&mut socket, &session).await?;
                }
            }
            frame = frames.recv() => match frame {
                Ok(frame) => {
                    if need_keyframe && !frame.keyframe {
                        continue;
                    }
                    need_keyframe = false;
                    socket.send(frame_message(&frame)).await?;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("server: client fell behind by {n} frames, waiting for a keyframe");
                    need_keyframe = true;
                    session.encoder.request_keyframe();
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            Ok(()) = position.changed() => {
                let pos = *position.borrow_and_update();
                socket.send(cursor_move_message(pos)).await?;
            }
            Ok(()) = image.changed() => {
                let current = image.borrow_and_update().clone();
                socket.send(cursor_image_message(&current)).await?;
            }
        }
    }
}

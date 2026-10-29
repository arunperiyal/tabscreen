//! HTTP + WebSocket server: serves the web client and speaks the protocol in PLAN.md.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use anyhow::Result;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use tokio::sync::{broadcast, watch};

use crate::capture::CursorImage;
use crate::encoder::{EncodedFrame, Encoder};
use crate::input::{InputCmd, TouchAction};

const MSG_HELLO: u8 = 0x01;
const MSG_CONFIG: u8 = 0x02;
const MSG_FRAME: u8 = 0x03;
const MSG_TOUCH: u8 = 0x04;
const MSG_KEYFRAME_REQUEST: u8 = 0x05;
const MSG_PING: u8 = 0x06;
const MSG_CURSOR_MOVE: u8 = 0x07;
const MSG_CURSOR_IMAGE: u8 = 0x08;

#[derive(Clone)]
pub struct AppState {
    pub width: u32,
    pub height: u32,
    pub frames: broadcast::Sender<Arc<EncodedFrame>>,
    pub cursor_position: watch::Receiver<(i32, i32)>,
    pub cursor_image: watch::Receiver<Arc<CursorImage>>,
    pub encoder: Arc<Encoder>,
    pub input: Sender<InputCmd>,
}

pub async fn serve(addr: SocketAddr, state: AppState) -> Result<()> {
    let app = Router::new()
        .route("/", get(|| async { Html(include_str!("../../web/index.html")) }))
        .route(
            "/client.js",
            get(|| async { ([("content-type", "text/javascript")], include_str!("../../web/client.js")) }),
        )
        .route("/ws", get(ws_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        eprintln!("server: client connected");
        if let Err(e) = client(socket, &state).await {
            // A tab closed or a cable pulled resets the connection; not worth an error.
            let reset = e.chain().any(|c| c.to_string().contains("without closing handshake"));
            if !reset {
                eprintln!("server: client error: {e:#}");
            }
        }
        let _ = state.input.send(InputCmd::ReleaseAll);
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

async fn client(mut socket: WebSocket, state: &AppState) -> Result<()> {
    let mut frames = state.frames.subscribe();
    let mut position = state.cursor_position.clone();
    let mut image = state.cursor_image.clone();

    let config = serde_json::json!({ "w": state.width, "h": state.height });
    let mut msg = vec![MSG_CONFIG];
    msg.extend_from_slice(config.to_string().as_bytes());
    socket.send(Message::Binary(msg.into())).await?;
    let (current_image, current_position) = (image.borrow_and_update().clone(), *position.borrow_and_update());
    socket.send(cursor_image_message(&current_image)).await?;
    socket.send(cursor_move_message(current_position)).await?;

    // Decoding can only start at a keyframe.
    let mut need_keyframe = true;
    state.encoder.request_keyframe();

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(incoming) = incoming else { return Ok(()) };
                let Message::Binary(data) = incoming? else { continue };
                match data.first() {
                    Some(&MSG_TOUCH) => {
                        if let Some(cmd) = parse_touch(&data[1..]) {
                            let _ = state.input.send(cmd);
                        }
                    }
                    Some(&MSG_KEYFRAME_REQUEST) => {
                        need_keyframe = true;
                        state.encoder.request_keyframe();
                    }
                    Some(&MSG_PING) => socket.send(Message::Binary(data)).await?,
                    Some(&MSG_HELLO) => eprintln!("server: hello {}", String::from_utf8_lossy(&data[1..])),
                    _ => {}
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
                    state.encoder.request_keyframe();
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

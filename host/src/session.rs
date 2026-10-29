//! One virtual monitor at a time, created for the connected client at its
//! screen size and removed a little while after the last client leaves.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{broadcast, watch};

use crate::capture::{self, CursorChannels};
use crate::display;
use crate::encoder::{Codec, EncodedFrame, Encoder};
use crate::input::{self, InputCmd};
use crate::mutter::{CursorMode, VirtualMonitor};

/// How long the monitor outlives its last client, so a reload or a quick
/// replug doesn't shuffle windows around.
const IDLE_TEARDOWN: Duration = Duration::from_secs(10);

pub struct Settings {
    pub codec: Codec,
    pub bitrate_kbps: u32,
    /// Overrides the size clients ask for.
    pub fixed_size: Option<(u32, u32)>,
}

/// Everything tied to one virtual monitor.
pub struct Session {
    pub width: u32,
    pub height: u32,
    pub encoder: Arc<Encoder>,
    pub input: Sender<InputCmd>,
    /// Mutter's name for the monitor, e.g. `Meta-0`.
    pub connector: String,
    vm: Arc<VirtualMonitor>,
}

impl Session {
    fn stop(&self) {
        self.encoder.stop();
        self.vm.stop();
        eprintln!("session: virtual monitor {} ({}x{}) removed", self.connector, self.width, self.height);
    }
}

pub struct Manager {
    settings: Settings,
    pub frames: broadcast::Sender<Arc<EncodedFrame>>,
    pub cursor: CursorChannels,
    /// The active session; clients watch it to follow restarts and resizes.
    current: watch::Sender<Option<Arc<Session>>>,
    /// Serializes starting and stopping sessions.
    lifecycle: Mutex<()>,
    /// Connected clients, and a counter bumped on every connect so a pending
    /// idle teardown can tell that someone came back.
    clients: Mutex<(usize, u64)>,
}

impl Manager {
    pub fn new(settings: Settings) -> Arc<Self> {
        let (frames, _) = broadcast::channel(8);
        Arc::new(Self {
            settings,
            frames,
            cursor: CursorChannels::default(),
            current: watch::channel(None).0,
            lifecycle: Mutex::new(()),
            clients: Mutex::new((0, 0)),
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<Session>>> {
        self.current.subscribe()
    }

    /// The session for a client whose screen is `requested` pixels, starting
    /// or resizing the virtual monitor if needed. Blocks on D-Bus.
    pub fn acquire(self: &Arc<Self>, requested: (u32, u32)) -> Result<Arc<Session>> {
        let (width, height) = self.settings.fixed_size.unwrap_or(requested);
        let _lifecycle = self.lifecycle.lock().unwrap();
        let current = self.current.borrow().clone();
        if let Some(session) = current {
            if (session.width, session.height) == (width, height) {
                return Ok(session);
            }
            // Mutter reuses the connector, so remove the old monitor first.
            session.stop();
        }
        let session = self.start(width, height)?;
        self.current.send_replace(Some(session.clone()));
        Ok(session)
    }

    fn start(self: &Arc<Self>, width: u32, height: u32) -> Result<Arc<Session>> {
        let codec = self.settings.codec.resolve();
        let encoder = Encoder::new(codec, self.settings.bitrate_kbps, self.frames.clone())?;
        let display = zbus::blocking::Connection::session()?;
        let before = display::scales(&display)?;
        let vm = Arc::new(VirtualMonitor::start(CursorMode::Metadata)?);
        let capture = capture::spawn(vm.node_id, width, height, encoder.clone(), self.cursor.clone());
        // The monitor appears once capture has negotiated its size.
        let connector = display::wait_for_new_connector(&display, &before)?;
        let scale = display::watch_scale(display, connector.clone())?;
        eprintln!(
            "session: virtual monitor {connector} {width}x{height} active (scale {}, {codec:?}, PipeWire node {})",
            scale.get(),
            vm.node_id
        );
        let input = input::spawn(vm.clone(), width, height, scale);
        let session = Arc::new(Session { width, height, encoder, input, connector, vm });

        // If Mutter ends the stream on its own, forget the session so the next
        // client request starts a fresh one.
        let manager: Weak<Self> = Arc::downgrade(self);
        let ended: Weak<Session> = Arc::downgrade(&session);
        std::thread::Builder::new().name("capture-watch".into()).spawn(move || {
            if let Ok(Err(e)) = capture.join() {
                eprintln!("session: capture failed: {e:#}");
            }
            if let (Some(manager), Some(ended)) = (manager.upgrade(), ended.upgrade()) {
                let _lifecycle = manager.lifecycle.lock().unwrap();
                manager.current.send_if_modified(|current| {
                    let is_ended = current.as_ref().is_some_and(|s| Arc::ptr_eq(s, &ended));
                    if is_ended {
                        eprintln!("session: stream ended");
                        *current = None;
                    }
                    is_ended
                });
            }
        })?;
        Ok(session)
    }

    pub fn client_connected(&self) {
        let mut clients = self.clients.lock().unwrap();
        clients.0 += 1;
        clients.1 += 1;
    }

    /// Remove the monitor if no client reconnects within [`IDLE_TEARDOWN`].
    pub fn client_disconnected(self: &Arc<Self>) {
        let generation = {
            let mut clients = self.clients.lock().unwrap();
            clients.0 -= 1;
            if clients.0 > 0 {
                return;
            }
            clients.1
        };
        let manager = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(IDLE_TEARDOWN).await;
            let idle = *manager.clients.lock().unwrap() == (0, generation);
            if idle {
                let _ = tokio::task::spawn_blocking(move || manager.stop()).await;
            }
        });
    }

    /// Remove the virtual monitor, if any. Blocks on D-Bus.
    pub fn stop(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        if let Some(session) = self.current.send_replace(None) {
            session.stop();
        }
    }
}

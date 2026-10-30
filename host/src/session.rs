//! One stream's session: a virtual monitor created for the connected client at
//! its screen size, or a mirror of an existing monitor. Started by the first
//! client, removed a little while after the last one leaves.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{broadcast, watch};

use crate::capture::{self, CursorChannels};
use crate::display;
use crate::encoder::{Codec, EncodedFrame, Encoder};
use crate::input::{self, InputCmd};
use crate::mutter::{CursorMode, ScreenStream, Source};

/// How long the session outlives its last client, so a reload or a quick
/// replug doesn't shuffle windows around.
const IDLE_TEARDOWN: Duration = Duration::from_secs(10);

pub struct Settings {
    /// Stream name, used in URLs (`/s/<name>/`) and logs.
    pub name: String,
    pub source: Source,
    pub codec: Codec,
    pub bitrate_kbps: u32,
    /// Overrides the size clients ask for (virtual monitors only).
    pub fixed_size: Option<(u32, u32)>,
    /// Accept touch input from clients. Off for view-only streams.
    pub touch: bool,
}

/// Everything tied to one running stream.
pub struct Session {
    pub width: u32,
    pub height: u32,
    pub encoder: Arc<Encoder>,
    pub input: Sender<InputCmd>,
    /// Mutter's name for the monitor, e.g. `Meta-0` or `HDMI-1`.
    pub connector: String,
    stream: Arc<ScreenStream>,
    name: String,
}

impl Session {
    fn stop(&self) {
        self.encoder.stop();
        self.stream.stop();
        eprintln!("{}: stopped {} ({}x{})", self.name, self.connector, self.width, self.height);
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

    pub fn name(&self) -> &str {
        &self.settings.name
    }

    pub fn source(&self) -> &Source {
        &self.settings.source
    }

    pub fn touch_enabled(&self) -> bool {
        self.settings.touch
    }

    /// Mirrors run from startup and stay up without viewers: Mutter only sends
    /// a mirrored monitor's frames when its content changes, so a mirror started
    /// on demand would show nothing until then. Kept running, the latest frame
    /// is always ready for a new viewer.
    pub fn always_on(&self) -> bool {
        matches!(self.settings.source, Source::Mirror(_))
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<Session>>> {
        self.current.subscribe()
    }

    /// The session for a client whose screen is `requested` pixels, starting
    /// it, or resizing a virtual monitor, if needed. Blocks on D-Bus.
    pub fn acquire(self: &Arc<Self>, requested: (u32, u32)) -> Result<Arc<Session>> {
        let name = &self.settings.name;
        let (width, height) = self.settings.fixed_size.unwrap_or(requested);
        let _lifecycle = self.lifecycle.lock().unwrap();
        let current = self.current.borrow().clone();
        if let Some(session) = current {
            // A mirror has the mirrored monitor's size, whatever clients ask for.
            if matches!(self.settings.source, Source::Mirror(_)) || (session.width, session.height) == (width, height) {
                return Ok(session);
            }
            // With several viewers, the first one's size stays: resizing would
            // move windows around for everyone. A lone client may resize (rotation).
            let clients = self.clients.lock().unwrap().0;
            if clients > 1 {
                eprintln!(
                    "{name}: keeping {}x{} for {clients} clients (asked for {width}x{height})",
                    session.width, session.height
                );
                return Ok(session);
            }
            // Mutter reuses the connector, so remove the old monitor first.
            session.stop();
        }
        let session = self.start((width, height))?;
        self.current.send_replace(Some(session.clone()));
        Ok(session)
    }

    fn start(self: &Arc<Self>, requested: (u32, u32)) -> Result<Arc<Session>> {
        let Settings { name, source, codec, bitrate_kbps, .. } = &self.settings;
        let codec = codec.resolve();
        let encoder = Encoder::new(codec, *bitrate_kbps, self.frames.clone())?;
        let display = zbus::blocking::Connection::session()?;
        let before = display::scales(&display)?;
        let stream = Arc::new(ScreenStream::start(source, CursorMode::Metadata)?);
        let (negotiated_tx, negotiated) = channel();
        let size = match source {
            Source::Virtual => Some(requested),
            Source::Mirror(_) => None,
        };
        let capture = capture::spawn(stream.node_id, size, encoder.clone(), self.cursor.clone(), negotiated_tx);
        let (width, height) =
            negotiated.recv_timeout(Duration::from_secs(5)).context("the stream never negotiated a frame size")?;
        let connector = match source {
            // The virtual monitor appears once capture has negotiated its size.
            Source::Virtual => display::wait_for_new_connector(&display, &before)?,
            Source::Mirror(connector) => connector.clone(),
        };
        let scale = display::watch_scale(display, connector.clone())?;
        let what = match source {
            Source::Virtual => "virtual monitor",
            Source::Mirror(_) => "mirroring",
        };
        eprintln!(
            "{name}: {what} {connector} {width}x{height} (scale {}, {codec:?}, touch {}, PipeWire node {})",
            scale.get(),
            if self.settings.touch { "on" } else { "off" },
            stream.node_id
        );
        let input = input::spawn(stream.clone(), width, height, scale);
        let session = Arc::new(Session { width, height, encoder, input, connector, stream, name: name.clone() });

        self.watch_session(&session, capture, negotiated)?;
        Ok(session)
    }

    /// Forget the session when its stream ends (Mutter stopped it, the mirrored
    /// monitor was unplugged) or its size changes (mirrored monitor's mode
    /// changed), so clients start a fresh one.
    fn watch_session(
        self: &Arc<Self>,
        session: &Arc<Session>,
        capture: std::thread::JoinHandle<Result<()>>,
        negotiated: Receiver<(u32, u32)>,
    ) -> Result<()> {
        let size = (session.width, session.height);
        let (manager, ended) = (Arc::downgrade(self), Arc::downgrade(session));
        std::thread::Builder::new().name("size-watch".into()).spawn(move || {
            if negotiated.iter().any(|s| s != size) {
                end(&manager, &ended, "frame size changed");
            }
        })?;
        let (manager, ended) = (Arc::downgrade(self), Arc::downgrade(session));
        std::thread::Builder::new().name("capture-watch".into()).spawn(move || {
            if let Ok(Err(e)) = capture.join() {
                eprintln!("capture failed: {e:#}");
            }
            end(&manager, &ended, "stream ended");
        })?;
        Ok(())
    }

    /// Returns how many clients are now connected.
    pub fn client_connected(&self) -> usize {
        let mut clients = self.clients.lock().unwrap();
        clients.0 += 1;
        clients.1 += 1;
        clients.0
    }

    /// Stop the session if no client reconnects within [`IDLE_TEARDOWN`].
    pub fn client_disconnected(self: &Arc<Self>) {
        let generation = {
            let mut clients = self.clients.lock().unwrap();
            clients.0 -= 1;
            eprintln!("{}: client disconnected ({} connected)", self.settings.name, clients.0);
            if clients.0 > 0 || self.always_on() {
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

    /// Stop the session, if any. Blocks on D-Bus.
    pub fn stop(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        if let Some(session) = self.current.send_replace(None) {
            session.stop();
        }
    }
}

/// Clear `ended` from its manager if it's still the current session.
fn end(manager: &Weak<Manager>, ended: &Weak<Session>, reason: &str) {
    let (Some(manager), Some(ended)) = (manager.upgrade(), ended.upgrade()) else { return };
    let _lifecycle = manager.lifecycle.lock().unwrap();
    let is_current = manager.current.borrow().as_ref().is_some_and(|s| Arc::ptr_eq(s, &ended));
    if is_current {
        eprintln!("{}: {reason}", manager.settings.name);
        ended.stop();
        manager.current.send_replace(None);
    }
}

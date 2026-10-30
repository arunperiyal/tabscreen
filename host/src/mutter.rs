//! Screen streams (a new virtual monitor, or a mirror of an existing one) and
//! input injection through Mutter's own D-Bus APIs
//! (`org.gnome.Mutter.RemoteDesktop` + `org.gnome.Mutter.ScreenCast`).
//!
//! The xdg-desktop-portal can create a virtual monitor but rejects touch on it,
//! see PLAN.md (M0). Mutter ends the session when this D-Bus connection closes.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, Value};

const RD_BUS: &str = "org.gnome.Mutter.RemoteDesktop";
const RD_SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";
const SC_BUS: &str = "org.gnome.Mutter.ScreenCast";

/// How the cursor reaches the consumer of the stream.
#[derive(Clone, Copy, Debug)]
pub enum CursorMode {
    Hidden = 0,
    /// Drawn into the frames. On virtual monitors pointer-only movement
    /// produces no new frames, so the cursor stutters.
    Embedded = 1,
    /// Sent as `SPA_META_Cursor` on (possibly empty) buffers.
    Metadata = 2,
}

/// What a stream shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A new virtual monitor, sized by the PipeWire consumer.
    Virtual,
    /// An existing monitor, by connector name (e.g. `HDMI-1`), at its own size.
    Mirror(String),
}

pub struct ScreenStream {
    _conn: Connection,
    rd_session: Proxy<'static>,
    stream_path: OwnedObjectPath,
    /// PipeWire node carrying the monitor's frames.
    pub node_id: u32,
}

impl ScreenStream {
    /// Start a stream. A virtual monitor's size is decided by the format the
    /// PipeWire consumer negotiates, not here.
    pub fn start(source: &Source, cursor_mode: CursorMode) -> Result<Self> {
        let conn = Connection::session()?;
        let rd = Proxy::new(&conn, RD_BUS, "/org/gnome/Mutter/RemoteDesktop", RD_BUS)?;
        let rd_path: OwnedObjectPath = rd.call("CreateSession", &()).context("RemoteDesktop.CreateSession")?;
        let rd_session = Proxy::new_owned(conn.clone(), RD_BUS, rd_path, RD_SESSION)?;
        let session_id: String = rd_session.get_property("SessionId")?;

        let sc = Proxy::new(&conn, SC_BUS, "/org/gnome/Mutter/ScreenCast", SC_BUS)?;
        let props = HashMap::from([("remote-desktop-session-id", Value::from(session_id.as_str()))]);
        let sc_path: OwnedObjectPath = sc.call("CreateSession", &(props,)).context("ScreenCast.CreateSession")?;
        let sc_session = Proxy::new(&conn, SC_BUS, sc_path.as_str(), "org.gnome.Mutter.ScreenCast.Session")?;
        let props = HashMap::from([("cursor-mode", Value::from(cursor_mode as u32))]);
        let stream_path: OwnedObjectPath = match source {
            Source::Virtual => sc_session.call("RecordVirtual", &(props,)).context("RecordVirtual")?,
            Source::Mirror(connector) => sc_session
                .call("RecordMonitor", &(connector.as_str(), props))
                .with_context(|| format!("RecordMonitor {connector} (see --list-monitors)"))?,
        };

        let stream = Proxy::new_owned(conn.clone(), SC_BUS, stream_path.clone(), "org.gnome.Mutter.ScreenCast.Stream")?;
        let mut added = stream.receive_signal("PipeWireStreamAdded")?;
        rd_session.call_method("Start", &()).context("RemoteDesktop.Session.Start")?;
        let msg = added.next().ok_or_else(|| anyhow!("no PipeWireStreamAdded signal"))?;
        let node_id: u32 = msg.body().deserialize()?;

        Ok(Self { _conn: conn, rd_session, stream_path, node_id })
    }

    pub fn touch_down(&self, slot: u32, x: f64, y: f64) -> Result<()> {
        self.rd_session.call_method("NotifyTouchDown", &(self.stream_path.as_str(), slot, x, y))?;
        Ok(())
    }

    pub fn touch_motion(&self, slot: u32, x: f64, y: f64) -> Result<()> {
        self.rd_session.call_method("NotifyTouchMotion", &(self.stream_path.as_str(), slot, x, y))?;
        Ok(())
    }

    pub fn touch_up(&self, slot: u32) -> Result<()> {
        self.rd_session.call_method("NotifyTouchUp", &(slot,))?;
        Ok(())
    }

    pub fn pointer_motion_absolute(&self, x: f64, y: f64) -> Result<()> {
        self.rd_session.call_method("NotifyPointerMotionAbsolute", &(self.stream_path.as_str(), x, y))?;
        Ok(())
    }

    pub fn stop(&self) {
        // Fails harmlessly if Mutter already ended the session (stream consumer gone).
        let _ = self.rd_session.call_method("Stop", &());
    }
}

impl Drop for ScreenStream {
    fn drop(&mut self) {
        self.stop();
    }
}

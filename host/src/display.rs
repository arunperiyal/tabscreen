//! Monitor layout from Mutter's DisplayConfig: which connector is ours, and
//! its current scale. Touch coordinates are in logical pixels, so a monitor
//! at 200 % needs device pixels halved (see PLAN.md, M2).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

type Mode = (String, i32, i32, f64, f64, Vec<f64>, HashMap<String, OwnedValue>);
type MonitorSpec = (String, String, String, String);
type Monitor = (MonitorSpec, Vec<Mode>, HashMap<String, OwnedValue>);
type LogicalMonitor = (i32, i32, f64, u32, bool, Vec<MonitorSpec>, HashMap<String, OwnedValue>);
type State = (u32, Vec<Monitor>, Vec<LogicalMonitor>, HashMap<String, OwnedValue>);

fn proxy(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new(conn, "org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig", "org.gnome.Mutter.DisplayConfig")
}

/// Active connectors and their scales.
pub fn scales(conn: &Connection) -> Result<HashMap<String, f64>> {
    let (_serial, _monitors, logical, _props): State = proxy(conn)?.call("GetCurrentState", &())?;
    Ok(logical
        .into_iter()
        .flat_map(|(_x, _y, scale, _transform, _primary, monitors, _props)| {
            monitors.into_iter().map(move |(connector, ..)| (connector, scale))
        })
        .collect())
}

/// Wait for a connector that isn't in `before` to appear (our new virtual monitor).
pub fn wait_for_new_connector(conn: &Connection, before: &HashMap<String, f64>) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(connector) = scales(conn)?.into_keys().find(|c| !before.contains_key(c)) {
            return Ok(connector);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("virtual monitor did not appear in the display configuration")
}

/// A monitor's scale, kept current as the user changes it in Settings.
pub struct Scale(AtomicU64);

impl Scale {
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Relaxed))
    }
}

/// Track `connector`'s scale until the returned `Scale` is dropped.
pub fn watch_scale(conn: Connection, connector: String) -> Result<Arc<Scale>> {
    let initial = scales(&conn)?.get(&connector).copied().unwrap_or(1.0);
    let scale = Arc::new(Scale(AtomicU64::new(initial.to_bits())));
    let weak: Weak<Scale> = Arc::downgrade(&scale);
    let changes = proxy(&conn)?.receive_signal("MonitorsChanged")?;
    std::thread::Builder::new().name("scale-watch".into()).spawn(move || {
        for _ in changes {
            let Some(scale) = weak.upgrade() else { return };
            match scales(&conn).map(|s| s.get(&connector).copied()) {
                Ok(Some(new)) if new != scale.get() => {
                    eprintln!("display: {connector} scale {new}");
                    scale.0.store(new.to_bits(), Relaxed);
                }
                Ok(Some(_)) => {}
                Ok(None) => return, // our monitor is gone
                Err(e) => eprintln!("display: {e:#}"),
            }
        }
    })?;
    Ok(scale)
}

/// A monitor as shown by `--list-monitors`.
pub struct MonitorInfo {
    pub connector: String,
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub virtual_: bool,
}

/// Active monitors with their names and current modes.
pub fn monitors(conn: &Connection) -> Result<Vec<MonitorInfo>> {
    let (_serial, monitors, _logical, _props): State = proxy(conn)?.call("GetCurrentState", &())?;
    let scales = scales(conn)?;
    Ok(monitors
        .into_iter()
        .filter_map(|((connector, vendor, ..), modes, props)| {
            let scale = *scales.get(&connector)?; // only active monitors
            let (_, width, height, ..) = modes.into_iter().find(|m| {
                m.6.get("is-current").and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
            })?;
            let name = props
                .get("display-name")
                .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
                .unwrap_or_default();
            Some(MonitorInfo { virtual_: vendor == "MetaVendor", connector, name, width, height, scale })
        })
        .collect())
}

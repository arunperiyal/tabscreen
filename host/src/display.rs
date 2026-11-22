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

/// Where an unscaled, unrotated monitor sits on the desktop: `(x, y, width, height)`
/// in logical pixels. `None` for a scaled or rotated monitor, whose area capture
/// has not been verified (callers fall back to capturing the monitor itself).
pub fn plain_monitor_rect(conn: &Connection, connector: &str) -> Result<Option<(i32, i32, i32, i32)>> {
    let (_serial, monitors, logical, _props): State = proxy(conn)?.call("GetCurrentState", &())?;
    Ok(logical.into_iter().find_map(|(x, y, scale, transform, _primary, members, _props)| {
        members.iter().find(|(c, ..)| c == connector)?;
        let (_, modes, _) = monitors.iter().find(|m| m.0.0 == connector)?;
        let (_, w, h, ..) = modes
            .iter()
            .find(|m| m.6.get("is-current").and_then(|v| bool::try_from(v).ok()).unwrap_or(false))?;
        rect_if_plain(x, y, scale, transform, *w, *h)
    }))
}

/// The rectangle for a monitor with this scale and transform, if it is plain.
fn rect_if_plain(x: i32, y: i32, scale: f64, transform: u32, width: i32, height: i32) -> Option<(i32, i32, i32, i32)> {
    ((scale - 1.0).abs() < 1e-6 && transform == 0).then_some((x, y, width, height))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_monitors_get_an_area() {
        assert_eq!(rect_if_plain(1920, 0, 1.0, 0, 1920, 1080), Some((1920, 0, 1920, 1080)));
        assert_eq!(rect_if_plain(0, 0, 2.0, 0, 3840, 2160), None, "scaled");
        assert_eq!(rect_if_plain(0, 0, 1.25, 0, 1920, 1080), None, "fractional scale");
        assert_eq!(rect_if_plain(0, 0, 1.0, 1, 1080, 1920), None, "rotated");
    }
}

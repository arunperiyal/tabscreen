//! Per-viewer counters, logged once per stream every few seconds.
//!
//! The frame-ack delay is measured on the host (time from sending a frame to the
//! client's ack), so it needs no clock agreement with the client and includes the
//! network queue; it is what the viewer's own `rtt` on the status line approximates.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

/// Counters for one connected viewer, updated by its connection task.
pub struct ViewerStats {
    pub peer: SocketAddr,
    frames: AtomicU64,
    bytes: AtomicU64,
    skipped: AtomicU64,
    delay_sum_us: AtomicU64,
    delay_n: AtomicU64,
    delay_max_us: AtomicU64,
}

/// What a viewer received since the last report.
#[derive(Debug, Default, PartialEq)]
pub struct Snapshot {
    pub frames: u64,
    pub bytes: u64,
    pub skipped: u64,
    /// Average and maximum frame-ack delay, if any acks arrived.
    pub delay: Option<(Duration, Duration)>,
}

impl ViewerStats {
    pub fn new(peer: SocketAddr) -> Self {
        Self {
            peer,
            frames: 0.into(),
            bytes: 0.into(),
            skipped: 0.into(),
            delay_sum_us: 0.into(),
            delay_n: 0.into(),
            delay_max_us: 0.into(),
        }
    }

    /// A frame of `bytes` was sent to the viewer.
    pub fn sent(&self, bytes: usize) {
        self.frames.fetch_add(1, Relaxed);
        self.bytes.fetch_add(bytes as u64, Relaxed);
    }

    /// A frame was dropped for this viewer because it was slow.
    pub fn skipped(&self) {
        self.skipped.fetch_add(1, Relaxed);
    }

    /// The viewer acked a frame sent `delay` ago.
    pub fn acked(&self, delay: Duration) {
        let us = delay.as_micros() as u64;
        self.delay_sum_us.fetch_add(us, Relaxed);
        self.delay_n.fetch_add(1, Relaxed);
        self.delay_max_us.fetch_max(us, Relaxed);
    }

    /// Read and reset the counters.
    pub fn take(&self) -> Snapshot {
        let n = self.delay_n.swap(0, Relaxed);
        let sum = self.delay_sum_us.swap(0, Relaxed);
        let max = self.delay_max_us.swap(0, Relaxed);
        Snapshot {
            frames: self.frames.swap(0, Relaxed),
            bytes: self.bytes.swap(0, Relaxed),
            skipped: self.skipped.swap(0, Relaxed),
            delay: (n > 0).then(|| (Duration::from_micros(sum / n), Duration::from_micros(max))),
        }
    }
}

/// One log line for a stream: `name: 2 viewers | 10.0.0.5: 58 fps, 4.1 Mbit/s, ack delay
/// 12 ms (max 40), skipped 0 | ...`. `secs` is the time the snapshots cover.
pub fn format_line(name: &str, secs: f64, viewers: &[(SocketAddr, Snapshot)]) -> String {
    let mut line = format!("{name}: {} viewer{}", viewers.len(), if viewers.len() == 1 { "" } else { "s" });
    for (peer, s) in viewers {
        let delay = match s.delay {
            Some((avg, max)) => format!("{:.0} ms (max {:.0})", avg.as_secs_f64() * 1e3, max.as_secs_f64() * 1e3),
            None => "no acks".to_string(),
        };
        line += &format!(
            " | {}: {:.0} fps, {:.1} Mbit/s, ack delay {delay}, skipped {}",
            peer.ip().to_canonical(),
            s.frames as f64 / secs,
            s.bytes as f64 * 8.0 / secs / 1e6,
            s.skipped
        );
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> SocketAddr {
        "192.168.1.7:5000".parse().unwrap()
    }

    #[test]
    fn take_reports_and_resets() {
        let s = ViewerStats::new(peer());
        s.sent(1000);
        s.sent(500);
        s.skipped();
        s.acked(Duration::from_millis(10));
        s.acked(Duration::from_millis(30));
        let snap = s.take();
        assert_eq!((snap.frames, snap.bytes, snap.skipped), (2, 1500, 1));
        assert_eq!(snap.delay, Some((Duration::from_millis(20), Duration::from_millis(30))));
        assert_eq!(s.take(), Snapshot::default(), "counters start over");
    }

    #[test]
    fn line_shows_each_viewer() {
        let snap = Snapshot { frames: 580, bytes: 5_125_000, skipped: 3, delay: Some((Duration::from_millis(12), Duration::from_millis(40))) };
        let line = format_line("audience", 10.0, &[(peer(), snap), (peer(), Snapshot::default())]);
        assert_eq!(
            line,
            "audience: 2 viewers | 192.168.1.7: 58 fps, 4.1 Mbit/s, ack delay 12 ms (max 40), skipped 3 \
             | 192.168.1.7: 0 fps, 0.0 Mbit/s, ack delay no acks, skipped 0"
        );
    }
}

//! Per-client record of video frames sent but not yet acknowledged.
//!
//! The client acks each frame's `pts_us` when it arrives. The difference between
//! what was sent and what was acked is the backlog sitting in the network path;
//! the server uses it to decide when a viewer is too slow (see M2b in the plan).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// More than this many frames unacked, and the oldest older than `SLOW_AFTER`:
/// the viewer is behind. The count keeps a normal Wi‑Fi round trip (a few frames
/// at 60 fps) from looking slow.
const MAX_FRAMES: usize = 3;
const SLOW_AFTER: Duration = Duration::from_millis(100);
/// A keyframe is large and legitimately takes longer to arrive; without extra
/// room a slow link would never get past one (skip, resync, skip again).
const KEYFRAME_SLOW_AFTER: Duration = Duration::from_millis(1000);

struct Sent {
    pts_us: u64,
    bytes: usize,
    at: Instant,
    keyframe: bool,
}

#[derive(Default)]
pub struct InFlight {
    /// Frames sent, oldest first.
    sent: VecDeque<Sent>,
    bytes: usize,
}

impl InFlight {
    /// Record a frame handed to the socket.
    pub fn sent(&mut self, pts_us: u64, bytes: usize, keyframe: bool, at: Instant) {
        self.sent.push_back(Sent { pts_us, bytes, at, keyframe });
        self.bytes += bytes;
    }

    /// The client received the frame with `pts_us` (and so everything before it,
    /// since TCP delivers in order).
    pub fn acked(&mut self, pts_us: u64) {
        while let Some(front) = self.sent.front() {
            if front.pts_us > pts_us {
                break;
            }
            self.bytes -= front.bytes;
            self.sent.pop_front();
        }
    }

    /// Forget everything, e.g. when a new session starts and the client resets.
    pub fn clear(&mut self) {
        self.sent.clear();
        self.bytes = 0;
    }

    /// Whether the viewer is too far behind to be sent more video.
    pub fn is_slow(&self, now: Instant) -> bool {
        let Some(oldest) = self.sent.front() else { return false };
        let limit = if oldest.keyframe { KEYFRAME_SLOW_AFTER } else { SLOW_AFTER };
        self.sent.len() > MAX_FRAMES && now.duration_since(oldest.at) > limit
    }

    /// Frames sent and not yet acked.
    pub fn frames(&self) -> usize {
        self.sent.len()
    }

    /// Bytes sent and not yet acked.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn ack_releases_that_frame_and_earlier_ones() {
        let t = Instant::now();
        let mut f = InFlight::default();
        f.sent(10, 100, false, t);
        f.sent(20, 200, false, t);
        f.sent(30, 300, false, t);
        assert_eq!((f.frames(), f.bytes()), (3, 600));
        f.acked(20);
        assert_eq!((f.frames(), f.bytes()), (1, 300));
        f.acked(30);
        assert_eq!((f.frames(), f.bytes()), (0, 0));
    }

    #[test]
    fn stale_or_unknown_ack_changes_nothing() {
        let t = Instant::now();
        let mut f = InFlight::default();
        f.sent(50, 10, false, t);
        f.acked(40);
        assert_eq!(f.frames(), 1);
        f.clear();
        f.acked(100);
        assert_eq!((f.frames(), f.bytes()), (0, 0));
    }

    #[test]
    fn slow_needs_both_a_backlog_and_an_old_frame() {
        let t = Instant::now();
        let mut f = InFlight::default();
        for i in 0..4 {
            f.sent(i, 10, false, ms(t, i * 16));
        }
        assert!(!f.is_slow(ms(t, 60)), "4 frames but the oldest is only 60 ms old");
        assert!(f.is_slow(ms(t, 150)));
        f.acked(1);
        assert!(!f.is_slow(ms(t, 150)), "two frames left: within the count budget");
        assert!(!InFlight::default().is_slow(t));
    }

    #[test]
    fn an_unacked_keyframe_gets_more_time() {
        let t = Instant::now();
        let mut f = InFlight::default();
        f.sent(0, 500_000, true, t);
        for i in 1..4 {
            f.sent(i, 10, false, ms(t, i * 16));
        }
        assert!(!f.is_slow(ms(t, 500)));
        assert!(f.is_slow(ms(t, 1100)));
    }
}

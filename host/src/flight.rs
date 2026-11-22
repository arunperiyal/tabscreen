//! Per-client record of video frames sent but not yet acknowledged.
//!
//! The client acks each frame's `pts_us` when it arrives. The difference between
//! what was sent and what was acked is the backlog sitting in the network path;
//! the server uses it to decide when a viewer is too slow (see M2b in the plan).

use std::collections::VecDeque;

#[derive(Default)]
pub struct InFlight {
    /// `(pts_us, bytes)` of frames sent, oldest first.
    sent: VecDeque<(u64, usize)>,
    bytes: usize,
}

impl InFlight {
    /// Record a frame handed to the socket.
    pub fn sent(&mut self, pts_us: u64, bytes: usize) {
        self.sent.push_back((pts_us, bytes));
        self.bytes += bytes;
    }

    /// The client received the frame with `pts_us` (and so everything before it,
    /// since TCP delivers in order).
    pub fn acked(&mut self, pts_us: u64) {
        while let Some(&(pts, bytes)) = self.sent.front() {
            if pts > pts_us {
                break;
            }
            self.bytes -= bytes;
            self.sent.pop_front();
        }
    }

    /// Forget everything, e.g. when a new session starts and the client resets.
    pub fn clear(&mut self) {
        self.sent.clear();
        self.bytes = 0;
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

    #[test]
    fn ack_releases_that_frame_and_earlier_ones() {
        let mut f = InFlight::default();
        f.sent(10, 100);
        f.sent(20, 200);
        f.sent(30, 300);
        assert_eq!((f.frames(), f.bytes()), (3, 600));
        f.acked(20);
        assert_eq!((f.frames(), f.bytes()), (1, 300));
        f.acked(30);
        assert_eq!((f.frames(), f.bytes()), (0, 0));
    }

    #[test]
    fn stale_or_unknown_ack_changes_nothing() {
        let mut f = InFlight::default();
        f.sent(50, 10);
        f.acked(40);
        assert_eq!(f.frames(), 1);
        f.clear();
        f.acked(100);
        assert_eq!((f.frames(), f.bytes()), (0, 0));
    }
}

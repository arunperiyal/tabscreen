//! Touch injection on its own thread, so slow D-Bus calls never block the server.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::display::Scale;
use crate::mutter::VirtualMonitor;

#[derive(Debug, Clone, Copy)]
pub enum TouchAction {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Debug)]
pub enum InputCmd {
    /// `x, y` normalized to `[0, 1]` across the monitor.
    Touch { action: TouchAction, slot: u32, x: f32, y: f32 },
    /// Lift every active finger (client disconnected).
    ReleaseAll,
}

pub fn spawn(vm: Arc<VirtualMonitor>, width: u32, height: u32, scale: Arc<Scale>) -> Sender<InputCmd> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || run(vm, width, height, &scale, rx))
        .expect("spawn input thread");
    tx
}

fn run(vm: Arc<VirtualMonitor>, width: u32, height: u32, scale: &Scale, rx: Receiver<InputCmd>) {
    let mut active = HashSet::new();
    // Mutter takes logical pixels: device pixels divided by the monitor's scale.
    let to_px = |v: f32, size: u32| v.clamp(0.0, 1.0) as f64 * (size - 1) as f64 / scale.get();
    for cmd in rx {
        let result = match cmd {
            InputCmd::Touch { action, slot, x, y } => {
                let (x, y) = (to_px(x, width), to_px(y, height));
                match action {
                    TouchAction::Down => {
                        active.insert(slot);
                        vm.touch_down(slot, x, y)
                    }
                    TouchAction::Move if active.contains(&slot) => vm.touch_motion(slot, x, y),
                    TouchAction::Move => Ok(()),
                    TouchAction::Up | TouchAction::Cancel => {
                        if active.remove(&slot) { vm.touch_up(slot) } else { Ok(()) }
                    }
                }
            }
            InputCmd::ReleaseAll => active.drain().try_for_each(|slot| vm.touch_up(slot)),
        };
        if let Err(e) = result {
            eprintln!("input: {e:#}");
        }
    }
}

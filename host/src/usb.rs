//! Keeps `adb reverse` set up for every connected Android device, so the
//! tablet's `localhost:PORT` reaches us even after the cable is replugged
//! (adb forgets reverse rules when a device disconnects).

use std::collections::HashSet;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

const POLL: Duration = Duration::from_secs(2);

/// Serials of devices that are connected and authorized.
fn devices() -> Result<HashSet<String>> {
    let out = Command::new("adb").arg("devices").output().context("running adb (is it installed and on PATH?)")?;
    if !out.status.success() {
        bail!("adb devices: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, state)| *state == "device")
        .map(|(serial, _)| serial.to_string())
        .collect())
}

fn reverse(serial: &str, port: u16) -> Result<()> {
    let spec = format!("tcp:{port}");
    let out = Command::new("adb").args(["-s", serial, "reverse", &spec, &spec]).output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn spawn(port: u16) {
    std::thread::Builder::new()
        .name("adb".into())
        .spawn(move || {
            let mut forwarded: HashSet<String> = HashSet::new();
            let mut failed: HashSet<String> = HashSet::new();
            loop {
                match devices() {
                    Ok(present) => {
                        for serial in forwarded.iter().filter(|s| !present.contains(*s)) {
                            eprintln!("usb: {serial} disconnected");
                        }
                        forwarded.retain(|s| present.contains(s));
                        failed.retain(|s| present.contains(s));
                        for serial in present.difference(&forwarded.clone()) {
                            match reverse(serial, port) {
                                Ok(()) => {
                                    eprintln!("usb: {serial}: localhost:{port} on the device now reaches this host");
                                    forwarded.insert(serial.clone());
                                }
                                // Log once per device, keep retrying quietly.
                                Err(e) if failed.insert(serial.clone()) => eprintln!("usb: {serial}: adb reverse failed: {e}"),
                                Err(_) => {}
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("usb: {e:#}; giving up on automatic port forwarding");
                        return;
                    }
                }
                std::thread::sleep(POLL);
            }
        })
        .expect("spawn adb thread");
}

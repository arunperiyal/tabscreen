use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::Parser;
use gstreamer as gst;
use tabscreen_host::capture::{self, CursorChannels, CursorImage};
use tabscreen_host::encoder::{Codec, Encoder};
use tabscreen_host::input;
use tabscreen_host::mutter::{CursorMode, VirtualMonitor};
use tabscreen_host::server::{self, AppState};
use tokio::sync::{broadcast, watch};

/// Use a tablet as a touch-enabled second monitor for GNOME.
#[derive(Parser)]
struct Args {
    /// Virtual monitor size; use the tablet's native resolution.
    #[arg(long, default_value = "1920x1200", value_parser = parse_size)]
    size: (u32, u32),
    #[arg(long, default_value_t = 8765)]
    port: u16,
    /// Address to listen on. The default only accepts local (USB) connections.
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    bind: IpAddr,
    #[arg(long, value_enum, default_value_t = Codec::X264)]
    encoder: Codec,
    /// Target bitrate in kbit/s.
    #[arg(long, default_value_t = 12000)]
    bitrate: u32,
    /// Forward the port to a USB-connected Android device (`adb reverse`).
    #[arg(long)]
    usb: bool,
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let (w, h): (u32, u32) = (w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?);
    // 4:2:0 encoding needs even dimensions.
    if w < 64 || h < 64 || w % 2 != 0 || h % 2 != 0 {
        return Err("width and height must be even and at least 64".into());
    }
    Ok((w, h))
}

fn adb_reverse(port: u16) -> Result<()> {
    let status = std::process::Command::new("adb")
        .args(["reverse", &format!("tcp:{port}"), &format!("tcp:{port}")])
        .status()
        .context("running adb (is it installed and on PATH?)")?;
    if !status.success() {
        bail!("adb reverse failed; is the tablet connected with USB debugging allowed? (`adb devices`)");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let (width, height) = args.size;
    gst::init()?;

    if args.usb {
        adb_reverse(args.port)?;
        eprintln!("usb: tablet's localhost:{0} now forwards to this machine's port {0}", args.port);
    }

    let (frames, _) = broadcast::channel(8);
    let encoder = Encoder::new(args.encoder, args.bitrate, frames.clone())?;

    let vm = Arc::new(VirtualMonitor::start(CursorMode::Metadata)?);
    eprintln!("tabscreen: virtual monitor {width}x{height} active (PipeWire node {})", vm.node_id);

    let (position_tx, position_rx) = watch::channel((0, 0));
    let (image_tx, image_rx) = watch::channel(Arc::new(CursorImage::default()));
    let capture = capture::spawn(
        vm.node_id,
        width,
        height,
        encoder.clone(),
        CursorChannels { position: position_tx, image: image_tx },
    );
    let input = input::spawn(vm.clone(), width, height);

    let state = AppState {
        width,
        height,
        frames,
        cursor_position: position_rx,
        cursor_image: image_rx,
        encoder: encoder.clone(),
        input,
    };
    let addr = SocketAddr::new(args.bind, args.port);
    eprintln!("tabscreen: open http://localhost:{} in the tablet's Chrome (Ctrl+C to stop)", args.port);

    let result = tokio::select! {
        r = server::serve(addr, state) => r,
        r = tokio::signal::ctrl_c() => r.map_err(Into::into),
        r = tokio::task::spawn_blocking(move || capture.join()) => match r {
            Ok(Ok(Ok(()))) => Err(anyhow::anyhow!("virtual monitor stream ended")),
            Ok(Ok(Err(e))) => Err(e),
            _ => Err(anyhow::anyhow!("capture thread failed")),
        },
    };

    eprintln!("tabscreen: stopping");
    encoder.stop();
    vm.stop();
    result
}

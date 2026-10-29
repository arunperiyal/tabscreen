use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::Result;
use clap::Parser;
use gstreamer as gst;
use tabscreen_host::encoder::Codec;
use tabscreen_host::server;
use tabscreen_host::session::{Manager, Settings};
use tabscreen_host::usb;

/// Use a tablet as a touch-enabled second monitor for GNOME.
#[derive(Parser)]
struct Args {
    /// Virtual monitor size. By default it matches the connected tablet's screen.
    #[arg(long, value_parser = parse_size)]
    size: Option<(u32, u32)>,
    #[arg(long, default_value_t = 8765)]
    port: u16,
    /// Address to listen on. The default only accepts local (USB) connections.
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    bind: IpAddr,
    #[arg(long, value_enum, default_value_t = Codec::Auto)]
    encoder: Codec,
    /// Target bitrate in kbit/s.
    #[arg(long, default_value_t = 12000)]
    bitrate: u32,
    /// Keep `adb reverse` set up for USB-connected Android devices.
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    gst::init()?;

    if args.usb {
        usb::spawn(args.port);
    }
    let manager = Manager::new(Settings { codec: args.encoder, bitrate_kbps: args.bitrate, fixed_size: args.size });
    eprintln!(
        "tabscreen: encoder {:?}; open http://localhost:{} in the tablet's Chrome (Ctrl+C to stop)",
        args.encoder.resolve(),
        args.port
    );

    let addr = SocketAddr::new(args.bind, args.port);
    let result = tokio::select! {
        r = server::serve(addr, manager.clone()) => r,
        r = tokio::signal::ctrl_c() => r.map_err(Into::into),
    };

    eprintln!("tabscreen: stopping");
    tokio::task::spawn_blocking(move || manager.stop()).await?;
    result
}

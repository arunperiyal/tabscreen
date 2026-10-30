use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Result, bail};
use clap::Parser;
use gstreamer as gst;
use tabscreen_host::encoder::Codec;
use tabscreen_host::mutter::Source;
use tabscreen_host::server::{self, Stream};
use tabscreen_host::session::{Manager, Settings};
use tabscreen_host::{display, usb, wifi};

/// Use a tablet as a touch-enabled second monitor for GNOME, or share a screen
/// with several viewers.
#[derive(Parser)]
#[command(after_help = "\
Streams:
  Without --stream there is one stream, \"main\": a virtual monitor sized by the
  tablet (or a mirror with --mirror). With --stream you define several, each
  with its own page and Wi-Fi link:

    --stream NAME=virtual[,view-only][,size=WxH]
    --stream NAME=mirror:CONNECTOR[,view-only]

  Example: tablet as a second monitor, projector shown to the audience:
    tabscreen-host --usb --wifi --stream tab=virtual --stream audience=mirror:HDMI-1,view-only")]
struct Args {
    /// A stream: NAME=virtual or NAME=mirror:CONNECTOR, plus options (repeatable).
    #[arg(long = "stream", value_name = "SPEC", value_parser = parse_stream)]
    streams: Vec<StreamSpec>,
    /// Mirror an existing monitor instead of creating a virtual one (see --list-monitors).
    #[arg(long, value_name = "CONNECTOR", conflicts_with = "streams")]
    mirror: Option<String>,
    /// View only: ignore touch from all clients (e.g. projecting to several viewers).
    #[arg(long, conflicts_with = "streams")]
    no_touch: bool,
    /// Virtual monitor size. By default it matches the connected tablet's screen.
    #[arg(long, value_parser = parse_size, conflicts_with_all = ["streams", "mirror"])]
    size: Option<(u32, u32)>,
    /// List monitors (connector names for --mirror) and exit.
    #[arg(long)]
    list_monitors: bool,
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
    /// Also accept clients over Wi‑Fi: HTTPS on all interfaces, protected by a
    /// token per stream in the URL (printed with a QR code at startup).
    #[arg(long)]
    wifi: bool,
    #[arg(long, default_value_t = 8766)]
    wifi_port: u16,
}

#[derive(Clone, Debug)]
struct StreamSpec {
    name: String,
    source: Source,
    touch: bool,
    size: Option<(u32, u32)>,
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

fn parse_stream(s: &str) -> Result<StreamSpec, String> {
    let (name, rest) = s.split_once('=').ok_or("expected NAME=virtual or NAME=mirror:CONNECTOR")?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("stream name {name:?}: use letters, digits, '-' and '_'"));
    }
    let mut parts = rest.split(',');
    let source = match parts.next().unwrap_or_default() {
        "virtual" => Source::Virtual,
        m => match m.strip_prefix("mirror:") {
            Some(connector) if !connector.is_empty() => Source::Mirror(connector.to_string()),
            _ => return Err(format!("unknown source {m:?}: use virtual or mirror:CONNECTOR")),
        },
    };
    let mut spec = StreamSpec { name: name.to_string(), source, touch: true, size: None };
    for option in parts {
        match option.split_once('=') {
            None if option == "view-only" => spec.touch = false,
            Some(("size", size)) if spec.source == Source::Virtual => spec.size = Some(parse_size(size)?),
            _ => return Err(format!("unknown option {option:?} (view-only, size=WxH for virtual)")),
        }
    }
    Ok(spec)
}

fn list_monitors() -> Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    println!("{:<10} {:>11} {:>6}  NAME", "CONNECTOR", "SIZE", "SCALE");
    for m in display::monitors(&conn)? {
        let note = if m.virtual_ { " (virtual)" } else { "" };
        let size = format!("{}x{}", m.width, m.height);
        println!("{:<10} {size:>11} {:>5}%  {}{note}", m.connector, (m.scale * 100.0).round(), m.name);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.list_monitors {
        return list_monitors();
    }
    gst::init()?;

    let specs = if args.streams.is_empty() {
        let source = args.mirror.clone().map_or(Source::Virtual, Source::Mirror);
        vec![StreamSpec { name: "main".into(), source, touch: !args.no_touch, size: args.size }]
    } else {
        args.streams.clone()
    };
    for (i, spec) in specs.iter().enumerate() {
        if specs[..i].iter().any(|s| s.name == spec.name) {
            bail!("two streams named {:?}", spec.name);
        }
    }
    warn_missing_monitors(&specs);

    if args.wifi {
        // rustls needs one crypto backend chosen per process.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let streams: Arc<Vec<Stream>> = Arc::new(
        specs
            .iter()
            .map(|spec| {
                let manager = Manager::new(Settings {
                    name: spec.name.clone(),
                    source: spec.source.clone(),
                    codec: args.encoder,
                    bitrate_kbps: args.bitrate,
                    fixed_size: spec.size,
                    touch: spec.touch,
                });
                let token = if args.wifi { Some(wifi::token(&spec.name)?.into()) } else { None };
                anyhow::Ok(Stream { manager, token })
            })
            .collect::<Result<_>>()?,
    );

    for stream in streams.iter().filter(|s| s.manager.always_on()) {
        let manager = stream.manager.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = manager.acquire((0, 0)) {
                eprintln!("warning: {}: {e:#}; retrying when a viewer connects", manager.name());
            }
        });
    }
    if args.usb {
        usb::spawn(args.port);
    }
    let wifi_server = if args.wifi { Some(prepare_wifi(&streams, args.wifi_port)?) } else { None };
    print_streams(&streams, args.port, args.encoder);

    let addr = SocketAddr::new(args.bind, args.port);
    let wifi_streams = streams.clone();
    let result = tokio::select! {
        r = server::serve(addr, streams.clone()) => r,
        r = async move {
            match wifi_server {
                Some((addr, access)) => server::serve_tls(addr, wifi_streams, &access).await,
                None => std::future::pending().await,
            }
        } => r,
        r = tokio::signal::ctrl_c() => r.map_err(Into::into),
    };

    eprintln!("tabscreen: stopping");
    tokio::task::spawn_blocking(move || streams.iter().for_each(|s| s.manager.stop())).await?;
    result
}

/// A projector may be plugged in later, so a missing mirror target is only a warning.
fn warn_missing_monitors(specs: &[StreamSpec]) {
    let Ok(conn) = zbus::blocking::Connection::session() else { return };
    let Ok(monitors) = display::monitors(&conn) else { return };
    for spec in specs {
        if let Source::Mirror(connector) = &spec.source
            && !monitors.iter().any(|m| &m.connector == connector)
        {
            eprintln!("warning: {}: no active monitor {connector:?} yet (see --list-monitors)", spec.name);
        }
    }
}

/// Page path of stream `i`; the first stream is also at `/`.
fn path(streams: &[Stream], i: usize) -> String {
    if i == 0 { "/".into() } else { format!("/s/{}/", streams[i].manager.name()) }
}

fn print_streams(streams: &[Stream], port: u16, encoder: Codec) {
    eprintln!("tabscreen: encoder {:?}; Ctrl+C to stop", encoder.resolve());
    for (i, stream) in streams.iter().enumerate() {
        let m = &stream.manager;
        let what = match m.source() {
            Source::Virtual => "virtual monitor".to_string(),
            Source::Mirror(connector) => format!("mirror of {connector}"),
        };
        let touch = if m.touch_enabled() { "touch" } else { "view only" };
        eprintln!("  {:<10} {what}, {touch}; USB: http://localhost:{port}{}", m.name(), path(streams, i));
    }
}

/// Load the certificate and print each stream's Wi‑Fi URL and QR code.
fn prepare_wifi(streams: &[Stream], port: u16) -> Result<(SocketAddr, wifi::Access)> {
    let ip = wifi::lan_ip();
    let access = wifi::certificate(ip)?;
    let host = ip.map_or_else(|| "<this-computer's-IP>".to_string(), |ip| ip.to_string());
    for (i, stream) in streams.iter().enumerate() {
        let token = stream.token.as_deref().unwrap_or_default();
        let url = format!("https://{host}:{port}{}#t={token}", path(streams, i));
        let power = if stream.manager.touch_enabled() { "see and control" } else { "see" };
        eprintln!("\nwifi: {} — anyone with this link can {power} it:\n\n  {url}\n", stream.manager.name());
        if ip.is_some() {
            eprintln!("{}", wifi::qr(&url)?);
        }
    }
    eprintln!("wifi: accept the certificate warning once (Advanced → Proceed).\n");
    // Listen on IPv6 and IPv4 where the system supports dual-stack sockets.
    Ok((SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port), access))
}

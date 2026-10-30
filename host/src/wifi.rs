//! Wi‑Fi access without adb: HTTPS with a self-signed certificate (Chrome only
//! allows WebCodecs on secure pages) and a secret token, because anyone on the
//! network can reach the port and would otherwise see the screen and inject touches.
//!
//! Both are kept in `~/.config/tabscreen/` so the tablet's bookmark and
//! certificate exception survive restarts.

use std::fs;
use std::io::Write;
use std::net::{IpAddr, UdpSocket};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub struct Access {
    pub cert_pem: String,
    pub key_pem: String,
    pub token: String,
}

fn config_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    let dir = base.join("tabscreen");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Write a file readable only by the user.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// The address other LAN devices reach us on: the source address of the
/// default route. Connecting a UDP socket sends no packets.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?; // TEST-NET-1, never routed anywhere real
    Some(socket.local_addr().ok()?.ip())
}

/// Load the token and certificate, creating them if needed. The certificate is
/// regenerated when `ip` isn't among its names (e.g. a new DHCP lease).
pub fn load_or_create(ip: Option<IpAddr>) -> Result<Access> {
    let dir = config_dir()?;

    let token_path = dir.join("token");
    let token = match fs::read_to_string(&token_path) {
        Ok(token) if token.trim().len() >= 16 => token.trim().to_string(),
        _ => {
            let bytes: [u8; 16] = rand::random();
            let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            write_private(&token_path, &token)?;
            token
        }
    };

    let (cert_path, key_path, names_path) = (dir.join("cert.pem"), dir.join("key.pem"), dir.join("cert-names"));
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    if let Some(ip) = ip {
        names.push(ip.to_string());
    }
    let existing = (fs::read_to_string(&cert_path), fs::read_to_string(&key_path), fs::read_to_string(&names_path));
    if let (Ok(cert_pem), Ok(key_pem), Ok(cert_names)) = existing {
        if names.iter().all(|n| cert_names.lines().any(|l| l == n)) {
            return Ok(Access { cert_pem, key_pem, token });
        }
    }

    let generated = rcgen::generate_simple_self_signed(names.clone()).context("generating certificate")?;
    let (cert_pem, key_pem) = (generated.cert.pem(), generated.signing_key.serialize_pem());
    write_private(&cert_path, &cert_pem)?;
    write_private(&key_path, &key_pem)?;
    write_private(&names_path, &names.join("\n"))?;
    eprintln!("wifi: new self-signed certificate for {} (the tablet will warn once)", names.join(", "));
    Ok(Access { cert_pem, key_pem, token })
}

/// The URL as a QR code for the terminal, so the tablet can scan it.
pub fn qr(url: &str) -> Result<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(url.as_bytes())?;
    Ok(code.render::<Dense1x2>().dark_color(Dense1x2::Light).light_color(Dense1x2::Dark).quiet_zone(true).build())
}

//! mDNS advertisement + QR-coded connection info.
//!
//! ## What this does and doesn't protect against
//!
//! Three layers, each covering a different gap:
//! - **mDNS** (`advertise`): zero-config discovery -- no manual IP entry.
//! - **Pairing token** (checked in [`crate::session::handshake`]): access
//!   control -- an unpaired client can't get in even if it finds the host.
//! - **Noise transport encryption** (`palmtop_proto::noise`, wired into
//!   `session.rs`): the video/input stream is no longer plaintext on the LAN.
//!
//! The Noise side still has a real, deliberate gap: the host's static public
//! key is advertised over **mDNS** (see `advertise`'s TXT record) so the
//! discovery UI can auto-fill it, alongside being in the QR/manual connect
//! string. mDNS is LAN-broadcast, not a truly out-of-band channel, so an
//! active attacker on the LAN could in principle spoof the advertisement and
//! get a client to TOFU-pin an attacker-controlled key -- the pairing token
//! alone doesn't close that gap, since it authenticates the *user*, not the
//! host's cryptographic identity.
//!
//! In-app camera QR scanning now exists (`QrScanActivity`), so a genuinely
//! out-of-band source for the key is finally available -- but this gap is
//! **not automatically closed by that**, and it would be wrong to read it as
//! closed. The client still accepts an mDNS-advertised pubkey through the
//! discovery path, so the spoofing scenario above is still reachable for a
//! user who taps a discovered host instead of scanning. Actually closing it
//! is a deliberate follow-up decision -- either stop trusting mDNS-sourced
//! keys outright, or treat only scan-sourced pairings as pinned -- with a
//! real usability cost either way, since discovery-plus-typed-token is the
//! faster path when it's safe.
//!
//! ## Why a pure-Rust mDNS crate instead of Avahi
//!
//! The plan (§3.4) names Avahi specifically, but the actual requirement is
//! mDNS/DNS-SD advertisement, not Avahi itself -- and not every distro runs
//! Avahi by default (some ship systemd-resolved's own mDNS responder
//! instead). `mdns-sd` advertises directly over UDP multicast with no
//! external daemon dependency, which is a better fit for the plan's
//! multi-distro goal (§7 compositor/distro matrix). Worth revisiting if a
//! real interop problem shows up.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use qrcode::render::{svg, unicode};
use qrcode::QrCode;

const SERVICE_TYPE: &str = "_palmtop._tcp.local.";

/// Registers the `_palmtop._tcp` mDNS service. Keeps the returned
/// `ServiceDaemon` alive for as long as advertisement should continue --
/// dropping it withdraws the registration.
pub fn advertise(port: u16, protocol_version: u16, noise_pubkey_hex: &str) -> Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new().context("start mDNS responder")?;

    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()))
        .unwrap_or_else(|_| "palmtop-host".to_string());
    let instance_name = format!("Palmtop on {hostname}");

    let txt = [
        ("protocol_version", protocol_version.to_string()),
        ("pubkey", noise_pubkey_hex.to_string()),
    ];
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &format!("{hostname}.local."),
        "", // empty = advertise on all local addresses
        port,
        &txt[..],
    )
    .context("build mDNS service info")?
    .enable_addr_auto();

    daemon.register(service).context("register mDNS service")?;
    println!("[mdns] advertising {instance_name} ({SERVICE_TYPE}) on port {port}");
    Ok(daemon)
}

/// Renders the connection info three ways: a terminal QR code, a scannable
/// **SVG file**, and plain text -- each covering a case the others don't.
///
/// The SVG is not redundant with the terminal QR, and the reason is worth
/// stating because it cost a real debugging session to find. The URI carries a
/// 64-hex-character Noise public key, which pushes the code to roughly a 57x57
/// module grid; `unicode::Dense1x2` packs two vertical modules into one
/// character cell, so on screen each module ends up about one cell wide and
/// half a cell tall -- non-square, and at a typical terminal font the whole
/// code spans only a few hundred physical pixels. A phone camera at any normal
/// holding distance simply cannot resolve that: the Android scanner detected
/// *nothing* against it, with no error anywhere, because nothing was broken --
/// the decoder just never had enough detail. Opening the SVG in a browser or
/// image viewer gives square modules at any size, which scans immediately.
///
/// Terminal QR stays first because when it does work it's the fastest path
/// (nothing to open); text stays last as the always-available fallback for
/// manual entry.
///
/// The file lands in `$XDG_RUNTIME_DIR` at mode 0600 deliberately: it embeds
/// the pairing token, so it belongs on a user-private tmpfs that the session
/// wipes on logout, not in a home directory where it would outlive its use.
pub fn render_connect_info(host: &str, port: u16, token: &str, noise_pubkey_hex: &str) -> Result<String> {
    let uri = format!("palmtop://{host}:{port}/{token}?pubkey={noise_pubkey_hex}");
    let code = QrCode::new(uri.as_bytes()).context("encode QR code")?;
    let qr_text = code
        .render::<unicode::Dense1x2>()
        .quiet_zone(true)
        .build();

    let svg_note = match write_qr_svg(&code) {
        Ok(path) => format!("  or open this, which scans far more reliably:\n    {}\n", path.display()),
        // Never fatal: the terminal QR and the manual-entry text both still
        // work, so a read-only or missing XDG_RUNTIME_DIR shouldn't stop the
        // daemon from starting.
        Err(e) => format!("  (couldn't write the scannable QR file: {e:#})\n"),
    };

    Ok(format!(
        "{qr_text}\n\
         Scan with the Palmtop app,\n\
         {svg_note}\
         or enter manually:\n  \
         host:   {host}\n  \
         port:   {port}\n  \
         token:  {token}\n  \
         pubkey: {noise_pubkey_hex}\n"
    ))
}

/// Writes the QR as an SVG to `$XDG_RUNTIME_DIR/palmtop-pair.svg`, 0600.
///
/// SVG rather than PNG because `qrcode`'s svg renderer needs no dependency
/// beyond what's already compiled in, and vector output means the code stays
/// crisp however far the viewer zooms -- which is the entire point of the file.
fn write_qr_svg(code: &QrCode) -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is not set")?;
    let path = dir.join("palmtop-pair.svg");

    let svg = code
        .render::<svg::Color>()
        .min_dimensions(720, 720)
        .quiet_zone(true)
        .build();

    // Create with 0600 up front rather than writing then chmod-ing, so the
    // token is never briefly readable by other users on the machine.
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(svg.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;

    Ok(path)
}

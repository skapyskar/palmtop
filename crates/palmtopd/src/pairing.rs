//! mDNS advertisement + QR-coded connection info.
//!
//! ## What this does and doesn't protect against
//!
//! This gives real access control (an unpaired client can't get past
//! [`crate::session::handshake`]) and a real zero-config discovery/connect
//! UX (scan the QR, no manual IP entry). It does **not** yet encrypt the
//! session -- the plan's full design (§3.4, §6) calls for a Noise handshake
//! with a static keypair and TOFU pinning, which is a substantial standalone
//! piece of work deserving its own pass rather than being rushed in here.
//! Until that lands: the token (and the whole video/input stream) is
//! plaintext on the LAN. Real pairing gate, not yet a real security boundary
//! against a hostile network -- don't oversell this in product copy.
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

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use qrcode::render::unicode;
use qrcode::QrCode;

const SERVICE_TYPE: &str = "_palmtop._tcp.local.";

/// Registers the `_palmtop._tcp` mDNS service. Keeps the returned
/// `ServiceDaemon` alive for as long as advertisement should continue --
/// dropping it withdraws the registration.
pub fn advertise(port: u16, protocol_version: u16) -> Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new().context("start mDNS responder")?;

    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()))
        .unwrap_or_else(|_| "palmtop-host".to_string());
    let instance_name = format!("Palmtop on {hostname}");

    let txt = [("protocol_version", protocol_version.to_string())];
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

/// Renders a terminal-displayable QR code encoding a `palmtop://host:port/token`
/// URI, plus the same info as plain text (some terminals/fonts render QR
/// unicode blocks poorly -- the text form is always there as a fallback).
pub fn render_connect_info(host: &str, port: u16, token: &str) -> Result<String> {
    let uri = format!("palmtop://{host}:{port}/{token}");
    let code = QrCode::new(uri.as_bytes()).context("encode QR code")?;
    let qr_text = code
        .render::<unicode::Dense1x2>()
        .quiet_zone(true)
        .build();

    Ok(format!(
        "{qr_text}\n\
         Scan with the Palmtop app, or enter manually:\n  \
         host:  {host}\n  \
         port:  {port}\n  \
         token: {token}\n"
    ))
}

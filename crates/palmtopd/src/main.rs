mod capture;
mod doctor;
mod encode;
mod input;
mod modes;
mod pairing;
mod session;

use std::sync::mpsc;
use std::thread;

use anyhow::Result;

fn main() -> Result<()> {
    // Loaded before the flag check so --doctor can report on the real config,
    // but tolerated as absent: diagnosing a machine whose config is broken is
    // exactly when the diagnostics are most useful, so a load failure must not
    // stop them running.
    let cfg = palmtop_config::HostConfig::load();

    if std::env::args().any(|a| a == "--doctor") {
        if let Err(e) = &cfg {
            eprintln!("[doctor] configuration could not be loaded: {e:#}\n");
        }
        let healthy = doctor::run(cfg.as_ref().ok())?;
        std::process::exit(if healthy { 0 } else { 1 });
    }

    let cfg = cfg?;
    println!(
        "[palmtopd] vaapi={} codec={} qp={} fps={} port={}",
        cfg.gpu.vaapi_render_node, cfg.encode.codec, cfg.encode.qp, cfg.encode.fps, cfg.host.port
    );

    // Probed once at startup rather than per session: it costs a few hundred
    // milliseconds, and finding out the GPU cannot encode is something the
    // operator should learn from the daemon's own startup log, not from a
    // phone that connects to a blank screen ten minutes later.
    let render_node = cfg.resolved_render_node()?;
    println!("[gpu] encoding on {render_node}");

    // Kept alive for the daemon's lifetime -- dropping it withdraws the
    // mDNS registration.
    let _mdns = pairing::advertise(
        cfg.host.port,
        palmtop_proto::PROTOCOL_VERSION,
        &cfg.pairing.noise_public_key,
    )?;

    let host_ip = cfg.resolved_ip()?;
    print!(
        "{}",
        pairing::render_connect_info(
            &host_ip,
            cfg.host.port,
            &cfg.pairing.token,
            &cfg.pairing.noise_public_key
        )?
    );

    // Only meaningful when the address is auto-detected; a pinned one is an
    // explicit instruction to leave alone. See watch_address.
    if cfg.host.ip.trim().is_empty() {
        pairing::watch_address(
            host_ip.clone(),
            cfg.host.port,
            cfg.pairing.token.clone(),
            cfg.pairing.noise_public_key.clone(),
        );
    }

    // Input injector lives for the daemon's lifetime, independent of any
    // particular client connection -- see input.rs.
    let (input_tx, input_rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(e) = input::run(input_rx) {
            eprintln!("[input] fatal: {e:#}");
        }
    });

    // One runtime for the daemon's lifetime, never dropped mid-session. The
    // portal's DBus connection (ashpd/zbus) lives inside it; dropping the
    // runtime while a screencast session is still in use -- even after the
    // fd has been extracted -- risks the portal treating our disconnect from
    // DBus as the client going away and tearing down the still-in-use
    // PipeWire stream. See session.rs for where this used to happen.
    let rt = std::sync::Arc::new(tokio::runtime::Runtime::new()?);
    session::run(cfg, render_node, input_tx, rt)
}

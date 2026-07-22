mod capture;
mod encode;
mod input;
mod modes;
mod pairing;
mod session;

use std::sync::mpsc;
use std::thread;

use anyhow::Result;

fn main() -> Result<()> {
    let cfg = palmtop_config::HostConfig::load()?;
    println!(
        "[palmtopd] vaapi={} codec={} qp={} fps={} port={}",
        cfg.gpu.vaapi_render_node, cfg.encode.codec, cfg.encode.qp, cfg.encode.fps, cfg.host.port
    );

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
    let rt = tokio::runtime::Runtime::new()?;
    session::run(cfg, input_tx, &rt)
}

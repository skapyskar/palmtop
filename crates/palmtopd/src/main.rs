mod capture;
mod diagnostics;
mod encode;
mod modes;
mod pairing;
mod platform;
mod session;

use platform::doctor;
use platform::input;

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

    if std::env::args().any(|a| a == "--list-encoders") {
        list_encoders(cfg.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?);
        return Ok(());
    }

    if let Some(codec) = flag_value("--set-encoder") {
        let path = palmtop_config::set_configured_codec(&codec)?;
        println!("Set encoder to {codec} in {}", path.display());
        println!("Restart the daemon for it to take effect:");
        println!("  systemctl --user restart palmtopd");
        return Ok(());
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
    let backend = cfg.resolved_encode_backend()?;
    println!("[gpu] encoding via {backend}");

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
    session::run(cfg, backend, input_tx, rt)
}

/// The value following `flag` in argv, if present -- both `--flag value` and
/// `--flag=value`, since a user who guesses one form and gets a silent no-op
/// has no way to tell that from the flag not existing.
fn flag_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    for (i, a) in args.iter().enumerate() {
        if let Some(rest) = a.strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_string());
        }
        if a == flag {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// Prints every encoder this machine can really use, and which one is
/// currently configured.
///
/// Exists because auto-detection answers "what works" but not "what is
/// best". It picks the first backend that succeeds, and on a hybrid-GPU
/// laptop that ordering is essentially arbitrary with respect to what
/// actually feels smoother -- iGPU VA-API and dGPU NVENC differ in latency,
/// power draw, thermal behaviour, and quality in ways no probe can rank.
/// Only the person watching the stream can.
fn list_encoders(cfg: &palmtop_config::HostConfig) {
    let configured = cfg.encode.codec.trim();
    println!("\nProbing every encoder on this machine (a few seconds)...\n");

    let probes = cfg.probe_backends();
    let working: Vec<&palmtop_config::BackendProbe> = probes.iter().filter(|p| p.works).collect();

    for p in &probes {
        let mark = if p.works { "  ok  " } else { " no   " };
        let current = if p.codec == configured { "  <- configured" } else { "" };
        println!("[{mark}] {:<12} {}{current}", p.codec, p.label);
    }

    let auto_marker = if configured == palmtop_config::AUTO_CODEC || configured.is_empty() {
        "  <- configured"
    } else {
        ""
    };
    println!(
        "[  --  ] {:<12} pick the first one that works, automatically{auto_marker}",
        palmtop_config::AUTO_CODEC
    );

    println!();
    match cfg.resolved_encode_backend() {
        Ok(backend) => println!("Right now the daemon would stream via: {backend}"),
        Err(e) => println!("Right now the daemon could not encode at all: {e:#}"),
    }

    if working.is_empty() {
        println!("\nNothing works here -- run `palmtopd --doctor` for the specific cause.");
        return;
    }

    println!("\nTo pin one (any of these, or `auto`):");
    for p in &working {
        println!("  palmtopd --set-encoder {}", p.codec);
    }
    println!(
        "\nWorth trying more than one if the stream feels sluggish: whichever the probe\n\
         happens to find first is not necessarily the one that feels best on your hardware."
    );
}

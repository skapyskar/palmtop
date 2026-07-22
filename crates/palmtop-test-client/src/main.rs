//! Minimal test client for `palmtopd`.
//!
//! Not the Android client -- this exists to validate the host's session logic
//! (Noise handshake, pairing, continuous live video streaming, input
//! round-trip) in isolation, on this machine, before the Android side can be
//! blamed for or credited with anything. Connects, receives real live video
//! for a few seconds, writes it to disk for `ffprobe`/`ffmpeg` to validate as
//! a real decodable stream, then exercises the input path (move, click, type).
//!
//! Reads the host's Noise public key straight out of `config/host.toml`,
//! which only works because this runs *on* the host machine -- a stand-in
//! for "the QR/mDNS-sourced pubkey the phone would otherwise TOFU-pin" (see
//! palmtopd/src/pairing.rs for what that trust model does and doesn't cover).

use std::io::{BufWriter, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use palmtop_proto::{noise, Button, Message, Modifiers, NoiseTransport, PROTOCOL_VERSION};

fn main() -> Result<()> {
    let cfg = palmtop_config::HostConfig::load()?;
    let addr = format!("{}:{}", cfg.resolved_ip()?, cfg.host.port);
    println!("[test-client] connecting to {addr}");
    let mut stream = TcpStream::connect(&addr).with_context(|| format!("connect {addr}"))?;
    stream.set_nodelay(true).ok();

    let host_pubkey = noise::from_hex(&cfg.pairing.noise_public_key)
        .context("decode noise_public_key from config/host.toml")?;
    let mut transport = NoiseTransport::handshake_initiator(&mut stream, &host_pubkey)
        .context("noise handshake (initiator)")?;
    println!("[test-client] noise handshake ok");

    send(&mut transport, &mut stream, &Message::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: cfg.pairing.token.clone(),
    })?;
    match recv(&mut transport, &mut stream)?.context("connection closed during handshake")? {
        Message::HelloAck { ok: true, .. } => println!("[test-client] handshake ok"),
        Message::HelloAck { ok: false, reason } => bail!("host rejected handshake: {reason}"),
        other => bail!("expected HelloAck, got {other:?}"),
    }

    let (width, height, fps) = match recv(&mut transport, &mut stream)?
        .context("connection closed before VideoConfig")?
    {
        Message::VideoConfig { codec, width, height, fps, mode, drop_budget_ms } => {
            println!(
                "[test-client] video config: {codec} {width}x{height} @{fps}fps \
                 (mode {mode}, drop budget {drop_budget_ms}ms)"
            );
            (width, height, fps)
        }
        other => bail!("expected VideoConfig, got {other:?}"),
    };

    // Exercise a mode switch before collecting frames. This is the riskiest
    // path on the host -- it tears down and rebuilds the whole encode stage
    // mid-session -- and doing it here means a regression shows up in a
    // 20-second terminal run instead of only when someone has a phone in hand.
    // Sync mode is the useful one to assert on because its 720p differs from
    // the 1080p default, so a resolution that fails to change is a visible
    // failure rather than a silent no-op.
    if std::env::args().any(|a| a == "--test-mode-switch") {
        println!("[test-client] requesting sync mode (expect 1280x720)");
        send(&mut transport, &mut stream, &Message::SetMode { mode: 0 })?;

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut switched = false;
        while Instant::now() < deadline && !switched {
            stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
            match recv(&mut transport, &mut stream) {
                Ok(Some(Message::VideoConfig { width, height, fps, mode, drop_budget_ms, .. })) => {
                    println!(
                        "[test-client] mode switch -> {width}x{height} @{fps}fps \
                         (mode {mode}, drop budget {drop_budget_ms}ms)"
                    );
                    if (width, height) != (1280, 720) {
                        bail!("expected 1280x720 after switching to sync mode, got {width}x{height}");
                    }
                    if mode != 0 {
                        bail!("host reported mode {mode} after a sync-mode request");
                    }
                    switched = true;
                }
                // Frames from the old encoder may still be in flight; they are
                // expected and harmless before the new config arrives.
                Ok(Some(_)) => {}
                Ok(None) => bail!("host closed the connection during the mode switch"),
                Err(e) if e.to_string().contains("timed out") => {}
                Err(e) => return Err(e),
            }
        }
        if !switched {
            bail!("host never sent a new VideoConfig after SetMode");
        }
        println!("[test-client] mode switch ok");
    }

    // Collect a few seconds of real live frames.
    let out_path = std::env::temp_dir().join("palmtop-test-client-capture.h264");
    let mut out = BufWriter::new(std::fs::File::create(&out_path)?);
    let mut frames = 0u32;
    let mut bytes = 0u64;
    let mut first_keyframe = None;
    let deadline = Instant::now() + Duration::from_secs(4);

    // Read frames on a second connection thread isn't needed -- the writer
    // side of the session only sends video, so blocking reads here are fine.
    while Instant::now() < deadline {
        stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
        match recv(&mut transport, &mut stream) {
            Ok(Some(Message::VideoFrame { keyframe, data, .. })) => {
                if first_keyframe.is_none() {
                    first_keyframe = Some(keyframe);
                }
                bytes += data.len() as u64;
                out.write_all(&data)?;
                frames += 1;
            }
            Ok(Some(other)) => println!("[test-client] unexpected message: {other:?}"),
            Ok(None) => {
                println!("[test-client] host closed the connection");
                break;
            }
            Err(e) => {
                // Read timeout is expected/harmless here; anything else is
                // real. `{e:#}` (full anyhow cause chain) is needed to still
                // see "timed out" now that the read is wrapped in a couple
                // of `.context()` layers inside NoiseTransport::recv.
                if !format!("{e:#}").to_lowercase().contains("timed out") {
                    println!("[test-client] read error: {e:#}");
                }
            }
        }
    }
    out.flush()?;
    stream.set_read_timeout(None).ok();

    println!(
        "[test-client] received {frames} frames, {bytes} bytes, first_keyframe={:?}",
        first_keyframe
    );
    println!("[test-client] saved to {} for offline validation", out_path.display());

    if frames == 0 {
        bail!("received zero video frames -- capture/encode pipeline did not produce output");
    }
    if first_keyframe != Some(true) {
        println!("[test-client] WARNING: first frame was not a keyframe -- decoder would stall");
    }

    // Exercise the input path: move to center, click, type 'h'.
    println!("[test-client] sending input: move -> click -> key 'h'");
    send(&mut transport, &mut stream, &Message::PointerMotionAbsolute { x: 0.5, y: 0.5 })?;
    send(&mut transport, &mut stream, &Message::PointerButton { button: Button::Left, pressed: true })?;
    send(&mut transport, &mut stream, &Message::PointerButton { button: Button::Left, pressed: false })?;
    const KEY_H: u32 = 35; // evdev keycode, matches spike-wlr-input
    send(&mut transport, &mut stream, &Message::Key {
        evdev_code: KEY_H,
        pressed: true,
        modifiers: Modifiers::NONE,
    })?;
    send(&mut transport, &mut stream, &Message::Key {
        evdev_code: KEY_H,
        pressed: false,
        modifiers: Modifiers::NONE,
    })?;

    std::thread::sleep(Duration::from_millis(300));
    println!("[test-client] done. Expected resolution: {width}x{height} @{fps}fps");
    Ok(())
}

fn send(transport: &mut NoiseTransport, stream: &mut TcpStream, msg: &Message) -> Result<()> {
    let mut buf = Vec::new();
    msg.write_to(&mut buf)?;
    transport.send(stream, &buf)
}

fn recv(transport: &mut NoiseTransport, stream: &mut TcpStream) -> Result<Option<Message>> {
    match transport.recv(stream)? {
        Some(bytes) => Ok(Message::read_from(&mut &bytes[..])?),
        None => Ok(None),
    }
}

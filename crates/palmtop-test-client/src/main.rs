//! Minimal test client for `palmtopd`.
//!
//! Not the Android client -- this exists to validate the host's session logic
//! (handshake, continuous live video streaming, input round-trip) in
//! isolation, on this machine, before the Android side can be blamed for or
//! credited with anything. Connects, receives real live video for a few
//! seconds, writes it to disk for `ffprobe`/`ffmpeg` to validate as a real
//! decodable stream, then exercises the input path (move, click, type).

use std::io::{BufWriter, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use palmtop_proto::{Button, Message, Modifiers, PROTOCOL_VERSION};

fn main() -> Result<()> {
    let cfg = palmtop_config::HostConfig::load()?;
    let addr = format!("{}:{}", cfg.resolved_ip()?, cfg.host.port);
    println!("[test-client] connecting to {addr}");
    let mut stream = TcpStream::connect(&addr).with_context(|| format!("connect {addr}"))?;
    stream.set_nodelay(true).ok();

    Message::Hello { protocol_version: PROTOCOL_VERSION, token: cfg.pairing.token.clone() }
        .write_to(&mut stream)?;
    match Message::read_from(&mut stream)?.context("connection closed during handshake")? {
        Message::HelloAck { ok: true, .. } => println!("[test-client] handshake ok"),
        Message::HelloAck { ok: false, reason } => bail!("host rejected handshake: {reason}"),
        other => bail!("expected HelloAck, got {other:?}"),
    }

    let (width, height, fps) = match Message::read_from(&mut stream)?.context("connection closed before VideoConfig")? {
        Message::VideoConfig { codec, width, height, fps } => {
            println!("[test-client] video config: {codec} {width}x{height} @{fps}fps");
            (width, height, fps)
        }
        other => bail!("expected VideoConfig, got {other:?}"),
    };

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
        match Message::read_from(&mut stream) {
            Ok(Some(Message::VideoFrame { keyframe, data })) => {
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
                // Read timeout is expected/harmless here; anything else is real.
                if !e.to_string().to_lowercase().contains("timed out") {
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
    Message::PointerMotionAbsolute { x: 0.5, y: 0.5 }.write_to(&mut stream)?;
    Message::PointerButton { button: Button::Left, pressed: true }.write_to(&mut stream)?;
    Message::PointerButton { button: Button::Left, pressed: false }.write_to(&mut stream)?;
    const KEY_H: u32 = 35; // evdev keycode, matches spike-wlr-input
    Message::Key { evdev_code: KEY_H, pressed: true, modifiers: Modifiers::NONE }
        .write_to(&mut stream)?;
    Message::Key { evdev_code: KEY_H, pressed: false, modifiers: Modifiers::NONE }
        .write_to(&mut stream)?;

    std::thread::sleep(Duration::from_millis(300));
    println!("[test-client] done. Expected resolution: {width}x{height} @{fps}fps");
    Ok(())
}

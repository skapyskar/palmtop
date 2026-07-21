//! TCP session handling: accepts one client at a time (matches the plan's
//! MVP scope -- §9 "two phones connecting at once ... MVP: single"), does the
//! protocol handshake, then wires capture -> encode -> network together and
//! forwards incoming input events to the injector thread.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use palmtop_config::HostConfig;
use palmtop_proto::{Message, PROTOCOL_VERSION};

use crate::capture::{self, FrameSlot};
use crate::encode;

pub fn run(cfg: HostConfig, input_tx: Sender<Message>, rt: &tokio::runtime::Runtime) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", cfg.host.port))
        .with_context(|| format!("bind 0.0.0.0:{}", cfg.host.port))?;
    println!("[net] listening on 0.0.0.0:{}", cfg.host.port);

    loop {
        let (stream, addr) = listener.accept()?;
        println!("[net] client connected: {addr}");
        if let Err(e) = handle_client(stream, &cfg, &input_tx, rt) {
            eprintln!("[net] session ended: {e:#}");
        }
        println!("[net] ready for next client");
    }
}

fn handle_client(
    mut stream: TcpStream,
    cfg: &HostConfig,
    input_tx: &Sender<Message>,
    rt: &tokio::runtime::Runtime,
) -> Result<()> {
    stream.set_nodelay(true).ok();
    handshake(&mut stream, &cfg.pairing.token)?;

    // Deliberately does NOT drop the runtime after this call (it used to, and
    // that was a bug -- see main.rs). The portal's DBus connection lives
    // inside it and must stay up for as long as the PipeWire stream it
    // granted is in use, not just for the duration of the initial request.
    let (node_id, fd, (width, height)) = rt.block_on(capture::request_screencast())?;
    println!("[net] streaming {width}x{height} to client");

    let stop = Arc::new(AtomicBool::new(false));
    let slot = FrameSlot::new();

    let cap_handle = {
        let (slot, stop) = (slot.clone(), stop.clone());
        thread::spawn(move || {
            if let Err(e) = capture::run(fd, node_id, slot, stop) {
                eprintln!("[capture] {e:#}");
            }
        })
    };

    let mut child = encode::spawn(cfg, width, height)?;
    let ffmpeg_stdin = child.stdin.take().context("ffmpeg stdin")?;
    let ffmpeg_stdout = child.stdout.take().context("ffmpeg stdout")?;

    let feeder_handle = {
        let (slot, stop) = (slot.clone(), stop.clone());
        thread::spawn(move || encode::run_feeder(ffmpeg_stdin, slot, stop))
    };

    // Single-slot mailbox, not a channel -- see LatestEncoded's doc comment
    // for why an unbounded channel here was a real bug (~24s observed lag).
    let latest_encoded = encode::LatestEncoded::new();
    let reader_handle = {
        let latest_encoded = latest_encoded.clone();
        thread::spawn(move || encode::run_reader(ffmpeg_stdout, latest_encoded))
    };

    let (pong_tx, pong_rx) = mpsc::channel::<u64>();

    let write_half = stream.try_clone().context("clone tcp stream for writer")?;
    let codec = cfg.encode.codec.clone();
    let fps = cfg.encode.fps;
    let writer_handle = {
        let stop = stop.clone();
        thread::spawn(move || {
            run_writer(write_half, codec, width, height, fps, latest_encoded, pong_rx, stop)
        })
    };

    // Blocks on this thread until the client disconnects or errors.
    run_network_reader(stream, input_tx.clone(), pong_tx, &stop);

    stop.store(true, Ordering::Relaxed);
    let _ = cap_handle.join();
    let _ = feeder_handle.join(); // drops ffmpeg's stdin -> ffmpeg flushes + exits
    let _ = child.wait();
    let _ = reader_handle.join(); // stdout EOF once ffmpeg has exited
    let _ = writer_handle.join(); // exits once it observes `stop`
    Ok(())
}

fn handshake(stream: &mut TcpStream, expected_token: &str) -> Result<()> {
    match Message::read_from(stream)? {
        Some(Message::Hello { protocol_version, token }) => {
            if protocol_version != PROTOCOL_VERSION {
                let reason =
                    format!("protocol mismatch: host={PROTOCOL_VERSION} client={protocol_version}");
                Message::HelloAck { ok: false, reason: reason.clone() }.write_to(stream)?;
                bail!("{reason}");
            }
            // Constant-time-ish is not the point here (LAN-only, no encryption
            // yet -- see pairing.rs's doc comment on what this does and does
            // not protect against); a plain comparison is fine.
            if token != expected_token {
                let reason = "pairing rejected: wrong or missing token".to_string();
                Message::HelloAck { ok: false, reason: reason.clone() }.write_to(stream)?;
                bail!("{reason}");
            }
            Message::HelloAck { ok: true, reason: String::new() }.write_to(stream)?;
            Ok(())
        }
        other => bail!("expected Hello as the first message, got {other:?}"),
    }
}

fn run_network_reader(
    mut read_half: TcpStream,
    input_tx: Sender<Message>,
    pong_tx: Sender<u64>,
    stop: &AtomicBool,
) {
    loop {
        match Message::read_from(&mut read_half) {
            Ok(Some(Message::Ping { nonce })) => {
                let _ = pong_tx.send(nonce);
            }
            Ok(Some(
                msg @ (Message::PointerMotionRelative { .. }
                | Message::PointerMotionAbsolute { .. }
                | Message::PointerButton { .. }
                | Message::Scroll { .. }
                | Message::Key { .. }
                | Message::Text { .. }),
            )) => {
                let _ = input_tx.send(msg);
            }
            Ok(Some(_unexpected)) => {}
            Ok(None) => {
                println!("[net] client disconnected");
                break;
            }
            Err(e) => {
                eprintln!("[net] read error: {e:#}");
                break;
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
}

fn run_writer(
    mut write_half: TcpStream,
    codec: String,
    width: u32,
    height: u32,
    fps: u32,
    latest_encoded: Arc<encode::LatestEncoded>,
    pong_rx: mpsc::Receiver<u64>,
    stop: Arc<AtomicBool>,
) {
    let config_msg = Message::VideoConfig { codec, width, height, fps };
    if config_msg.write_to(&mut write_half).is_err() {
        return;
    }
    while !stop.load(Ordering::Relaxed) {
        // Pongs are rare and latency-insensitive relative to video -- drain
        // whatever's pending each time this thread wakes up, then get back
        // to the frame that actually matters for responsiveness.
        while let Ok(nonce) = pong_rx.try_recv() {
            let pong_msg = Message::Pong { nonce };
            if pong_msg.write_to(&mut write_half).is_err() {
                return;
            }
        }
        // Short timeout so a stale `stop` doesn't leave this thread blocked
        // indefinitely with no new frame arriving to wake it.
        if let Some(unit) = latest_encoded.take_latest_timeout(Duration::from_millis(50)) {
            let msg = Message::VideoFrame { keyframe: unit.keyframe, data: unit.data };
            if msg.write_to(&mut write_half).is_err() {
                return;
            }
        }
    }
}


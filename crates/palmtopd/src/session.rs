//! TCP session handling: accepts one client at a time (matches the plan's
//! MVP scope -- §9 "two phones connecting at once ... MVP: single"), performs
//! the Noise handshake and pairing-token check, then wires capture -> encode
//! -> network together and forwards incoming input events to the injector.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use palmtop_config::HostConfig;
use palmtop_proto::{noise, Message, NoiseTransport, PROTOCOL_VERSION};

use crate::capture::{self, FrameSlot};
use crate::encode;

/// Every message after the Noise handshake goes through this, guarded by a
/// Mutex because the reader and writer threads each hold an independent
/// clone of the underlying `TcpStream` but must share one `NoiseTransport`
/// (its send/receive nonces are two independent counters internally, so
/// contention here is brief lock-holding, not a real bottleneck).
type SharedNoise = Arc<Mutex<NoiseTransport>>;

/// Anything that needs sending on the socket other than video.
///
/// Everything routes through the writer thread rather than being written
/// directly, because two threads writing to one `TcpStream` would interleave
/// their bytes mid-message and corrupt the stream. The writer owning the write
/// side exclusively is what makes that impossible by construction.
enum Outgoing {
    /// Built in the writer, not at the call site, so `t_host_send_us` reflects
    /// the moment the reply actually goes out. Stamping it earlier would fold
    /// our own queuing delay into the client's network RTT estimate.
    Pong { nonce: u64, t_client_us: u64, t_host_recv_us: u64 },
    /// A `VideoConfig` announcing new stream dimensions. The writer discards
    /// any pending frame before sending it -- see [`encode::LatestEncoded::clear`].
    Reconfigure(Message),
}

/// Caps how much encoded video the kernel will hold on our behalf.
///
/// `LatestEncoded` carefully drops stale frames and then hands the survivor to
/// a kernel send buffer that may hold several more -- where nothing can drop
/// them, and where they are invisible unless you go looking. The "never queue"
/// invariant this pipeline enforces at every other stage simply stopped at the
/// syscall boundary.
///
/// A full buffer makes `write_all` block, which is the point: that is
/// backpressure, and `LatestEncoded` keeps discarding stale frames for as long
/// as the writer sits parked. Failure is non-fatal -- an unsized buffer is the
/// behaviour we already had, not a broken session.
///
/// # What this actually buys, measured
///
/// Not what was predicted. The hypothesis was lower latency; A/B runs with the
/// bound on and off show **no latency difference at all** -- p50 stayed within
/// run-to-run noise in every mode, on both static and high-motion content. On a
/// static desktop that is unsurprising in hindsight: the buffer never fills, so
/// there is no queue to bound.
///
/// The real benefit showed up in the *drop* rate under high-motion 1080p, where
/// the buffer does fill:
///
/// | mode     | drop% unbounded | drop% bounded |
/// |----------|-----------------|---------------|
/// | balanced | 39.8%           | **22.4%**     |
/// | quality  | 60.5%           | **34.2%**     |
///
/// Roughly half the wasted work removed, at identical latency. The mechanism is
/// the one the "never queue" invariant predicts: with the buffer bounded, the
/// writer blocks sooner, so `LatestEncoded` discards stale frames *before* they
/// are encrypted and transmitted. Unbounded, those same frames get pushed into
/// the kernel, sent over the air, decrypted by the phone -- and only then
/// dropped. Same latency either way; one path burns bandwidth and client CPU to
/// reach it.
///
/// Kept for that reason, with the corrected rationale, rather than for the
/// latency win it was expected to deliver and did not.
fn set_send_buffer(stream: &TcpStream, bytes: usize) {
    if let Err(e) = socket2::SockRef::from(stream).set_send_buffer_size(bytes) {
        eprintln!("[net] could not set SO_SNDBUF to {bytes}: {e} (continuing with the default)");
    }
}

/// Everything that must be rebuilt when the quality mode changes. Capture and
/// the network threads survive a switch untouched; only the encoder does not.
struct EncodeStage {
    child: std::process::Child,
    feeder: thread::JoinHandle<()>,
    reader: thread::JoinHandle<()>,
}

fn start_encode_stage(
    cfg: &HostConfig,
    preset: &crate::modes::Preset,
    src_width: u32,
    src_height: u32,
    slot: Arc<FrameSlot>,
    latest_encoded: Arc<encode::LatestEncoded>,
    stage_stop: Arc<AtomicBool>,
) -> Result<EncodeStage> {
    let mut child = encode::spawn(cfg, preset, src_width, src_height)?;
    let ffmpeg_stdin = child.stdin.take().context("ffmpeg stdin")?;
    let ffmpeg_stdout = child.stdout.take().context("ffmpeg stdout")?;

    // Carries each frame's capture timestamp across ffmpeg's pipes, which have
    // no metadata channel of their own -- see encode::TimestampFifo.
    let timestamps = encode::TimestampFifo::new();

    let feeder = {
        let timestamps = timestamps.clone();
        thread::spawn(move || encode::run_feeder(ffmpeg_stdin, slot, timestamps, stage_stop))
    };
    let reader = thread::spawn(move || encode::run_reader(ffmpeg_stdout, latest_encoded, timestamps));
    Ok(EncodeStage { child, feeder, reader })
}

/// Tears the encoder down and *waits* for it. Joining the reader matters: it
/// guarantees nothing can publish another frame afterwards, which is what lets
/// the writer safely clear the pending slot before announcing a new resolution.
fn stop_encode_stage(stage: EncodeStage, stage_stop: &AtomicBool) {
    stage_stop.store(true, Ordering::Relaxed);
    let _ = stage.feeder.join(); // drops ffmpeg's stdin -> it flushes and exits
    let mut child = stage.child;
    let _ = child.wait();
    let _ = stage.reader.join(); // stdout EOF once ffmpeg has gone
}

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

    let private_key = noise::from_hex(&cfg.pairing.noise_private_key)
        .context("decode noise_private_key from config/host.toml")?;
    let transport = NoiseTransport::handshake_responder(&mut stream, &private_key)
        .context("noise handshake (responder)")?;
    let noise: SharedNoise = Arc::new(Mutex::new(transport));

    let device_profile = handshake(&mut stream, &noise, &cfg.pairing.token)?;

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

    // Single-slot mailbox, not a channel -- see LatestEncoded's doc comment
    // for why an unbounded channel here was a real bug (~24s observed lag).
    // Survives mode changes; only the encoder feeding it is rebuilt.
    let latest_encoded = encode::LatestEncoded::new();

    let (out_tx, out_rx) = mpsc::channel::<Outgoing>();
    let (mode_tx, mode_rx) = mpsc::channel::<crate::modes::Mode>();

    let mut mode = crate::modes::Mode::default();
    let mut preset = mode.preset().clamped_to(&device_profile);
    set_send_buffer(&stream, preset.sndbuf_bytes);

    let mut stage_stop = Arc::new(AtomicBool::new(false));
    let mut stage = start_encode_stage(
        cfg, &preset, width, height, slot.clone(), latest_encoded.clone(), stage_stop.clone(),
    )?;

    let write_half = stream.try_clone().context("clone tcp stream for writer")?;
    let read_half = stream.try_clone().context("clone tcp stream for reader")?;
    let codec = cfg.encode.codec.clone();

    let writer_handle = {
        let (stop, noise, latest_encoded) = (stop.clone(), noise.clone(), latest_encoded.clone());
        thread::spawn(move || run_writer(write_half, noise, latest_encoded, out_rx, stop))
    };

    // The very first VideoConfig goes through the same path a mode change
    // does, so there is exactly one code path that announces stream
    // dimensions rather than two that could drift apart.
    let announce = |active_mode: crate::modes::Mode, preset: &crate::modes::Preset| {
        Outgoing::Reconfigure(Message::VideoConfig {
            codec: codec.clone(),
            width: preset.width,
            height: preset.height,
            fps: preset.fps,
            mode: active_mode.as_u8(),
            drop_budget_ms: preset.drop_budget_ms,
        })
    };
    let _ = out_tx.send(announce(mode, &preset));
    println!("[net] streaming in {} mode ({}x{}@{})", mode.name(), preset.width, preset.height, preset.fps);

    let reader_handle = {
        let (stop, noise, input_tx, out_tx) =
            (stop.clone(), noise.clone(), input_tx.clone(), out_tx.clone());
        thread::spawn(move || run_network_reader(read_half, noise, input_tx, out_tx, mode_tx, stop))
    };

    // Mode changes are rare; this loop spends almost all its time parked. The
    // timeout is only so a client disconnect is noticed promptly rather than
    // leaving this thread blocked on a channel nobody will ever send to.
    while !stop.load(Ordering::Relaxed) {
        match mode_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(new_mode) => {
                // Stop the old encoder *completely* before announcing anything:
                // joining its reader is what guarantees no old-resolution frame
                // can still be published after the new VideoConfig goes out.
                stop_encode_stage(stage, &stage_stop);

                mode = new_mode;
                preset = mode.preset().clamped_to(&device_profile);
                set_send_buffer(&stream, preset.sndbuf_bytes);
                stage_stop = Arc::new(AtomicBool::new(false));
                stage = start_encode_stage(
                    cfg, &preset, width, height, slot.clone(), latest_encoded.clone(),
                    stage_stop.clone(),
                )?;
                let _ = out_tx.send(announce(mode, &preset));
                println!(
                    "[net] switched to {} mode ({}x{}@{}, cap {}kb/s)",
                    mode.name(), preset.width, preset.height, preset.fps, preset.maxrate_kbps
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    stop.store(true, Ordering::Relaxed);
    stop_encode_stage(stage, &stage_stop);
    let _ = cap_handle.join();
    let _ = reader_handle.join();
    let _ = writer_handle.join(); // exits once it observes `stop`
    Ok(())
}

/// Serializes `msg` and sends it as one Noise transport payload (which may
/// span several wire-level chunks internally).
///
/// Deliberately does NOT call `NoiseTransport::send`/`recv`, which combine
/// crypto with blocking I/O in one call -- fine for a single-threaded caller,
/// but a real deadlock here: the reader and writer run on separate threads
/// sharing one `noise` lock, and a lock held for the duration of a blocking
/// network read (waiting on the *other* side to send something) starves
/// whichever thread needs the lock to send anything back. Found live: the
/// first version of this used `send`/`recv` directly and hung indefinitely
/// past its own timeout, because the reader thread's blocking read held the
/// lock the writer thread needed just to send `VideoConfig`. Fix: only ever
/// hold the lock for the brief, non-blocking `encrypt_chunk`/`decrypt_chunk`
/// calls; all I/O happens outside it.
fn send_encrypted(noise: &SharedNoise, stream: &mut TcpStream, msg: &Message) -> Result<()> {
    let mut buf = Vec::new();
    msg.write_to(&mut buf).context("serialize message")?;
    let frames = {
        let mut n = noise.lock().unwrap();
        noise::chunk_and_encrypt(&mut n, &buf).context("noise encrypt")?
    };
    for frame in frames {
        stream.write_all(&frame)?;
    }
    Ok(())
}

/// Blocks for one full decrypted message. `Ok(None)` means a clean
/// disconnect at a message boundary, matching `Message::read_from`'s contract.
/// See `send_encrypted`'s doc comment for why the lock is scoped so tightly.
fn recv_encrypted(noise: &SharedNoise, stream: &mut TcpStream) -> Result<Option<Message>> {
    let mut reassembler = noise::Reassembler::new();
    let bytes = loop {
        let Some(ciphertext) = noise::read_one_frame(stream).context("read noise frame")? else {
            return Ok(None);
        };
        let plaintext = {
            let mut n = noise.lock().unwrap();
            n.decrypt_chunk(&ciphertext).context("noise decrypt")?
        };
        if let Some(complete) = reassembler.push(&plaintext)? {
            break complete;
        }
    };
    Message::read_from(&mut &bytes[..])
        .context("parse decrypted message")?
        .context("decrypted payload contained no message")
        .map(Some)
}

/// Returns the connecting client's self-reported capabilities, which the
/// caller uses to size the stream -- see `palmtop_proto::DeviceProfile`.
fn handshake(
    stream: &mut TcpStream,
    noise: &SharedNoise,
    expected_token: &str,
) -> Result<palmtop_proto::DeviceProfile> {
    match recv_encrypted(noise, stream)? {
        Some(Message::Hello { protocol_version, token, profile }) => {
            if protocol_version != PROTOCOL_VERSION {
                let reason =
                    format!("protocol mismatch: host={PROTOCOL_VERSION} client={protocol_version}");
                send_encrypted(noise, stream, &Message::HelloAck { ok: false, reason: reason.clone() })?;
                bail!("{reason}");
            }
            // The channel is Noise-encrypted by this point, so this is now a
            // real secrecy-preserving comparison, not just an access-control
            // formality over plaintext -- see pairing.rs for the one gap that
            // remains (mDNS-sourced pubkey trust, pending a camera scanner).
            if token != expected_token {
                let reason = "pairing rejected: wrong or missing token".to_string();
                send_encrypted(noise, stream, &Message::HelloAck { ok: false, reason: reason.clone() })?;
                bail!("{reason}");
            }
            send_encrypted(noise, stream, &Message::HelloAck { ok: true, reason: String::new() })?;
            println!(
                "[net] client: {} ({}x{} @{}Hz, decodes up to {}x{}@{}, low-latency decoder: {})",
                profile.model, profile.screen_width, profile.screen_height, profile.refresh_hz,
                profile.max_decode_width, profile.max_decode_height, profile.max_decode_fps,
                profile.low_latency_decoder
            );
            Ok(profile)
        }
        other => bail!("expected Hello as the first message, got {other:?}"),
    }
}

fn run_network_reader(
    mut read_half: TcpStream,
    noise: SharedNoise,
    input_tx: Sender<Message>,
    out_tx: Sender<Outgoing>,
    mode_tx: Sender<crate::modes::Mode>,
    stop: Arc<AtomicBool>,
) {
    let noise = &noise;
    loop {
        match recv_encrypted(noise, &mut read_half) {
            Ok(Some(Message::Ping { nonce, t_client_us })) => {
                // Stamped here, the instant it is parsed, so the client's RTT
                // excludes however long we then take to schedule the reply.
                let t_host_recv_us = capture::monotonic_us();
                let _ = out_tx.send(Outgoing::Pong { nonce, t_client_us, t_host_recv_us });
            }
            Ok(Some(Message::SetMode { mode })) => match crate::modes::Mode::from_u8(mode) {
                Some(m) => {
                    let _ = mode_tx.send(m);
                }
                // Not silently defaulting: a client from a different protocol
                // version should be visibly wrong here rather than quietly
                // served a mode it never asked for.
                None => eprintln!("[net] client requested unknown mode {mode} -- ignoring"),
            },
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

/// Owns the write side of the socket exclusively. Nothing else may write to
/// it: two threads interleaving bytes mid-message would corrupt the stream.
fn run_writer(
    mut write_half: TcpStream,
    noise: SharedNoise,
    latest_encoded: Arc<encode::LatestEncoded>,
    out_rx: mpsc::Receiver<Outgoing>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        // Control messages are rare and latency-insensitive relative to video
        // -- drain whatever's pending each time this thread wakes up, then get
        // back to the frame that actually matters for responsiveness.
        while let Ok(item) = out_rx.try_recv() {
            let msg = match item {
                Outgoing::Pong { nonce, t_client_us, t_host_recv_us } => Message::Pong {
                    nonce,
                    t_client_us,
                    t_host_recv_us,
                    // Taken here, immediately before serialising, so the
                    // interval the client subtracts genuinely covers the whole
                    // time we held the probe.
                    t_host_send_us: capture::monotonic_us(),
                },
                Outgoing::Reconfigure(msg) => {
                    // Drop any frame the previous encoder left pending. Its
                    // reader thread has already been joined by the caller, so
                    // nothing can publish after this point and the client's
                    // "everything after VideoConfig is the new size" assumption
                    // holds.
                    latest_encoded.clear();
                    msg
                }
            };
            if send_encrypted(&noise, &mut write_half, &msg).is_err() {
                return;
            }
        }
        // Short timeout so a stale `stop` doesn't leave this thread blocked
        // indefinitely with no new frame arriving to wake it.
        if let Some(unit) = latest_encoded.take_latest_timeout(Duration::from_millis(50)) {
            let msg = Message::VideoFrame {
                keyframe: unit.keyframe,
                capture_us: unit.capture_us,
                data: unit.data,
            };
            if send_encrypted(&noise, &mut write_half, &msg).is_err() {
                return;
            }
        }
    }
}

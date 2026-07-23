//! TCP session handling: performs the Noise handshake and pairing-token
//! check, then wires capture -> encode -> network together and forwards
//! incoming input events to the injector.
//!
//! One client streams at a time (the plan's §9 "two phones at once ... MVP:
//! single"), but connections are *accepted* concurrently and the newest
//! authenticated client takes over. Enforcing "single" by simply not accepting
//! was the earlier design and it was much worse than it sounds: a second phone
//! completed its TCP handshake against the kernel's listen backlog, then sat
//! unread forever -- no token check, no portal prompt, no error, nothing to
//! see from either end. See `run` and `ActiveSession`.

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

use crate::capture::{monotonic_us, FrameSlot};
use crate::encode;
use crate::platform;

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
    /// Progress or failure from a thread that does not own the socket, so it
    /// reaches the phone rather than only the host's log.
    Status { stage: String, ok: bool, detail: String },
}

/// Reports a stage to the client, best-effort.
///
/// Failing to send a status must never be what ends a session: this is
/// diagnostics, and diagnostics that can themselves take down the thing they
/// are diagnosing are worse than none. Errors go to the host log and are
/// otherwise swallowed.
fn send_status(noise: &SharedNoise, stream: &mut TcpStream, stage: &str, ok: bool, detail: &str) {
    if ok {
        println!("[{stage}] {detail}");
    } else {
        eprintln!("[{stage}] FAILED: {detail}");
    }
    let msg = Message::Status {
        stage: stage.to_string(),
        ok,
        detail: detail.to_string(),
    };
    if let Err(e) = send_encrypted(noise, stream, &msg) {
        eprintln!("[net] could not send status to the client: {e:#}");
    }
}

/// How long the stream may produce nothing before the client is told.
///
/// The "connected, approved, still black" report is exactly this state, and
/// it previously lasted forever in silence. Generous enough not to fire on a
/// slow first keyframe, short enough that nobody sits staring at a blank
/// screen wondering whether to keep waiting.
const FIRST_FRAME_GRACE: Duration = Duration::from_secs(8);

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

/// The parts of a session that stay fixed across mode changes. Grouped so
/// restarting the encoder reads as "same session, new preset" at the call
/// site, rather than as an eight-argument call whose ordering has to be
/// checked against the signature every time.
struct StageContext<'a> {
    cfg: &'a HostConfig,
    backend: &'a palmtop_config::EncodeBackend,
    src_width: u32,
    src_height: u32,
    slot: Arc<FrameSlot>,
    latest_encoded: Arc<encode::LatestEncoded>,
}

fn start_encode_stage(
    ctx: &StageContext<'_>,
    preset: &crate::modes::Preset,
    stage_stop: Arc<AtomicBool>,
) -> Result<EncodeStage> {
    let (slot, latest_encoded) = (ctx.slot.clone(), ctx.latest_encoded.clone());
    let mut child =
        encode::spawn(ctx.cfg, ctx.backend, preset, ctx.src_width, ctx.src_height)?;
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

/// The session currently holding the screen, if any.
struct ActiveSession {
    stop: Arc<AtomicBool>,
    /// A clone of the client's socket, kept for one purpose: shutting it down
    /// during a takeover. Setting `stop` alone cannot free a thread parked in
    /// a blocking read -- only closing the socket under it can, and without
    /// that the outgoing session would linger until its peer happened to send
    /// something, which a phone that has gone to sleep never will.
    stream: TcpStream,
}

type SessionSlot = Arc<Mutex<Option<ActiveSession>>>;

/// Clears the session registry on the way out, but only if the entry is still
/// ours.
///
/// The identity check is the whole point. A session that has been taken over
/// unblocks and unwinds *after* its replacement has already registered, so an
/// unconditional clear would delete the new client's entry -- leaving the next
/// client with nothing to take over from and two sessions fighting over the
/// screen. Implemented as a guard so it holds on every exit path, including
/// the `?` returns between here and the end of the session.
struct ReleaseOnDrop {
    active: SessionSlot,
    stop: Arc<AtomicBool>,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let mut slot = self.active.lock().unwrap();
        if slot.as_ref().is_some_and(|s| Arc::ptr_eq(&s.stop, &self.stop)) {
            *slot = None;
        }
    }
}

/// Detects a peer that has silently vanished.
///
/// Nothing else does. Every read on this socket is blocking and untimed, so a
/// phone that disappears without closing the connection -- screen off, Wi-Fi
/// dropped, app swiped away, walked out of range -- leaves the host parked in
/// a read that will never return. With the single-session model below, that
/// wedged session held the screen indefinitely and locked *every* device out,
/// including the one that had been working a moment earlier. Roughly 30s to
/// notice, which is far below the threshold where a person starts wondering
/// whether the software is broken.
fn configure_socket(stream: &TcpStream) {
    stream.set_nodelay(true).ok();
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(15))
        .with_interval(Duration::from_secs(5));
    // Windows (and a few BSDs) don't let the retry count be set per-socket,
    // so socket2 doesn't expose the setter there at all. Not a behavioural
    // gap worth working around: the time and interval above are what
    // actually bound how long a vanished phone can wedge a session, and
    // Windows applies its own fixed retry count on top of them.
    #[cfg(not(any(
        target_os = "windows",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "solaris"
    )))]
    let keepalive = keepalive.with_retries(3);
    if let Err(e) = socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive) {
        eprintln!("[net] could not enable TCP keepalive: {e} (a dead client may linger)");
    }
}

pub fn run(
    cfg: HostConfig,
    backend: palmtop_config::EncodeBackend,
    input_tx: Sender<Message>,
    rt: Arc<tokio::runtime::Runtime>,
) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", cfg.host.port))
        .with_context(|| format!("bind 0.0.0.0:{}", cfg.host.port))?;
    println!("[net] listening on 0.0.0.0:{}", cfg.host.port);

    let cfg = Arc::new(cfg);
    let backend = Arc::new(backend);
    let active: SessionSlot = Arc::new(Mutex::new(None));

    // One thread per connection, rather than servicing a client inline.
    //
    // Inline was a real and badly-presenting bug: while a session ran, this
    // loop never called accept() again, so a second phone's connect() still
    // succeeded (the kernel completes the handshake into the listen backlog
    // by itself) while the host read nothing from it, checked no token, and
    // never requested the screen-share portal. From that phone the app looked
    // simply broken -- connected, then nothing, no error, no prompt, forever.
    // Accepting promptly is what makes the refusal-or-takeover below possible
    // at all: a connection nobody ever reads cannot be told anything.
    loop {
        let (stream, addr) = listener.accept()?;
        println!("[net] client connected: {addr}");
        let (cfg, backend, input_tx, rt, active) = (
            cfg.clone(),
            backend.clone(),
            input_tx.clone(),
            rt.clone(),
            active.clone(),
        );
        thread::spawn(move || {
            if let Err(e) = handle_client(stream, &cfg, &backend, &input_tx, &rt, &active) {
                eprintln!("[net] session with {addr} ended: {e:#}");
            } else {
                println!("[net] session with {addr} ended");
            }
        });
    }
}

fn handle_client(
    mut stream: TcpStream,
    cfg: &HostConfig,
    backend: &palmtop_config::EncodeBackend,
    input_tx: &Sender<Message>,
    rt: &tokio::runtime::Runtime,
    active: &SessionSlot,
) -> Result<()> {
    configure_socket(&stream);

    let private_key = noise::from_hex(&cfg.pairing.noise_private_key)
        .context("decode noise_private_key from config/host.toml")?;
    let transport = NoiseTransport::handshake_responder(&mut stream, &private_key)
        .context("noise handshake (responder)")?;
    let noise: SharedNoise = Arc::new(Mutex::new(transport));

    let device_profile = handshake(&mut stream, &noise, &cfg.pairing.token)?;

    let stop = Arc::new(AtomicBool::new(false));

    // Newest authenticated client wins.
    //
    // Ordering is the security-relevant part: this runs *after* the pairing
    // token has been checked, so an unpaired device on the network cannot
    // knock a legitimate session off the screen by merely opening a socket.
    //
    // Takeover rather than refusal because refusal fails in the case that
    // actually happens. The usual reason a session is still held is that the
    // previous one is already dead -- a phone that slept or lost Wi-Fi, whose
    // socket nothing has noticed yet -- and telling a real user "another
    // device is connected" when the other device is their own dormant phone
    // gives them nothing to do about it. Switching phones deliberately is the
    // same operation and works for free.
    {
        let mut slot = active.lock().unwrap();
        if let Some(previous) = slot.take() {
            println!("[net] a new client authenticated -- taking over the previous session");
            previous.stop.store(true, Ordering::Relaxed);
            // Unblocks the old session's threads immediately; see ActiveSession.
            let _ = previous.stream.shutdown(std::net::Shutdown::Both);
        }
        *slot = Some(ActiveSession {
            stop: stop.clone(),
            stream: stream.try_clone().context("clone tcp stream for the session registry")?,
        });
    }

    // Whatever happens below, this session must not leave a stale entry behind
    // that a later client would try to shut down.
    let _release = ReleaseOnDrop { active: active.clone(), stop: stop.clone() };

    // From here on, every failure is one the phone cannot otherwise see. The
    // laptop's operator has journalctl; the person holding the phone has a
    // blank screen and no way to tell "waiting for you to approve a dialog"
    // apart from "the GPU cannot encode". Each stage therefore reports itself
    // over the wire as well as to the log.
    send_status(
        &noise,
        &mut stream,
        "portal",
        true,
        "asking for screen-share permission -- approve the dialog on the laptop",
    );

    // Deliberately does NOT drop the runtime after this call (it used to, and
    // that was a bug -- see main.rs). The portal's DBus connection lives
    // inside it and must stay up for as long as the PipeWire stream it
    // granted is in use, not just for the duration of the initial request.
    let (screencast, (width, height)) = match rt.block_on(platform::capture::request_screencast()) {
        Ok(v) => v,
        Err(e) => {
            send_status(
                &noise,
                &mut stream,
                "portal",
                false,
                &format!(
                    "the laptop could not start screen sharing: {e:#}\n\
                     Run `palmtopd --doctor` on the laptop for the specific cause."
                ),
            );
            return Err(e).context("screen-share portal");
        }
    };
    send_status(
        &noise,
        &mut stream,
        "portal",
        true,
        &format!("screen sharing approved -- capturing {width}x{height}"),
    );
    println!("[net] streaming {width}x{height} to client");

    let slot = FrameSlot::new();

    let (out_tx, out_rx) = mpsc::channel::<Outgoing>();

    let cap_handle = {
        let (slot, stop, out_tx) = (slot.clone(), stop.clone(), out_tx.clone());
        thread::spawn(move || {
            if let Err(e) = platform::capture::run(screencast, slot, stop) {
                eprintln!("[capture] {e:#}");
                // Capture dying mid-session (the user revoked sharing, the
                // compositor restarted) is otherwise indistinguishable on the
                // phone from a frozen picture.
                let _ = out_tx.send(Outgoing::Status {
                    stage: "capture".to_string(),
                    ok: false,
                    detail: format!("screen capture stopped: {e:#}"),
                });
            }
        })
    };

    // Single-slot mailbox, not a channel -- see LatestEncoded's doc comment
    // for why an unbounded channel here was a real bug (~24s observed lag).
    // Survives mode changes; only the encoder feeding it is rebuilt.
    let latest_encoded = encode::LatestEncoded::new();

    let (mode_tx, mode_rx) = mpsc::channel::<crate::modes::Mode>();

    let mut mode = crate::modes::Mode::default();
    let mut preset = mode.preset().clamped_to(&device_profile);
    set_send_buffer(&stream, preset.sndbuf_bytes);

    let ctx = StageContext {
        cfg,
        backend,
        src_width: width,
        src_height: height,
        slot: slot.clone(),
        latest_encoded: latest_encoded.clone(),
    };

    let mut stage_stop = Arc::new(AtomicBool::new(false));
    let mut stage = match start_encode_stage(&ctx, &preset, stage_stop.clone()) {
        Ok(stage) => stage,
        Err(e) => {
            send_status(
                &noise,
                &mut stream,
                "encode",
                false,
                &format!(
                    "the laptop could not start its video encoder: {e:#}\n\
                     Run `palmtopd --doctor` on the laptop -- this is usually the GPU render \
                     node being wrong, or ffmpeg missing."
                ),
            );
            return Err(e).context("start encoder");
        }
    };

    let write_half = stream.try_clone().context("clone tcp stream for writer")?;
    let read_half = stream.try_clone().context("clone tcp stream for reader")?;
    let codec = cfg.encode.codec.clone();

    // Watched by the no-frames watchdog below.
    let sent_a_frame = Arc::new(AtomicBool::new(false));

    let writer_handle = {
        let (stop, noise, latest_encoded, sent_a_frame) =
            (stop.clone(), noise.clone(), latest_encoded.clone(), sent_a_frame.clone());
        thread::spawn(move || {
            run_writer(write_half, noise, latest_encoded, out_rx, stop, sent_a_frame)
        })
    };

    // "Connected, permission granted, screen still black" was a real report
    // with no signal attached to it whatsoever. If nothing has reached the
    // phone by now, say so on the phone -- the encoder can die at any point
    // after starting cleanly (a wrong GPU node fails exactly this way), and
    // silence is the one response that leaves nobody able to act.
    {
        let (out_tx, stop, sent_a_frame) = (out_tx.clone(), stop.clone(), sent_a_frame.clone());
        thread::spawn(move || {
            let deadline = std::time::Instant::now() + FIRST_FRAME_GRACE;
            while std::time::Instant::now() < deadline {
                if stop.load(Ordering::Relaxed) || sent_a_frame.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(200));
            }
            if sent_a_frame.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                return;
            }
            let _ = out_tx.send(Outgoing::Status {
                stage: "stream".to_string(),
                ok: false,
                detail: format!(
                    "no video has arrived after {}s, although the connection is fine. The \
                     laptop is capturing but producing no encoded frames -- run \
                     `palmtopd --doctor` on it; a GPU render node that cannot encode is the \
                     usual cause.",
                    FIRST_FRAME_GRACE.as_secs()
                ),
            });
        });
    }

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
    let _ = out_tx.send(Outgoing::Status {
        stage: "encode".to_string(),
        ok: true,
        detail: format!(
            "encoding {}x{}@{} on {} ({} mode)",
            preset.width, preset.height, preset.fps, backend, mode.name()
        ),
    });

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
                stage = start_encode_stage(&ctx, &preset, stage_stop.clone())?;
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
                let t_host_recv_us = monotonic_us();
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
    sent_a_frame: Arc<AtomicBool>,
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
                    t_host_send_us: monotonic_us(),
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
                Outgoing::Status { stage, ok, detail } => Message::Status { stage, ok, detail },
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
            // Set after the send succeeds, so the watchdog's question is
            // "did a frame really reach the phone", not "did we produce one".
            sent_a_frame.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected socket, for tests that only need something `shutdown`-able
    /// in the registry rather than a working session.
    fn dummy_stream() -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        TcpStream::connect(addr).unwrap()
    }

    fn register(active: &SessionSlot, stop: Arc<AtomicBool>) {
        *active.lock().unwrap() =
            Some(ActiveSession { stop, stream: dummy_stream() });
    }

    #[test]
    fn a_taken_over_session_does_not_clear_its_replacements_registration() {
        // The ordering that makes this subtle: a session that has been taken
        // over keeps running for a moment while it unwinds, and only *then*
        // drops its guard -- by which point the new session has already
        // registered. An unconditional clear here would silently deregister a
        // live session, so the next client would find nothing to take over
        // from and two sessions would end up fighting over the screen.
        let active: SessionSlot = Arc::new(Mutex::new(None));

        let old_stop = Arc::new(AtomicBool::new(false));
        register(&active, old_stop.clone());
        let old_guard = ReleaseOnDrop { active: active.clone(), stop: old_stop.clone() };

        // A newly authenticated client takes over.
        let new_stop = Arc::new(AtomicBool::new(false));
        {
            let mut slot = active.lock().unwrap();
            let previous = slot.take().expect("a session was registered");
            previous.stop.store(true, Ordering::Relaxed);
            let _ = previous.stream.shutdown(std::net::Shutdown::Both);
        }
        register(&active, new_stop.clone());

        assert!(old_stop.load(Ordering::Relaxed), "the old session should be told to stop");

        // Only now does the displaced session finish unwinding.
        drop(old_guard);

        let slot = active.lock().unwrap();
        let current = slot.as_ref().expect("the replacement must still be registered");
        assert!(
            Arc::ptr_eq(&current.stop, &new_stop),
            "the displaced session cleared the wrong registration"
        );
    }

    #[test]
    fn a_session_that_was_never_displaced_clears_itself_on_exit() {
        let active: SessionSlot = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        register(&active, stop.clone());

        {
            let _guard = ReleaseOnDrop { active: active.clone(), stop: stop.clone() };
        }

        assert!(
            active.lock().unwrap().is_none(),
            "a session must not leave a stale registration behind -- the next client \
             would try to shut down a socket nobody is using"
        );
    }
}

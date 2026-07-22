//! Continuous H.264 encode via a persistent `ffmpeg` subprocess, on whichever
//! of VA-API / NVENC / software libx264 `HostConfig::resolved_encode_backend`
//! found actually works on this machine -- see `spawn`.
//!
//! The Phase 0 encode spike (`spike-capture-encode`) proved VA-API throughput
//! (120fps on real frames) by batching N frames to a file and running ffmpeg
//! once. Live streaming needs ffmpeg kept running with frames piped to its
//! stdin as they're captured, and its Annex-B stdout parsed incrementally into
//! access units as they arrive -- there is no "whole file" to split anymore.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use pipewire::spa::param::video::VideoFormat;

use crate::capture::FrameSlot;

pub struct EncodedUnit {
    pub keyframe: bool,
    /// The capture timestamp of the frame this was encoded from, carried
    /// across ffmpeg by [`TimestampFifo`]. 0 means "unknown".
    pub capture_us: u64,
    pub data: Vec<u8>,
}

/// Upper bound on in-flight timestamps before the FIFO is assumed desynced.
/// `async_depth` is 1 and there are no B-frames, so more than a handful queued
/// means the one-frame-in/one-unit-out assumption has broken.
pub const FIFO_DESYNC_BOUND: usize = 8;

/// Reunites a capture timestamp with its encoded frame across ffmpeg's stdin
/// and stdout, which are ordinary pipes carrying no metadata of their own.
///
/// Correct only because the encoder runs `-bf 0` (no B-frames, so no
/// reordering) with input rate equal to output rate, which makes the
/// correspondence strictly one-in/one-out and in order. That assumption is
/// worth guarding rather than trusting: an ffmpeg build that dropped or
/// duplicated a single frame would desync this queue *permanently*, and every
/// latency figure thereafter would be wrong while looking entirely plausible.
/// Hence the bound below -- resetting loudly beats drifting silently.
pub struct TimestampFifo {
    inner: Mutex<VecDeque<u64>>,
}

impl TimestampFifo {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(VecDeque::new()) })
    }

    pub fn push(&self, us: u64) {
        let mut q = self.inner.lock().unwrap();
        q.push_back(us);
        if q.len() > FIFO_DESYNC_BOUND {
            eprintln!(
                "[encode] timestamp FIFO exceeded {FIFO_DESYNC_BOUND} entries -- ffmpeg is not \
                 emitting one access unit per input frame. Latency numbers would be wrong from \
                 here on; resetting the queue."
            );
            q.clear();
            q.push_back(us);
        }
    }

    /// Returns 0 when empty, meaning "unknown" rather than "captured at zero".
    pub fn pop(&self) -> u64 {
        self.inner.lock().unwrap().pop_front().unwrap_or(0)
    }

    /// Test-only: production code never needs the depth, but the desync guard
    /// is exactly the behaviour worth asserting on, and it is unobservable
    /// without this.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

/// Spawns the encoder for a given quality preset.
///
/// # Why VBR with a hard cap, and not constant-QP
///
/// This used to run constant-QP (`-qp 24`), which leaves frame size entirely
/// unbounded -- and frame size is transmit time. Measured on this GPU, 1080p30,
/// comparing worst-case access-unit size on high-complexity content:
///
/// | rate control | static desktop (median) | high motion (max frame) |
/// |--------------|------------------------|-------------------------|
/// | constant QP  | 1.3 KB                 | **1,300,638 B (1.3 MB)** |
/// | CBR 8Mb/s    | 33 KB                  | 90,600 B                 |
/// | VBR 4/8Mb/s  | 6.7 KB                 | 90,600 B                 |
///
/// A 1.3 MB frame needs roughly 200ms of airtime by itself on this link. That
/// is the mechanism behind the video-playback lag observed live: the stale-frame
/// drop handles the *consequence* of a frame arriving late, nothing was handling
/// the cause. Both VBR and CBR bound the worst case identically (the small VBV
/// buffer is what actually does it), 14x smaller than constant QP.
///
/// VBR rather than CBR because a desktop is mostly static, and CBR spends its
/// full target rate regardless -- 17x more data than necessary on unchanging
/// content, for no quality anyone can see, straight out of the battery and the
/// radio. VBR idles cheaply and only spends when the picture demands it.
///
/// `src_width`/`src_height` are the compositor's real output size; a preset may
/// ask for something smaller, in which case the encoder scales it down --
/// on the GPU for VA-API/NVENC, in software (an extra `-vf scale`) for the
/// software backend, which has no GPU to hand that work to.
///
/// The rate-control and GOP parameters (VBR target/cap/VBV window, `-bf 0`,
/// keyframe interval) are shared across all three backends -- the reasoning
/// above for *why* those specific numbers is backend-agnostic, only the
/// flag names differ (VA-API's `-rc_mode`, NVENC's `-rc`, libx264's implicit
/// ABR-from-bitrate). What's backend-specific is confined to
/// `hwaccel_args`/`filter_and_scale`/`rate_control_args` below, so the
/// shared reasoning stays in one place rather than being copied three times
/// with room for the copies to drift.
pub fn spawn(
    cfg: &palmtop_config::HostConfig,
    backend: &palmtop_config::EncodeBackend,
    preset: &crate::modes::Preset,
    src_width: u32,
    src_height: u32,
) -> Result<Child> {
    let scaling = preset.width != src_width || preset.height != src_height;
    // The steady-state target sits at half the ceiling, leaving headroom for
    // bursts without making that burst rate the normal spend.
    let target_kbps = (preset.maxrate_kbps / 2).max(1);
    // 100ms of airtime. This small VBV window is what actually bounds per-frame
    // size -- a large one would let the encoder blow a whole second's budget on
    // a single frame and reintroduce exactly the problem this replaced.
    let bufsize_kbit = (preset.maxrate_kbps / 10).max(1);

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"]);

    for arg in hwaccel_args(backend) {
        cmd.arg(arg);
    }

    cmd.args(["-f", "rawvideo", "-pix_fmt", "bgra", "-s"])
        .arg(format!("{src_width}x{src_height}"))
        .args(["-r", &preset.fps.to_string(), "-i", "pipe:0"])
        .arg("-vf")
        .arg(filter_and_scale(backend, scaling, preset.width, preset.height))
        .args(["-c:v", backend.codec_name()]);

    for arg in rate_control_args(backend, target_kbps, preset.maxrate_kbps, bufsize_kbit) {
        cmd.arg(arg);
    }

    cmd.args(["-bf", "0"])
        // Keyframe interval. A dropped P-frame corrupts decode until the next
        // keyframe resyncs, so a tight GOP bounds that glitch window; the
        // slower presets relax it because it costs real bitrate.
        .args(["-g", &preset.gop.to_string()]);

    if matches!(backend, palmtop_config::EncodeBackend::Vaapi { .. }) {
        // Frame-pipelining depth -- see EncodeSection::async_depth doc
        // comment. VA-API-specific: NVENC and libx264 have no equivalent
        // flag and default to behaviour already low-latency enough for this
        // use case without it.
        cmd.args(["-async_depth", &cfg.encode.async_depth.to_string()]);
    }

    cmd.args(["-f", "h264", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawn ffmpeg for {backend} (is ffmpeg on PATH?)"))
}

/// Arguments that must appear *before* `-i`: hardware device init for
/// VA-API, nothing for NVENC/software (ffmpeg's nvenc/libx264 encoders take
/// plain frames directly -- no device handle to set up first).
fn hwaccel_args(backend: &palmtop_config::EncodeBackend) -> Vec<String> {
    match backend {
        palmtop_config::EncodeBackend::Vaapi { render_node } => {
            vec!["-init_hw_device".to_string(), format!("vaapi=va:{render_node}")]
        }
        palmtop_config::EncodeBackend::Nvenc | palmtop_config::EncodeBackend::Software => vec![],
    }
}

/// The `-vf` value: pixel format conversion plus scaling, if the preset
/// asked for a size other than the capture's native one.
///
/// VA-API scales on the GPU as part of the same `hwupload` filter chain
/// (`scale_vaapi`), which is why it needs `format=nv12,hwupload` first --
/// the frame has to be GPU-resident before a GPU filter can touch it. NVENC
/// and software have no such requirement (plain filters, plain scale) but
/// pay for that simplicity in an extra CPU-side scale pass -- an acceptable
/// cost given both are already fallback paths, not the common case.
fn filter_and_scale(
    backend: &palmtop_config::EncodeBackend,
    scaling: bool,
    width: u32,
    height: u32,
) -> String {
    match backend {
        palmtop_config::EncodeBackend::Vaapi { .. } => {
            let scale = if scaling { format!(",scale_vaapi={width}:{height}") } else { String::new() };
            format!("format=nv12,hwupload{scale}")
        }
        palmtop_config::EncodeBackend::Nvenc => {
            let scale = if scaling { format!(",scale={width}:{height}") } else { String::new() };
            format!("format=nv12{scale}")
        }
        palmtop_config::EncodeBackend::Software => {
            // libx264's native/fastest path is yuv420p, not nv12.
            let scale = if scaling { format!(",scale={width}:{height}") } else { String::new() };
            format!("format=yuv420p{scale}")
        }
    }
}

/// Bitrate/VBV flags. Same target numbers everywhere (see this function's
/// caller's doc comment for why); only the flag *names* VA-API/NVENC/libx264
/// use to mean "VBR with this ceiling and this window" differ.
fn rate_control_args(
    backend: &palmtop_config::EncodeBackend,
    target_kbps: u32,
    maxrate_kbps: u32,
    bufsize_kbit: u32,
) -> Vec<String> {
    let mut args = match backend {
        palmtop_config::EncodeBackend::Vaapi { .. } => {
            vec!["-rc_mode".to_string(), "VBR".to_string()]
        }
        palmtop_config::EncodeBackend::Nvenc => {
            vec!["-rc".to_string(), "vbr".to_string()]
        }
        // libx264 has no separate rc-mode flag: -b:v alone selects ABR, and
        // -maxrate/-bufsize below constrain it exactly like the other two.
        palmtop_config::EncodeBackend::Software => {
            vec!["-preset".to_string(), "ultrafast".to_string(), "-tune".to_string(), "zerolatency".to_string()]
        }
    };
    args.extend([
        "-b:v".to_string(), format!("{target_kbps}k"),
        "-maxrate".to_string(), format!("{maxrate_kbps}k"),
        "-bufsize".to_string(), format!("{bufsize_kbit}k"),
    ]);
    args
}

/// Pulls frames off the [`FrameSlot`] and writes them to ffmpeg's stdin.
/// Exits (and closes stdin, which lets ffmpeg flush and exit) once `stop` is set.
///
/// `ffmpeg` was spawned with a hardcoded `-pix_fmt bgra` (see [`spawn`]), which
/// is an assumption about what the compositor negotiates, not a guarantee.
/// Verified against the first frame's actual format rather than trusted
/// silently -- a mismatch here means feeding ffmpeg data it will misinterpret
/// as valid pixels, which fails confusingly (garbled or rejected output)
/// rather than with a clear error, if left unchecked.
pub fn run_feeder(
    mut stdin: ChildStdin,
    slot: Arc<FrameSlot>,
    timestamps: Arc<TimestampFifo>,
    stop: Arc<AtomicBool>,
) {
    let mut checked_format = false;
    while let Some(frame) = slot.take_latest_blocking(&stop) {
        if !checked_format {
            checked_format = true;
            if frame.format != VideoFormat::BGRA {
                eprintln!(
                    "[encode] FATAL: compositor negotiated {:?}, but ffmpeg was started \
                     expecting BGRA ({}x{}) -- refusing to feed mismatched data",
                    frame.format, frame.width, frame.height
                );
                return;
            }
            println!("[encode] feeding {}x{} {:?} frames", frame.width, frame.height, frame.format);
        }
        // Pushed immediately before the write so the queue order matches
        // the order ffmpeg receives frames in.
        timestamps.push(frame.capture_us);
        if stdin.write_all(&frame.bytes).is_err() {
            break; // ffmpeg went away
        }
    }
    // Dropping `stdin` here closes the pipe -- ffmpeg sees EOF and exits cleanly.
}

/// Single-slot mailbox for encoded output, mirroring [`FrameSlot`]'s "keep only
/// the latest" design -- applied here for exactly the same reason.
///
/// The first version of this pipeline used an unbounded channel between the
/// encoder and the network writer. That is the bug that caused a real,
/// observed ~24s input-to-screen lag: `FrameSlot` correctly drops stale
/// *captured* frames when the encoder falls behind, but once a frame reached
/// this (former) channel, nothing ever dropped it -- if the network or client
/// decoder was ever even briefly slower than the encoder, encoded frames
/// piled up without bound and the writer dutifully sent all of them, in
/// order, arbitrarily late. "Never queue" has to hold at every stage, not
/// just the one that was measured first.
pub struct LatestEncoded {
    inner: Mutex<Option<EncodedUnit>>,
    cvar: Condvar,
}

impl LatestEncoded {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(None), cvar: Condvar::new() })
    }

    fn publish(&self, unit: EncodedUnit) {
        let mut guard = self.inner.lock().unwrap();
        *guard = Some(unit);
        self.cvar.notify_one();
    }

    /// Discards any frame still waiting to be sent.
    ///
    /// Used on a mode change: a frame encoded at the *old* resolution must
    /// never be sent after the `VideoConfig` announcing the new one, or the
    /// client decodes it with a decoder configured for different dimensions.
    /// Safe only because the caller has already joined the old encoder's
    /// reader thread, so nothing can publish into the slot afterwards.
    pub fn clear(&self) {
        *self.inner.lock().unwrap() = None;
    }

    /// Waits up to `timeout` for a unit. Returns `None` on timeout so the
    /// caller can do other periodic work (e.g. check a stop flag or drain a
    /// side channel) rather than blocking indefinitely.
    pub fn take_latest_timeout(&self, timeout: Duration) -> Option<EncodedUnit> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(u) = guard.take() {
            return Some(u);
        }
        let (mut guard, _timeout_result) = self.cvar.wait_timeout(guard, timeout).unwrap();
        guard.take()
    }
}

/// Reads ffmpeg's Annex-B stdout, splits it into access units as they become
/// complete, and publishes each to `latest` (overwriting any not-yet-sent
/// unit -- see [`LatestEncoded`]). Exits on stdout EOF (ffmpeg exited).
pub fn run_reader(
    mut stdout: impl Read,
    latest: Arc<LatestEncoded>,
    timestamps: Arc<TimestampFifo>,
) {
    let mut splitter = AnnexBSplitter::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = match stdout.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(_) => break,
        };
        for (keyframe, data) in splitter.push(&chunk[..n]) {
            latest.publish(EncodedUnit { keyframe, capture_us: timestamps.pop(), data });
        }
    }
}

/// Incrementally groups an Annex-B byte stream into access units.
///
/// An access unit is complete only once the *next* one's start code is seen
/// (there's no other way to know where the trailing bytes of a NAL end), so
/// this buffers a tail of not-yet-confirmed-complete data across calls to
/// `push`. Mirrors the batch splitter in `spike-h264-server`, restructured to
/// work incrementally on a stream instead of a whole file.
struct AnnexBSplitter {
    buf: Vec<u8>,
}

impl AnnexBSplitter {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<(bool, Vec<u8>)> {
        self.buf.extend_from_slice(chunk);
        let starts = find_start_codes(&self.buf);

        let mut out = Vec::new();
        let mut au_start_idx = 0;
        let mut consumed_up_to = 0usize;
        let mut i = 0;
        while i < starts.len() {
            let (pos, sc_len) = starts[i];
            let nal_type = self.buf.get(pos + sc_len).map(|b| b & 0x1f).unwrap_or(0);
            let is_vcl = nal_type == 1 || nal_type == 5; // non-IDR / IDR slice

            if is_vcl {
                let is_last_known_start = i + 1 == starts.len();
                if is_last_known_start {
                    // Can't know where this NAL ends until more data arrives.
                    break;
                }
                let au_begin = starts[au_start_idx].0;
                let au_end = starts[i + 1].0;
                out.push((nal_type == 5, self.buf[au_begin..au_end].to_vec()));
                consumed_up_to = au_end;
                au_start_idx = i + 1;
            }
            i += 1;
        }
        if consumed_up_to > 0 {
            self.buf.drain(0..consumed_up_to);
        }
        out
    }
}

/// Returns (offset_of_start_code, start_code_length) for each NAL in `data`.
fn find_start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                out.push((i, 3));
                i += 3;
                continue;
            } else if i + 4 <= data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                out.push((i, 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(start_code: &[u8], nal_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = start_code.to_vec();
        v.push(nal_type); // first byte's low 5 bits are the NAL type; high bits 0 here
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn single_vcl_nal_waits_for_next_start_code() {
        let mut s = AnnexBSplitter::new();
        let au = nal(&[0, 0, 0, 1], 5, &[0xAA, 0xBB]);
        // No following start code yet -- must not emit.
        assert!(s.push(&au).is_empty());
    }

    #[test]
    fn emits_once_the_next_nal_begins() {
        let mut s = AnnexBSplitter::new();
        let mut stream = nal(&[0, 0, 0, 1], 5, &[0xAA, 0xBB]); // IDR
        stream.extend(nal(&[0, 0, 0, 1], 1, &[0xCC])); // next AU begins
        let out = s.push(&stream);
        assert_eq!(out.len(), 1);
        assert!(out[0].0, "first NAL was type 5 (IDR) -> keyframe");
        assert_eq!(out[0].1, nal(&[0, 0, 0, 1], 5, &[0xAA, 0xBB]));
    }

    #[test]
    fn groups_parameter_sets_with_the_following_slice() {
        let mut s = AnnexBSplitter::new();
        let mut stream = nal(&[0, 0, 0, 1], 7, &[0x01]); // SPS
        stream.extend(nal(&[0, 0, 0, 1], 8, &[0x02])); // PPS
        stream.extend(nal(&[0, 0, 0, 1], 5, &[0x03])); // IDR slice -- ends the AU
        stream.extend(nal(&[0, 0, 0, 1], 1, &[0x04])); // next AU starts
        let out = s.push(&stream);
        assert_eq!(out.len(), 1);
        assert!(out[0].0);
        assert_eq!(out[0].1.len(), stream.len() - nal(&[0, 0, 0, 1], 1, &[0x04]).len());
    }

    #[test]
    fn timestamp_fifo_is_first_in_first_out() {
        let fifo = TimestampFifo::new();
        fifo.push(10);
        fifo.push(20);
        assert_eq!(fifo.pop(), 10);
        assert_eq!(fifo.pop(), 20);
    }

    #[test]
    fn timestamp_fifo_returns_zero_when_empty() {
        let fifo = TimestampFifo::new();
        // 0 means "unknown", which the client treats as un-measurable rather
        // than as a timestamp -- see MainActivity's frame-age check.
        assert_eq!(fifo.pop(), 0);
    }

    /// If ffmpeg ever stops emitting exactly one access unit per input frame,
    /// this queue desyncs and every timestamp after that point is wrong, with
    /// nothing logged anywhere. Confidently wrong latency numbers are worse
    /// than missing ones, so it self-resets instead of drifting forever.
    #[test]
    fn timestamp_fifo_resets_when_it_grows_past_the_bound() {
        let fifo = TimestampFifo::new();

        // Filling exactly to the bound is still healthy -- nothing is dropped.
        for i in 0..FIFO_DESYNC_BOUND {
            fifo.push(i as u64);
        }
        assert_eq!(fifo.len(), FIFO_DESYNC_BOUND);

        // One more crosses the line. The queue resets rather than growing,
        // keeping only the newest entry, so the next pop pairs with a recent
        // frame instead of one arbitrarily far in the past.
        fifo.push(999);
        assert_eq!(fifo.len(), 1);
        assert_eq!(fifo.pop(), 999);
    }

    #[test]
    fn works_across_multiple_push_calls_split_mid_nal() {
        let mut s = AnnexBSplitter::new();
        let mut stream = nal(&[0, 0, 0, 1], 5, &[0xAA, 0xBB, 0xCC]);
        stream.extend(nal(&[0, 0, 0, 1], 1, &[0xDD]));
        let (a, b) = stream.split_at(5); // split partway through the first NAL
        assert!(s.push(a).is_empty());
        let out = s.push(b);
        assert_eq!(out.len(), 1);
        assert!(out[0].0);
    }

    // --- multi-backend ffmpeg argument construction -----------------------
    //
    // These pin the exact flags each backend needs without requiring the
    // hardware to exist -- real end-to-end verification (does ffmpeg accept
    // these flags and actually produce H.264 on real VA-API/NVENC/software)
    // was done by hand against real hardware and a controlled fake ffmpeg
    // that fails only the backends under test; that isn't something CI can
    // repeat, so what's checked here is the one thing that can be pinned in
    // a plain unit test: that the three backends produce genuinely different,
    // individually-correct argument sets rather than one path accidentally
    // reusing another's flags.

    fn vaapi() -> palmtop_config::EncodeBackend {
        palmtop_config::EncodeBackend::Vaapi { render_node: "/dev/dri/renderD128".to_string() }
    }

    #[test]
    fn only_vaapi_gets_a_hwaccel_device() {
        assert_eq!(hwaccel_args(&vaapi()), vec!["-init_hw_device", "vaapi=va:/dev/dri/renderD128"]);
        assert!(hwaccel_args(&palmtop_config::EncodeBackend::Nvenc).is_empty());
        assert!(hwaccel_args(&palmtop_config::EncodeBackend::Software).is_empty());
    }

    #[test]
    fn each_backend_uses_its_own_pixel_format_and_scale_filter() {
        assert_eq!(filter_and_scale(&vaapi(), false, 1280, 720), "format=nv12,hwupload");
        assert_eq!(
            filter_and_scale(&vaapi(), true, 1280, 720),
            "format=nv12,hwupload,scale_vaapi=1280:720"
        );
        assert_eq!(
            filter_and_scale(&palmtop_config::EncodeBackend::Nvenc, true, 1280, 720),
            "format=nv12,scale=1280:720"
        );
        // libx264's native pixel format is yuv420p, not nv12 -- getting this
        // one wrong doesn't fail loudly, it just costs an extra conversion
        // ffmpeg performs silently, so it is worth pinning explicitly.
        assert_eq!(
            filter_and_scale(&palmtop_config::EncodeBackend::Software, true, 1280, 720),
            "format=yuv420p,scale=1280:720"
        );
    }

    #[test]
    fn no_scale_filter_is_added_when_the_preset_matches_the_source_size() {
        assert_eq!(filter_and_scale(&palmtop_config::EncodeBackend::Nvenc, false, 1920, 1080), "format=nv12");
        assert_eq!(
            filter_and_scale(&palmtop_config::EncodeBackend::Software, false, 1920, 1080),
            "format=yuv420p"
        );
    }

    #[test]
    fn each_backend_spells_vbr_its_own_way_but_shares_the_same_bitrate_numbers() {
        let vaapi_args = rate_control_args(&vaapi(), 2000, 4000, 400);
        let nvenc_args = rate_control_args(&palmtop_config::EncodeBackend::Nvenc, 2000, 4000, 400);
        let sw_args = rate_control_args(&palmtop_config::EncodeBackend::Software, 2000, 4000, 400);

        assert_eq!(vaapi_args[0], "-rc_mode");
        assert_eq!(nvenc_args[0], "-rc");
        assert_eq!(sw_args[0], "-preset"); // libx264 has no rc-mode flag at all

        // Whatever the flags leading up to it, the actual numbers must agree
        // across backends -- the whole point of sharing preset/maxrate math
        // in one place rather than three.
        for args in [&vaapi_args, &nvenc_args, &sw_args] {
            assert!(args.contains(&"-b:v".to_string()) && args.contains(&"2000k".to_string()));
            assert!(args.contains(&"-maxrate".to_string()) && args.contains(&"4000k".to_string()));
            assert!(args.contains(&"-bufsize".to_string()) && args.contains(&"400k".to_string()));
        }
    }
}

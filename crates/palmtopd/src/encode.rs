//! Continuous VA-API H.264 encode via a persistent `ffmpeg` subprocess.
//!
//! The Phase 0 encode spike (`spike-capture-encode`) proved VA-API throughput
//! (120fps on real frames) by batching N frames to a file and running ffmpeg
//! once. Live streaming needs ffmpeg kept running with frames piped to its
//! stdin as they're captured, and its Annex-B stdout parsed incrementally into
//! access units as they arrive -- there is no "whole file" to split anymore.

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
    pub data: Vec<u8>,
}

pub fn spawn(
    cfg: &palmtop_config::HostConfig,
    width: u32,
    height: u32,
) -> Result<Child> {
    Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-init_hw_device"])
        .arg(format!("vaapi=va:{}", cfg.gpu.vaapi_render_node))
        .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-s"])
        .arg(format!("{width}x{height}"))
        .args(["-r", &cfg.encode.fps.to_string(), "-i", "pipe:0"])
        .args(["-vf", "format=nv12,hwupload", "-c:v", &cfg.encode.codec])
        .args(["-qp", &cfg.encode.qp.to_string(), "-bf", "0", "-g"])
        // A keyframe every ~0.5s, not every 1s: LatestEncoded (below) can now
        // drop a P-frame when the network/client falls behind, and a dropped
        // P-frame corrupts decode until the next keyframe resyncs. Tighter
        // GOP bounds that glitch window. Cheap to spend -- Phase 0 measured
        // ~65x bitrate headroom on this link.
        .arg((cfg.encode.fps / 2).max(1).to_string())
        // Frame-pipelining depth -- see EncodeSection::async_depth doc comment.
        // Low by design: this is an interactive control loop, not a video export.
        .args(["-async_depth", &cfg.encode.async_depth.to_string()])
        .args(["-f", "h264", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn ffmpeg (is it on PATH? is the vaapi render node correct?)")
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
pub fn run_feeder(mut stdin: ChildStdin, slot: Arc<FrameSlot>, stop: Arc<AtomicBool>) {
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
pub fn run_reader(mut stdout: impl Read, latest: Arc<LatestEncoded>) {
    let mut splitter = AnnexBSplitter::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = match stdout.read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(_) => break,
        };
        for (keyframe, data) in splitter.push(&chunk[..n]) {
            latest.publish(EncodedUnit { keyframe, data });
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
}

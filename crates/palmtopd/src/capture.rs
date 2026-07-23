//! Platform-neutral capture types: the single-slot mailbox both the Linux
//! (portal + PipeWire) and Windows (Windows.Graphics.Capture) capture
//! backends publish into, and the clock they timestamp frames with.
//!
//! The backends themselves -- the part that actually talks to the OS -- live
//! under `platform::linux`/`platform::windows`. Nothing here should ever
//! need a `cfg(...)`; if it does, it belongs in `platform/` instead.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The pixel layout of a captured frame's bytes.
///
/// `encode.rs` hardcodes `-pix_fmt bgra` when it spawns ffmpeg (see
/// `encode::spawn`), so `Bgra` is the only format the pipeline is actually
/// built to feed it. `Other` exists purely so a capture backend can report
/// *what* it negotiated instead when that assumption doesn't hold --
/// `encode::run_feeder` refuses to feed ffmpeg anything else, and a specific
/// name in that refusal is the difference between "compositor negotiated
/// RGBx, refusing" and a bare "wrong format" with nothing to grep for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra,
    Other(String),
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Tightly packed (stride padding stripped), one row after another.
    pub bytes: Vec<u8>,
    /// [`monotonic_us`] at the moment this frame arrived from the capture
    /// backend.
    ///
    /// Travels with the frame all the way to the client, which converts it
    /// through the Ping/Pong clock offset to compute end-to-end latency.
    /// It excludes compositor->capture-backend latency -- measured
    /// separately at 4.8ms mean on Linux in Phase 0 -- so anything derived
    /// from it is a lower bound on true capture-to-display, not the whole
    /// story.
    pub capture_us: u64,
}

/// Microseconds on a process-wide monotonic clock.
///
/// `Instant` has no numeric representation, so this counts from the first
/// call. The epoch is arbitrary and differs from the client's -- which is
/// fine, and precisely what the Ping/Pong clock-offset estimate absorbs.
/// Monotonic rather than wall-clock deliberately: an NTP step or a
/// suspend/resume would otherwise make frames appear to arrive before they
/// were captured.
pub fn monotonic_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// Single-slot mailbox: publishing overwrites whatever hadn't been taken yet.
pub struct FrameSlot {
    inner: Mutex<Option<Frame>>,
    cvar: Condvar,
}

impl FrameSlot {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(None), cvar: Condvar::new() })
    }

    pub fn publish(&self, frame: Frame) {
        let mut guard = self.inner.lock().unwrap();
        *guard = Some(frame);
        self.cvar.notify_one();
    }

    /// Blocks until a frame is available or `stop` is set, whichever first.
    pub fn take_latest_blocking(&self, stop: &AtomicBool) -> Option<Frame> {
        let mut guard = self.inner.lock().unwrap();
        loop {
            if let Some(f) = guard.take() {
                return Some(f);
            }
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            let (g, _timeout) =
                self.cvar.wait_timeout(guard, Duration::from_millis(200)).unwrap();
            guard = g;
        }
    }
}

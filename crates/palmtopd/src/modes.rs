//! Quality presets: the knobs that trade picture quality and power against
//! sync, bundled into four named points rather than exposed individually.
//!
//! Why `Sync` runs at 30fps and not 60 -- now measured, not assumed. Phase 0
//! found 1080p60 *worse* than 1080p30 end-to-end on this class of device
//! (65ms vs 37ms): sixty frames a second sat near the Snapdragon 695's
//! decode ceiling, and once utilisation approaches 1 the queuing latency
//! balloons -- more frames arrive and each lands later, the opposite of what
//! a "fast" preset is for.
//!
//! The open question was whether 720p60, being a much lighter load, escapes
//! that. It does not: measured on the same link and content, 720p60 came in
//! at **67ms** end-to-end against 720p30's **52ms**. Same shape as Phase 0,
//! just less severe. Framerate is the wrong axis to spend latency budget on
//! here; resolution and bitrate are where the wins are. The device profile's
//! `[limits] max_fps` remains the per-device authority.

/// Wire-stable discriminants: these cross the protocol as `Message::SetMode`,
/// so they must not be renumbered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Sync = 0,
    Balanced = 1,
    Quality = 2,
    Battery = 3,
}

pub struct Preset {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub maxrate_kbps: u32,
    /// Keyframe interval, in frames.
    ///
    /// Shorter is better for recovery: a dropped P-frame corrupts decode until
    /// the next keyframe resyncs, so a tight GOP bounds that glitch window.
    /// It costs bitrate, which is why the slower presets relax it.
    pub gop: u32,
    /// How stale a frame may be, client-side, before it is skipped.
    pub drop_budget_ms: u32,
    /// Kernel socket send-buffer cap -- see `session::set_send_buffer`.
    pub sndbuf_bytes: usize,
}

/// Kernel send buffers below this are counterproductive: a keyframe would not
/// fit, so the writer would stall mid-frame for no latency benefit at all.
const SNDBUF_FLOOR: usize = 64 * 1024;

/// Bound the kernel queue at roughly this much airtime. Larger, and stale
/// frames pile up somewhere `LatestEncoded` cannot reach to drop them.
const SNDBUF_MILLIS: usize = 100;

const fn sndbuf_for(maxrate_kbps: u32) -> usize {
    let bytes_per_sec = maxrate_kbps as usize * 1000 / 8;
    let bytes = bytes_per_sec * SNDBUF_MILLIS / 1000;
    if bytes < SNDBUF_FLOOR {
        SNDBUF_FLOOR
    } else {
        bytes
    }
}

impl Mode {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// `None` for an unrecognised discriminant.
    ///
    /// Deliberately not defaulting to anything: a client from a different
    /// protocol version should produce a visible error, not quietly receive a
    /// mode it never asked for, which would show up only as "the picture looks
    /// wrong somehow" much later.
    pub fn from_u8(v: u8) -> Option<Mode> {
        match v {
            0 => Some(Mode::Sync),
            1 => Some(Mode::Balanced),
            2 => Some(Mode::Quality),
            3 => Some(Mode::Battery),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Sync => "sync",
            Mode::Balanced => "balanced",
            Mode::Quality => "quality",
            Mode::Battery => "battery",
        }
    }

    pub fn preset(self) -> Preset {
        match self {
            Mode::Sync => Preset {
                width: 1280,
                height: 720,
                fps: 30,
                maxrate_kbps: 6000,
                gop: 8,
                drop_budget_ms: 40,
                sndbuf_bytes: sndbuf_for(6000),
            },
            Mode::Balanced => Preset {
                width: 1920,
                height: 1080,
                fps: 30,
                maxrate_kbps: 8000,
                gop: 15,
                drop_budget_ms: 80,
                sndbuf_bytes: sndbuf_for(8000),
            },
            Mode::Quality => Preset {
                width: 1920,
                height: 1080,
                fps: 30,
                maxrate_kbps: 16000,
                gop: 30,
                drop_budget_ms: 150,
                sndbuf_bytes: sndbuf_for(16000),
            },
            Mode::Battery => Preset {
                width: 1280,
                height: 720,
                fps: 20,
                maxrate_kbps: 3000,
                gop: 20,
                drop_budget_ms: 120,
                sndbuf_bytes: sndbuf_for(3000),
            },
        }
    }
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Balanced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_discriminants_round_trip() {
        for m in [Mode::Sync, Mode::Balanced, Mode::Quality, Mode::Battery] {
            assert_eq!(Mode::from_u8(m.as_u8()), Some(m));
        }
    }

    /// An unknown discriminant must be rejected, not coerced to a default --
    /// a version skew should surface as an error, not as the wrong picture
    /// quality that nobody notices for weeks.
    #[test]
    fn unknown_mode_is_rejected() {
        assert_eq!(Mode::from_u8(200), None);
        assert_eq!(Mode::from_u8(4), None);
    }

    #[test]
    fn sync_is_lower_latency_than_quality_on_every_axis() {
        let s = Mode::Sync.preset();
        let q = Mode::Quality.preset();
        assert!(s.width * s.height < q.width * q.height, "fewer pixels");
        assert!(s.gop < q.gop, "tighter GOP");
        assert!(s.drop_budget_ms < q.drop_budget_ms, "stricter staleness budget");
    }

    #[test]
    fn battery_is_the_cheapest_preset() {
        let b = Mode::Battery.preset();
        for other in [Mode::Sync, Mode::Balanced, Mode::Quality] {
            let o = other.preset();
            assert!(b.maxrate_kbps <= o.maxrate_kbps, "{} bitrate", other.name());
            assert!(b.fps <= o.fps, "{} fps", other.name());
        }
    }

    #[test]
    fn send_buffer_never_below_the_floor() {
        for m in [Mode::Sync, Mode::Balanced, Mode::Quality, Mode::Battery] {
            assert!(
                m.preset().sndbuf_bytes >= SNDBUF_FLOOR,
                "{} would not fit a keyframe",
                m.name()
            );
        }
    }

    /// The send buffer should track the bitrate, otherwise the higher-quality
    /// presets would be throttled by a queue sized for the cheap ones.
    #[test]
    fn send_buffer_scales_with_bitrate_above_the_floor() {
        assert!(Mode::Quality.preset().sndbuf_bytes > Mode::Sync.preset().sndbuf_bytes);
    }
}

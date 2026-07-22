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

/// Floors applied to a client's self-reported decode capability before it is
/// believed. Below these, the numbers are not a real device's limits -- they
/// are a failed query, an old build, or a peer sending nonsense -- and
/// honouring them literally yields a stream too small to be of any use.
const MIN_CREDIBLE_DECODE_WIDTH: u32 = 320;
const MIN_CREDIBLE_DECODE_HEIGHT: u32 = 240;
/// Likewise for frame rate: any H.264 decoder shipped this decade manages
/// this, so a lower claim says the query failed rather than that the hardware
/// is genuinely that slow.
const MIN_CREDIBLE_FPS: u32 = 15;

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

impl Preset {
    /// Narrows this preset to what the connected phone actually reports it
    /// can handle (see `palmtop_proto::DeviceProfile`).
    ///
    /// Only ever reduces, never raises: the presets are the ceiling, the
    /// device profile is a further constraint on top. A phone claiming it
    /// can decode 4K does not get 4K, because the presets are also chosen
    /// for bandwidth and encode cost, not just decode capability.
    ///
    /// # Why clamp to decode capability but *not* to screen size
    /// Sending more pixels than the decoder can comfortably handle is
    /// actively harmful -- Phase 0 measured 1080p60 as *worse* end-to-end
    /// than 1080p30 on this class of hardware, because utilisation
    /// approaching 1 makes queuing latency balloon even while throughput
    /// still looks fine. Sending more pixels than the *screen* can show is
    /// merely wasteful, and not always even that: the pinch-zoom feature
    /// means extra resolution is genuinely useful when magnified, so a
    /// 720p-screen phone still benefits from a 1080p stream. Clamping to
    /// the panel would quietly destroy that.
    pub fn clamped_to(mut self, profile: &palmtop_proto::DeviceProfile) -> Preset {
        // Scale uniformly rather than clamping each axis independently --
        // clamping separately would change the aspect ratio and hand the
        // client a stream shaped differently from the desktop it shows.
        //
        // The reported ceilings are floored at something credible first.
        // A client reporting zero (a failed capability query, an old build,
        // or simply a hostile peer) would otherwise scale the stream to
        // literally 0x0 -- the guard has to be on the *inputs*, because
        // guarding only the output still lets a near-zero ceiling produce a
        // 2x1 stream that is technically non-degenerate and entirely useless.
        let max_w = profile.max_decode_width.max(MIN_CREDIBLE_DECODE_WIDTH);
        let max_h = profile.max_decode_height.max(MIN_CREDIBLE_DECODE_HEIGHT);
        if self.width > max_w || self.height > max_h {
            let scale = f64::min(max_w as f64 / self.width as f64, max_h as f64 / self.height as f64);
            // Round down to even numbers: H.264 chroma subsampling requires
            // even dimensions, and an odd width would be rejected or
            // silently corrected by the encoder.
            self.width = (((self.width as f64 * scale) as u32) / 2) * 2;
            self.height = (((self.height as f64 * scale) as u32) / 2) * 2;
        }

        // Never exceed what the decoder claims, nor what the panel can
        // actually show -- frames rendered faster than the display refreshes
        // are encoded, transmitted and decoded only to be discarded.
        let fps_ceiling = profile
            .max_decode_fps
            .min(profile.refresh_hz)
            .max(MIN_CREDIBLE_FPS);
        let original_fps = self.fps.max(1);
        self.fps = self.fps.min(fps_ceiling);

        // GOP is expressed in *frames*, so dropping the frame rate without
        // touching it would silently stretch the keyframe interval in
        // seconds -- widening the window during which a dropped frame leaves
        // the picture corrupted, which is the opposite of what the faster
        // presets tightened it for.
        //
        // Rescaled to hold that interval as close to constant as a whole
        // number of frames allows. It cannot be held exactly: 0.5s at 15fps
        // is 7.5 frames, and there is no such thing as half a keyframe. The
        // residual error is therefore bounded by one frame period, and
        // rounding (rather than truncating) is what keeps it to half of one
        // in the typical case.
        if self.fps < original_fps {
            let rescaled = (self.gop as f64 * self.fps as f64 / original_fps as f64).round();
            self.gop = (rescaled as u32).max(1);
        }
        self.gop = self.gop.max(1);

        self
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

    // ---- clamping to a connected device's reported capability ----

    fn profile(max_w: u32, max_h: u32, max_fps: u32, refresh: u32) -> palmtop_proto::DeviceProfile {
        palmtop_proto::DeviceProfile {
            model: "test".into(),
            screen_width: 1080,
            screen_height: 2400,
            density_dpi: 420,
            refresh_hz: refresh,
            max_decode_width: max_w,
            max_decode_height: max_h,
            max_decode_fps: max_fps,
            low_latency_decoder: true,
        }
    }

    #[test]
    fn a_capable_device_gets_the_preset_untouched() {
        let p = Mode::Balanced.preset().clamped_to(&profile(1920, 1080, 60, 60));
        assert_eq!((p.width, p.height, p.fps), (1920, 1080, 30));
    }

    #[test]
    fn a_weaker_decoder_scales_the_resolution_down_uniformly() {
        // 1920x1080 into a 1280x720 decoder ceiling: both axes bind equally
        // (identical aspect), so it lands exactly on 1280x720.
        let p = Mode::Balanced.preset().clamped_to(&profile(1280, 720, 60, 60));
        assert_eq!((p.width, p.height), (1280, 720));
    }

    /// Clamping each axis independently would reshape the picture; the
    /// desktop would arrive stretched. Uniform scaling is the whole point.
    #[test]
    fn clamping_preserves_aspect_ratio_when_only_one_axis_binds() {
        // Only width binds (1000 < 1920), height has room to spare.
        let p = Mode::Balanced.preset().clamped_to(&profile(1000, 4000, 60, 60));
        let original = 1920.0 / 1080.0;
        let clamped = p.width as f64 / p.height as f64;
        assert!((original - clamped).abs() < 0.01, "aspect drifted: {p:?}", p = (p.width, p.height));
        assert!(p.width <= 1000);
    }

    #[test]
    fn dimensions_stay_even_for_h264_chroma_subsampling() {
        for max in [999, 1001, 1333, 777] {
            let p = Mode::Balanced.preset().clamped_to(&profile(max, max, 60, 60));
            assert_eq!(p.width % 2, 0, "odd width from max={max}");
            assert_eq!(p.height % 2, 0, "odd height from max={max}");
        }
    }

    #[test]
    fn fps_is_capped_by_both_decoder_and_panel_refresh() {
        // Decoder allows 60 but the panel only refreshes at 24 -- frames
        // beyond the refresh rate would be decoded only to be discarded.
        let p = Mode::Balanced.preset().clamped_to(&profile(1920, 1080, 60, 24));
        assert_eq!(p.fps, 24);

        // And the reverse: fast panel, slow decoder.
        let p = Mode::Balanced.preset().clamped_to(&profile(1920, 1080, 15, 120));
        assert_eq!(p.fps, 15);
    }

    /// GOP is counted in frames, so halving the frame rate without adjusting
    /// it would double the keyframe interval in seconds -- silently widening
    /// the corruption window the fast presets exist to keep narrow.
    ///
    /// The interval cannot be held *exactly*, and the tolerance here says so
    /// rather than papering over it: 0.5s at 15fps is 7.5 frames, and there
    /// is no half keyframe. One frame period is the tightest bound that is
    /// actually achievable, so that is what gets asserted.
    #[test]
    fn gop_rescales_so_the_keyframe_interval_stays_within_one_frame() {
        let base = Mode::Balanced.preset(); // 30fps, gop 15 -> 0.5s
        let clamped = Mode::Balanced.preset().clamped_to(&profile(1920, 1080, 15, 60));
        assert_eq!(clamped.fps, 15);

        let base_seconds = base.gop as f64 / base.fps as f64;
        let clamped_seconds = clamped.gop as f64 / clamped.fps as f64;
        let one_frame = 1.0 / clamped.fps as f64;
        assert!((base_seconds - clamped_seconds).abs() <= one_frame,
                "interval drifted more than one frame: {base_seconds}s -> {clamped_seconds}s");

        // And the point of the rescale at all: it must not have simply left
        // the frame count alone, which would have doubled the interval.
        assert!(clamped.gop < base.gop, "gop was not rescaled at all");
    }

    /// A client reporting all zeroes -- a failed capability query, an old
    /// build, or a hostile peer -- must still produce a stream someone could
    /// actually look at. Caught a real bug: guarding only the *ceiling*
    /// against zero still scaled 1920x1080 down to exactly 0x0, because the
    /// guard has to be on the inputs to the scale, not its output.
    #[test]
    fn a_nonsense_profile_still_yields_a_usable_stream() {
        let p = Mode::Balanced.preset().clamped_to(&profile(0, 0, 0, 0));
        assert!(p.width >= MIN_CREDIBLE_DECODE_WIDTH.min(320),
                "degenerate width: {}", p.width);
        assert!(p.height >= 180, "degenerate height: {}", p.height);
        assert!(p.fps >= MIN_CREDIBLE_FPS, "unusable fps: {}", p.fps);
        assert!(p.gop > 0, "zero gop");
        assert_eq!(p.width % 2, 0);
        assert_eq!(p.height % 2, 0);
    }
}

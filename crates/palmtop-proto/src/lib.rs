//! Wire protocol shared by `palmtopd` (host) and the Android client.
//!
//! One TCP connection, multiplexed by message type -- matches the plan's "single
//! connection acceptable for MVP given LAN reliability" call (§3.5), and mirrors
//! what the Phase 0 spikes already validated end-to-end (scrcpy takes the same
//! approach). Framing is a hand-rolled TLV: `[1-byte tag][4-byte BE length][payload]`.
//! No serde/bincode dependency -- the format is simple enough that owning it
//! directly keeps the wire compact and the version-negotiation story explicit.

use std::io::{self, Read, Write};

use anyhow::{bail, Context, Result};

pub mod noise;
pub use noise::NoiseTransport;

/// Bumped on any wire-incompatible change. `Hello` carries this so a version
/// mismatch is a clean refusal (plan §9 "version skew") rather than garbage bytes.
/// v2 added the pairing `token` field to `Hello`.
/// v3: `Ping`/`Pong` carry timestamps -- both the keepalive plan §9 wanted
/// (v2 defined these messages but neither side ever sent one) and the
/// clock-offset probe every latency measurement depends on. `VideoFrame`
/// carries the capture timestamp for end-to-end measurement, and `SetMode`
/// selects a quality preset.
/// v4: `Hello` carries a [`DeviceProfile`], so the host tunes the stream to
/// whatever phone actually connected instead of relying on hand-written
/// per-device config that could never ship to strangers.
/// v5: `Status` lets the host narrate what it is doing and say plainly when a
/// stage fails, so a session that dies after `HelloAck` reports why on the
/// phone instead of presenting as an indefinite blank screen.
pub const PROTOCOL_VERSION: u16 = 5;

/// Cap on a single message payload. Generous for 4K video frames, small enough
/// to reject clearly-corrupt length headers instead of trying to allocate them.
const MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    fn to_u8(self) -> u8 {
        match self {
            Button::Left => 0,
            Button::Right => 1,
            Button::Middle => 2,
        }
    }
    fn from_u8(v: u8) -> Result<Self> {
        Ok(match v {
            0 => Button::Left,
            1 => Button::Right,
            2 => Button::Middle,
            _ => bail!("unknown button {v}"),
        })
    }
}

/// Modifier keys held during a `Key` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const SHIFT: Modifiers = Modifiers(1 << 0);
    pub const CTRL: Modifiers = Modifiers(1 << 1);
    pub const ALT: Modifiers = Modifiers(1 << 2);
    pub const SUPER: Modifiers = Modifiers(1 << 3);
    pub const NONE: Modifiers = Modifiers(0);

    pub fn contains(self, flag: Modifiers) -> bool {
        self.0 & flag.0 == flag.0
    }
    pub fn bits(self) -> u8 {
        self.0
    }
    pub fn from_bits_truncate(bits: u8) -> Self {
        Modifiers(bits)
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Modifiers;
    fn bitor(self, rhs: Modifiers) -> Modifiers {
        Modifiers(self.0 | rhs.0)
    }
}

/// What the client tells the host about itself, so the host can tune the
/// stream to hardware it has never seen.
///
/// This exists to delete a scaling problem, not merely as a convenience.
/// These values previously lived in hand-written `config/devices/*.toml`
/// files, populated by running `scripts/probe-device.sh` over ADB against
/// each phone -- workable for one developer with one phone on the desk, and
/// completely unshippable to strangers, who have neither the script nor any
/// reason to run it. The client already knows every one of these facts about
/// itself; having it simply say so at connect time removes the manual step
/// entirely, and means an unknown phone is configured correctly on its first
/// connection with nothing to edit.
///
/// Treated as a hint, never as a command: the host clamps its own presets
/// against these numbers but is not obliged to believe anything that would
/// produce a nonsensical stream (see `palmtopd::modes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    /// Human-readable, for logs and the host's paired-device list.
    pub model: String,
    /// Native screen size in pixels, as the device reports it.
    pub screen_width: u32,
    pub screen_height: u32,
    pub density_dpi: u32,
    /// Rounded to whole Hz -- the fractional part (a panel reporting
    /// 60.000004) has never mattered for any decision made from it.
    pub refresh_hz: u32,
    /// Largest frame the chosen hardware decoder claims it can handle, and
    /// the frame rate it claims at that size. The host will not send more
    /// than this, because exceeding a decoder's real capacity raises latency
    /// while throughput still looks fine -- measured in Phase 0, where
    /// 1080p60 came out *worse* end-to-end than 1080p30 on this class of
    /// hardware.
    pub max_decode_width: u32,
    pub max_decode_height: u32,
    pub max_decode_fps: u32,
    /// Whether a genuine low-latency decoder was found, as opposed to
    /// falling back to a general-purpose one.
    pub low_latency_decoder: bool,
}

impl DeviceProfile {
    /// A deliberately conservative stand-in used when a client says nothing
    /// useful about itself. Every value is one that virtually any Android
    /// device meeting this project's minimum can handle, so an unknown or
    /// malfunctioning client degrades to a working stream rather than to a
    /// broken one.
    pub fn conservative_default() -> Self {
        DeviceProfile {
            model: "unknown".to_string(),
            screen_width: 1280,
            screen_height: 720,
            density_dpi: 320,
            refresh_hz: 60,
            max_decode_width: 1280,
            max_decode_height: 720,
            max_decode_fps: 30,
            low_latency_decoder: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Client -> host, first message on the connection. `token` is the
    /// pairing secret from the host's QR code (see palmtopd/src/pairing.rs);
    /// an empty/wrong token is rejected the same as a version mismatch.
    /// `profile` lets the host size the stream to this specific phone -- see
    /// [`DeviceProfile`].
    Hello { protocol_version: u16, token: String, profile: DeviceProfile },
    /// Host -> client, response to `Hello`. `ok=false` means version
    /// mismatch *or* pairing rejection; the connection should be closed
    /// after reading the reason.
    HelloAck { ok: bool, reason: String },

    /// Host -> client, sent once and again whenever the quality mode
    /// changes the stream. Everything the client receives *after* this
    /// message is in the announced format -- TCP ordering is what makes
    /// that unambiguous, so the client can rebuild its decoder on receipt
    /// without having to guess which frames belong to which config.
    ///
    /// `mode` is the preset actually in force, echoed back rather than
    /// assumed: a client that requested a mode the host rejected should
    /// display what it is really getting, not what it asked for.
    /// `drop_budget_ms` is how stale a frame may be before the client
    /// should skip it. It travels with the config so the preset table has
    /// exactly one definition, on the host -- duplicating it in the client
    /// would let the two drift apart silently.
    VideoConfig {
        codec: String,
        width: u32,
        height: u32,
        fps: u32,
        mode: u8,
        drop_budget_ms: u32,
    },
    /// Host -> client. `keyframe` lets the client log/measure without parsing
    /// NALs. `capture_us` is the host's monotonic clock at the moment the
    /// capture thread received this frame from PipeWire; the client converts
    /// it through the Ping/Pong clock offset to compute end-to-end latency.
    /// It excludes compositor->PipeWire latency (measured separately at 4.8ms
    /// mean in Phase 0), so figures derived from it are a lower bound.
    VideoFrame { keyframe: bool, capture_us: u64, data: Vec<u8> },

    /// Client -> host: relative pointer motion (trackpad mode).
    PointerMotionRelative { dx: f32, dy: f32 },
    /// Client -> host: absolute pointer position, normalised to [0, 1] on each
    /// axis so it is independent of the negotiated video resolution.
    PointerMotionAbsolute { x: f32, y: f32 },
    /// Client -> host.
    PointerButton { button: Button, pressed: bool },
    /// Client -> host. High-resolution scroll units (matches wl_pointer axis
    /// semantics used by the proven wlr-input spike): positive = down/right.
    Scroll { dx: f32, dy: f32 },

    /// Host -> client: a running account of what the host is doing, so a
    /// session that fails partway through says so instead of just going quiet.
    ///
    /// This exists because every failure after `HelloAck` used to be invisible
    /// from the phone. The host would request the screen-share portal, fail to
    /// start its encoder, or have its capture stream torn down -- and in every
    /// case the phone showed the same blank screen with no indication whether
    /// it was waiting on a human to approve a dialog, waiting on a broken GPU,
    /// or simply not connected. The operator could read `journalctl` on the
    /// laptop; the person holding the phone could not.
    ///
    /// `stage` is a short stable identifier (`portal`, `capture`, `encode`,
    /// `stream`), suitable for the client to key off. `detail` is
    /// human-readable and may change freely. `ok=false` means this stage
    /// failed; the session usually ends immediately after.
    Status { stage: String, ok: bool, detail: String },

    /// Client -> host: a single key by evdev keycode, for keys the client can
    /// map itself (letters, digits, common punctuation, arrows, modifiers).
    Key { evdev_code: u32, pressed: bool, modifiers: Modifiers },
    /// Client -> host: arbitrary Unicode text the client could not map to a
    /// simple keycode (emoji, CJK, IME composition). The host resolves this via
    /// an uploaded xkb keymap rather than the client guessing keycodes -- see
    /// plan §3.3/§9 (keyboard layout mismatch, dead keys, IME).
    Text { utf8: String },

    /// Client -> host: select a quality preset. `mode` is a
    /// `palmtopd::modes::Mode` discriminant. Unknown values are rejected by
    /// the host rather than silently defaulting, so a version skew surfaces as
    /// an error instead of as the wrong picture quality that nobody notices.
    SetMode { mode: u8 },

    /// Client -> host: idle-connection keepalive so a NAT/router doesn't
    /// silently drop the session (plan §9 "Wi-Fi power save dropping
    /// packets"), *and* the clock-sync probe every latency measurement
    /// depends on. `t_client_us` is the client's monotonic clock at send.
    Ping { nonce: u64, t_client_us: u64 },
    /// Host -> client reply. Echoes `t_client_us` untouched and adds the
    /// host's own monotonic clock at receive and at send. Those four
    /// timestamps are exactly what the NTP offset/RTT formulas need -- see
    /// `LatencyTracker.java`. Sending only a nonce back (as v2 did) would
    /// make RTT measurable but the clock offset unknowable.
    Pong { nonce: u64, t_client_us: u64, t_host_recv_us: u64, t_host_send_us: u64 },
}

#[repr(u8)]
enum Tag {
    Hello = 1,
    HelloAck = 2,
    VideoConfig = 3,
    VideoFrame = 4,
    PointerMotionRelative = 5,
    PointerMotionAbsolute = 6,
    PointerButton = 7,
    Scroll = 8,
    Key = 9,
    Text = 10,
    Ping = 11,
    Pong = 12,
    SetMode = 13,
    Status = 14,
}

impl Message {
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<()> {
        let mut payload = Vec::new();
        let tag = match self {
            Message::Hello { protocol_version, token, profile } => {
                payload.extend_from_slice(&protocol_version.to_be_bytes());
                write_str(&mut payload, token);
                write_str(&mut payload, &profile.model);
                for v in [
                    profile.screen_width,
                    profile.screen_height,
                    profile.density_dpi,
                    profile.refresh_hz,
                    profile.max_decode_width,
                    profile.max_decode_height,
                    profile.max_decode_fps,
                ] {
                    payload.extend_from_slice(&v.to_be_bytes());
                }
                payload.push(profile.low_latency_decoder as u8);
                Tag::Hello
            }
            Message::HelloAck { ok, reason } => {
                payload.push(*ok as u8);
                write_str(&mut payload, reason);
                Tag::HelloAck
            }
            Message::VideoConfig { codec, width, height, fps, mode, drop_budget_ms } => {
                write_str(&mut payload, codec);
                payload.extend_from_slice(&width.to_be_bytes());
                payload.extend_from_slice(&height.to_be_bytes());
                payload.extend_from_slice(&fps.to_be_bytes());
                payload.push(*mode);
                payload.extend_from_slice(&drop_budget_ms.to_be_bytes());
                Tag::VideoConfig
            }
            Message::VideoFrame { keyframe, capture_us, data } => {
                payload.push(*keyframe as u8);
                payload.extend_from_slice(&capture_us.to_be_bytes());
                payload.extend_from_slice(data);
                Tag::VideoFrame
            }
            Message::PointerMotionRelative { dx, dy } => {
                payload.extend_from_slice(&dx.to_be_bytes());
                payload.extend_from_slice(&dy.to_be_bytes());
                Tag::PointerMotionRelative
            }
            Message::PointerMotionAbsolute { x, y } => {
                payload.extend_from_slice(&x.to_be_bytes());
                payload.extend_from_slice(&y.to_be_bytes());
                Tag::PointerMotionAbsolute
            }
            Message::PointerButton { button, pressed } => {
                payload.push(button.to_u8());
                payload.push(*pressed as u8);
                Tag::PointerButton
            }
            Message::Scroll { dx, dy } => {
                payload.extend_from_slice(&dx.to_be_bytes());
                payload.extend_from_slice(&dy.to_be_bytes());
                Tag::Scroll
            }
            Message::Key { evdev_code, pressed, modifiers } => {
                payload.extend_from_slice(&evdev_code.to_be_bytes());
                payload.push(*pressed as u8);
                payload.push(modifiers.bits());
                Tag::Key
            }
            Message::Text { utf8 } => {
                write_str(&mut payload, utf8);
                Tag::Text
            }
            Message::SetMode { mode } => {
                payload.push(*mode);
                Tag::SetMode
            }
            Message::Ping { nonce, t_client_us } => {
                payload.extend_from_slice(&nonce.to_be_bytes());
                payload.extend_from_slice(&t_client_us.to_be_bytes());
                Tag::Ping
            }
            Message::Pong { nonce, t_client_us, t_host_recv_us, t_host_send_us } => {
                payload.extend_from_slice(&nonce.to_be_bytes());
                payload.extend_from_slice(&t_client_us.to_be_bytes());
                payload.extend_from_slice(&t_host_recv_us.to_be_bytes());
                payload.extend_from_slice(&t_host_send_us.to_be_bytes());
                Tag::Pong
            }
            Message::Status { stage, ok, detail } => {
                write_str(&mut payload, stage);
                payload.push(*ok as u8);
                write_str(&mut payload, detail);
                Tag::Status
            }
        };

        w.write_all(&[tag as u8])?;
        w.write_all(&(payload.len() as u32).to_be_bytes())?;
        w.write_all(&payload)?;
        Ok(())
    }

    /// Blocks until a full message is available, an error occurs, or the
    /// stream is cleanly closed at a message boundary (returns `Ok(None)`).
    pub fn read_from<R: Read>(r: &mut R) -> Result<Option<Message>> {
        let mut tag_buf = [0u8; 1];
        match r.read_exact(&mut tag_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e).context("read tag"),
        }

        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf).context("read length")?;
        let len = u32::from_be_bytes(len_buf);
        if len > MAX_PAYLOAD {
            bail!("payload length {len} exceeds max {MAX_PAYLOAD} -- corrupt stream?");
        }
        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload).context("read payload")?;

        let mut p = &payload[..];
        let msg = match tag_buf[0] {
            t if t == Tag::Hello as u8 => {
                let protocol_version = read_u16(&mut p)?;
                let token = read_str(&mut p)?;
                let profile = DeviceProfile {
                    model: read_str(&mut p)?,
                    screen_width: read_u32(&mut p)?,
                    screen_height: read_u32(&mut p)?,
                    density_dpi: read_u32(&mut p)?,
                    refresh_hz: read_u32(&mut p)?,
                    max_decode_width: read_u32(&mut p)?,
                    max_decode_height: read_u32(&mut p)?,
                    max_decode_fps: read_u32(&mut p)?,
                    low_latency_decoder: read_u8(&mut p)? != 0,
                };
                Message::Hello { protocol_version, token, profile }
            }
            t if t == Tag::HelloAck as u8 => {
                let ok = read_u8(&mut p)? != 0;
                let reason = read_str(&mut p)?;
                Message::HelloAck { ok, reason }
            }
            t if t == Tag::VideoConfig as u8 => {
                let codec = read_str(&mut p)?;
                let width = read_u32(&mut p)?;
                let height = read_u32(&mut p)?;
                let fps = read_u32(&mut p)?;
                let mode = read_u8(&mut p)?;
                let drop_budget_ms = read_u32(&mut p)?;
                Message::VideoConfig { codec, width, height, fps, mode, drop_budget_ms }
            }
            t if t == Tag::VideoFrame as u8 => {
                let keyframe = read_u8(&mut p)? != 0;
                let capture_us = read_u64(&mut p)?;
                Message::VideoFrame { keyframe, capture_us, data: p.to_vec() }
            }
            t if t == Tag::PointerMotionRelative as u8 => {
                let dx = read_f32(&mut p)?;
                let dy = read_f32(&mut p)?;
                Message::PointerMotionRelative { dx, dy }
            }
            t if t == Tag::PointerMotionAbsolute as u8 => {
                let x = read_f32(&mut p)?;
                let y = read_f32(&mut p)?;
                Message::PointerMotionAbsolute { x, y }
            }
            t if t == Tag::PointerButton as u8 => {
                let button = Button::from_u8(read_u8(&mut p)?)?;
                let pressed = read_u8(&mut p)? != 0;
                Message::PointerButton { button, pressed }
            }
            t if t == Tag::Scroll as u8 => {
                let dx = read_f32(&mut p)?;
                let dy = read_f32(&mut p)?;
                Message::Scroll { dx, dy }
            }
            t if t == Tag::Key as u8 => {
                let evdev_code = read_u32(&mut p)?;
                let pressed = read_u8(&mut p)? != 0;
                let modifiers = Modifiers::from_bits_truncate(read_u8(&mut p)?);
                Message::Key { evdev_code, pressed, modifiers }
            }
            t if t == Tag::Text as u8 => Message::Text { utf8: read_str(&mut p)? },
            t if t == Tag::SetMode as u8 => Message::SetMode { mode: read_u8(&mut p)? },
            t if t == Tag::Ping as u8 => Message::Ping {
                nonce: read_u64(&mut p)?,
                t_client_us: read_u64(&mut p)?,
            },
            t if t == Tag::Pong as u8 => Message::Pong {
                nonce: read_u64(&mut p)?,
                t_client_us: read_u64(&mut p)?,
                t_host_recv_us: read_u64(&mut p)?,
                t_host_send_us: read_u64(&mut p)?,
            },
            t if t == Tag::Status as u8 => Message::Status {
                stage: read_str(&mut p)?,
                ok: read_u8(&mut p)? != 0,
                detail: read_str(&mut p)?,
            },
            other => bail!("unknown message tag {other}"),
        };
        Ok(Some(msg))
    }
}

fn write_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
}
fn read_str(p: &mut &[u8]) -> Result<String> {
    let len = read_u32(p)? as usize;
    if p.len() < len {
        bail!("truncated string field");
    }
    let (s, rest) = p.split_at(len);
    *p = rest;
    Ok(String::from_utf8(s.to_vec())?)
}
fn read_u8(p: &mut &[u8]) -> Result<u8> {
    if p.is_empty() {
        bail!("truncated u8 field");
    }
    let v = p[0];
    *p = &p[1..];
    Ok(v)
}
fn read_u16(p: &mut &[u8]) -> Result<u16> {
    if p.len() < 2 {
        bail!("truncated u16 field");
    }
    let v = u16::from_be_bytes([p[0], p[1]]);
    *p = &p[2..];
    Ok(v)
}
fn read_u32(p: &mut &[u8]) -> Result<u32> {
    if p.len() < 4 {
        bail!("truncated u32 field");
    }
    let v = u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
    *p = &p[4..];
    Ok(v)
}
fn read_u64(p: &mut &[u8]) -> Result<u64> {
    if p.len() < 8 {
        bail!("truncated u64 field");
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&p[..8]);
    *p = &p[8..];
    Ok(u64::from_be_bytes(b))
}
fn read_f32(p: &mut &[u8]) -> Result<f32> {
    Ok(f32::from_bits(read_u32(p)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(msg: Message) -> Message {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        Message::read_from(&mut &buf[..]).unwrap().unwrap()
    }

    #[test]
    fn hello_roundtrips() {
        let token = "deadbeef".to_string();
        // Deliberately not the conservative default -- distinct values in
        // every field so a serialisation bug that transposed two of them
        // (easy to do with seven consecutive u32s) cannot pass unnoticed.
        let profile = DeviceProfile {
            model: "Pixel 9 Pro".to_string(),
            screen_width: 1280,
            screen_height: 2856,
            density_dpi: 495,
            refresh_hz: 120,
            max_decode_width: 3840,
            max_decode_height: 2160,
            max_decode_fps: 60,
            low_latency_decoder: true,
        };
        match roundtrip(Message::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: token.clone(),
            profile: profile.clone(),
        }) {
            Message::Hello { protocol_version, token: t, profile: p } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(t, token);
                assert_eq!(p, profile);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn hello_roundtrips_a_profile_with_a_unicode_model_name() {
        // Device model strings are vendor-supplied and are not guaranteed
        // ASCII; a length-prefixed byte count (not a char count) is what
        // makes this work, so it is worth pinning.
        let mut profile = DeviceProfile::conservative_default();
        profile.model = "小米 14 Ultra".to_string();
        match roundtrip(Message::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: String::new(),
            profile: profile.clone(),
        }) {
            Message::Hello { profile: p, .. } => assert_eq!(p, profile),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn status_roundtrips_both_outcomes() {
        // The failure case carries the message a user will actually read on
        // their phone, so it must survive the wire intact -- including the
        // newlines and punctuation a diagnostic explanation needs.
        match roundtrip(Message::Status {
            stage: "encode".to_string(),
            ok: false,
            detail: "ffmpeg exited immediately:\n  cannot open /dev/dri/renderD128".to_string(),
        }) {
            Message::Status { stage, ok, detail } => {
                assert_eq!(stage, "encode");
                assert!(!ok);
                assert!(detail.contains("renderD128"));
                assert!(detail.contains('\n'));
            }
            other => panic!("wrong variant: {other:?}"),
        }

        match roundtrip(Message::Status {
            stage: "portal".to_string(),
            ok: true,
            detail: String::new(),
        }) {
            Message::Status { stage, ok, detail } => {
                assert_eq!(stage, "portal");
                assert!(ok);
                assert!(detail.is_empty());
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn video_frame_roundtrips_bytes_exactly() {
        let data = vec![0u8, 1, 2, 255, 254, 0, 0, 0];
        match roundtrip(Message::VideoFrame { keyframe: true, capture_us: 0, data: data.clone() }) {
            Message::VideoFrame { keyframe, data: d, .. } => {
                assert!(keyframe);
                assert_eq!(d, data);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn pointer_motion_preserves_float_precision() {
        match roundtrip(Message::PointerMotionRelative { dx: -12.375, dy: 0.001 }) {
            Message::PointerMotionRelative { dx, dy } => {
                assert_eq!(dx, -12.375);
                assert_eq!(dy, 0.001);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn text_roundtrips_unicode() {
        match roundtrip(Message::Text { utf8: "héllo 世界 🎉".into() }) {
            Message::Text { utf8 } => assert_eq!(utf8, "héllo 世界 🎉"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn clean_eof_at_boundary_is_none() {
        let mut empty: &[u8] = &[];
        assert!(Message::read_from(&mut empty).unwrap().is_none());
    }

    #[test]
    fn oversized_length_is_rejected_not_allocated() {
        let mut buf = vec![Tag::VideoFrame as u8];
        buf.extend_from_slice(&(MAX_PAYLOAD + 1).to_be_bytes());
        assert!(Message::read_from(&mut &buf[..]).is_err());
    }

    #[test]
    fn ping_pong_roundtrip_carries_timestamps() {
        match roundtrip(Message::Ping { nonce: 7, t_client_us: 123_456 }) {
            Message::Ping { nonce, t_client_us } => {
                assert_eq!(nonce, 7);
                assert_eq!(t_client_us, 123_456);
            }
            other => panic!("expected Ping, got {other:?}"),
        }
        match roundtrip(Message::Pong {
            nonce: 7,
            t_client_us: 123_456,
            t_host_recv_us: 200_000,
            t_host_send_us: 200_050,
        }) {
            Message::Pong { nonce, t_client_us, t_host_recv_us, t_host_send_us } => {
                assert_eq!(nonce, 7);
                assert_eq!(t_client_us, 123_456);
                assert_eq!(t_host_recv_us, 200_000);
                assert_eq!(t_host_send_us, 200_050);
            }
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    #[test]
    fn video_frame_roundtrip_carries_capture_timestamp() {
        let payload = vec![0u8, 0, 0, 1, 0x65, 0xAA, 0xBB];
        match roundtrip(Message::VideoFrame {
            keyframe: true,
            capture_us: 987_654_321,
            data: payload.clone(),
        }) {
            Message::VideoFrame { keyframe, capture_us, data } => {
                assert!(keyframe);
                assert_eq!(capture_us, 987_654_321);
                assert_eq!(data, payload);
            }
            other => panic!("expected VideoFrame, got {other:?}"),
        }
    }

    #[test]
    fn set_mode_roundtrips() {
        match roundtrip(Message::SetMode { mode: 2 }) {
            Message::SetMode { mode } => assert_eq!(mode, 2),
            other => panic!("expected SetMode, got {other:?}"),
        }
    }

    /// Pins the wire version so bumping it is always a deliberate act --
    /// the host and client compare this exact number during the
    /// handshake, and a silent bump on one side only would present as an
    /// unexplained connection refusal.
    #[test]
    fn protocol_version_is_five() {
        assert_eq!(PROTOCOL_VERSION, 5);
    }
}

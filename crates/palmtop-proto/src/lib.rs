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

/// Bumped on any wire-incompatible change. `Hello` carries this so a version
/// mismatch is a clean refusal (plan §9 "version skew") rather than garbage bytes.
/// v2 added the pairing `token` field to `Hello`.
pub const PROTOCOL_VERSION: u16 = 2;

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

#[derive(Debug, Clone)]
pub enum Message {
    /// Client -> host, first message on the connection. `token` is the
    /// pairing secret from the host's QR code (see palmtopd/src/pairing.rs);
    /// an empty/wrong token is rejected the same as a version mismatch.
    Hello { protocol_version: u16, token: String },
    /// Host -> client, response to `Hello`. `ok=false` means version
    /// mismatch *or* pairing rejection; the connection should be closed
    /// after reading the reason.
    HelloAck { ok: bool, reason: String },

    /// Host -> client, sent once (or again on resolution change) before frames.
    VideoConfig { codec: String, width: u32, height: u32, fps: u32 },
    /// Host -> client. `keyframe` lets the client log/measure without parsing NALs.
    VideoFrame { keyframe: bool, data: Vec<u8> },

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

    /// Client -> host: a single key by evdev keycode, for keys the client can
    /// map itself (letters, digits, common punctuation, arrows, modifiers).
    Key { evdev_code: u32, pressed: bool, modifiers: Modifiers },
    /// Client -> host: arbitrary Unicode text the client could not map to a
    /// simple keycode (emoji, CJK, IME composition). The host resolves this via
    /// an uploaded xkb keymap rather than the client guessing keycodes -- see
    /// plan §3.3/§9 (keyboard layout mismatch, dead keys, IME).
    Text { utf8: String },

    /// Either direction: idle-connection keepalive so a NAT/router doesn't
    /// silently drop the session (plan §9 "Wi-Fi power save dropping packets").
    Ping { nonce: u64 },
    Pong { nonce: u64 },
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
}

impl Message {
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<()> {
        let mut payload = Vec::new();
        let tag = match self {
            Message::Hello { protocol_version, token } => {
                payload.extend_from_slice(&protocol_version.to_be_bytes());
                write_str(&mut payload, token);
                Tag::Hello
            }
            Message::HelloAck { ok, reason } => {
                payload.push(*ok as u8);
                write_str(&mut payload, reason);
                Tag::HelloAck
            }
            Message::VideoConfig { codec, width, height, fps } => {
                write_str(&mut payload, codec);
                payload.extend_from_slice(&width.to_be_bytes());
                payload.extend_from_slice(&height.to_be_bytes());
                payload.extend_from_slice(&fps.to_be_bytes());
                Tag::VideoConfig
            }
            Message::VideoFrame { keyframe, data } => {
                payload.push(*keyframe as u8);
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
            Message::Ping { nonce } => {
                payload.extend_from_slice(&nonce.to_be_bytes());
                Tag::Ping
            }
            Message::Pong { nonce } => {
                payload.extend_from_slice(&nonce.to_be_bytes());
                Tag::Pong
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
                Message::Hello { protocol_version, token }
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
                Message::VideoConfig { codec, width, height, fps }
            }
            t if t == Tag::VideoFrame as u8 => {
                let keyframe = read_u8(&mut p)? != 0;
                Message::VideoFrame { keyframe, data: p.to_vec() }
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
            t if t == Tag::Ping as u8 => Message::Ping { nonce: read_u64(&mut p)? },
            t if t == Tag::Pong as u8 => Message::Pong { nonce: read_u64(&mut p)? },
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
        match roundtrip(Message::Hello { protocol_version: PROTOCOL_VERSION, token: token.clone() }) {
            Message::Hello { protocol_version, token: t } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(t, token);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn video_frame_roundtrips_bytes_exactly() {
        let data = vec![0u8, 1, 2, 255, 254, 0, 0, 0];
        match roundtrip(Message::VideoFrame { keyframe: true, data: data.clone() }) {
            Message::VideoFrame { keyframe, data: d } => {
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
}

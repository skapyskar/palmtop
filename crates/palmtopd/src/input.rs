//! wlr tier-2 input injection, driven live by messages arriving over the
//! network -- as opposed to `spike-wlr-input`, which drove a canned demo
//! sequence to prove the protocol was accepted at all.
//!
//! Runs on its own thread for the daemon's lifetime (independent of client
//! connections) because the Wayland connection and virtual devices are cheap
//! to keep alive and expensive to recreate per session.

use std::io::Write;
use std::os::fd::AsFd;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use anyhow::{Context, Result};
use palmtop_proto::{Button as ProtoButton, Message, Modifiers};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_pointer, wl_registry, wl_seat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Normalised absolute-motion coordinate space (matches the proven pattern
/// from spike-capture-latency: resolution-independent, avoids relative-motion
/// clamping at screen edges).
const EXTENT: u32 = 1000;

/// Approximate xkb modifier bit positions for a plain "us" keymap (Shift=0,
/// Ctrl=2, Mod1/Alt=3, Mod4/Super=6). Not derived from the compiled keymap --
/// a documented simplification. Could misbehave on non-"us" layouts; revisit
/// alongside the plan's §9 "keyboard layout mismatch" hardening item.
fn modifiers_to_depressed(m: Modifiers) -> u32 {
    let mut bits = 0u32;
    if m.contains(Modifiers::SHIFT) {
        bits |= 1 << 0;
    }
    if m.contains(Modifiers::CTRL) {
        bits |= 1 << 2;
    }
    if m.contains(Modifiers::ALT) {
        bits |= 1 << 3;
    }
    if m.contains(Modifiers::SUPER) {
        bits |= 1 << 6;
    }
    bits
}

struct State;

pub fn run(rx: Receiver<Message>) -> Result<()> {
    let conn = Connection::connect_to_env().context("wayland connect")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).context("registry")?;
    let qh = queue.handle();
    let mut state = State;

    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=8, ()).context("no wl_seat")?;
    let vpm: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .context("compositor has no zwlr_virtual_pointer_manager_v1")?;
    let pointer = vpm.create_virtual_pointer(Some(&seat), &qh, ());

    let vkm: ZwpVirtualKeyboardManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("compositor has no zwp_virtual_keyboard_manager_v1")?;
    let keyboard = vkm.create_virtual_keyboard(&seat, &qh, ());
    upload_keymap(&conn, &keyboard).context("upload keymap")?;

    let start = Instant::now();
    let now_ms = || start.elapsed().as_millis() as u32;

    println!("[input] ready");
    for msg in rx {
        match msg {
            Message::PointerMotionRelative { dx, dy } => {
                pointer.motion(now_ms(), dx as f64, dy as f64);
                pointer.frame();
            }
            Message::PointerMotionAbsolute { x, y } => {
                let xi = (x.clamp(0.0, 1.0) * EXTENT as f32) as u32;
                let yi = (y.clamp(0.0, 1.0) * EXTENT as f32) as u32;
                pointer.motion_absolute(now_ms(), xi, yi, EXTENT, EXTENT);
                pointer.frame();
            }
            Message::PointerButton { button, pressed } => {
                let code = match button {
                    ProtoButton::Left => BTN_LEFT,
                    ProtoButton::Right => BTN_RIGHT,
                    ProtoButton::Middle => BTN_MIDDLE,
                };
                let state = if pressed {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                };
                pointer.button(now_ms(), code, state);
                pointer.frame();
            }
            Message::Scroll { dx, dy } => {
                if dy != 0.0 {
                    pointer.axis(now_ms(), wl_pointer::Axis::VerticalScroll, dy as f64);
                }
                if dx != 0.0 {
                    pointer.axis(now_ms(), wl_pointer::Axis::HorizontalScroll, dx as f64);
                }
                pointer.frame();
            }
            Message::Key { evdev_code, pressed, modifiers } => {
                let depressed = modifiers_to_depressed(modifiers);
                keyboard.modifiers(depressed, 0, 0, 0);
                keyboard.key(now_ms(), evdev_code, pressed as u32);
            }
            Message::Text { utf8 } => {
                // Unicode text injection needs a per-character keymap upload
                // or a compose-key path -- deferred, see plan §9 (dead keys /
                // IME / CJK). Logged rather than silently dropped so the gap
                // is visible during the core-loop milestone.
                eprintln!("[input] TODO: text injection not yet implemented, dropped {utf8:?}");
            }
            Message::Ping { .. } | Message::Pong { .. } | Message::Hello { .. }
            | Message::HelloAck { .. } | Message::VideoConfig { .. }
            | Message::VideoFrame { .. } => {
                // Not input; the network layer handles these before forwarding here.
            }
        }
        conn.flush().ok();
        let _ = queue.roundtrip(&mut state);
    }
    Ok(())
}

fn upload_keymap(conn: &Connection, vk: &ZwpVirtualKeyboardV1) -> Result<()> {
    let ctx = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
    let keymap = xkbcommon::xkb::Keymap::new_from_names(
        &ctx, "", "", "us", "", None, xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .context("build xkb keymap")?;
    let keymap_str = keymap.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1);
    let bytes = keymap_str.as_bytes();

    let mut file = tempfile()?;
    file.write_all(bytes)?;
    file.write_all(&[0])?; // NUL-terminate
    file.flush().ok();

    vk.keymap(1 /* xkb_v1 */, file.as_fd(), (bytes.len() + 1) as u32);
    conn.flush().context("flush keymap upload")?;
    Ok(())
}

fn tempfile() -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = std::env::temp_dir().join(format!("palmtopd-keymap-{}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    let _ = std::fs::remove_file(&path); // unlink; fd stays valid
    Ok(file)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
macro_rules! inert {
    ($($t:ty),* $(,)?) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event,
                     _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
inert!(
    wl_seat::WlSeat,
    ZwlrVirtualPointerManagerV1,
    ZwlrVirtualPointerV1,
    ZwpVirtualKeyboardManagerV1,
    ZwpVirtualKeyboardV1,
);

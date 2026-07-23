//! `SendInput`-based input injection -- the Windows counterpart to
//! `platform::linux::input`, implementing the same `run(rx) -> Result<()>`
//! contract so `main.rs` never needs a `cfg(windows)` of its own.
//!
//! # What this cannot inherit from the Linux side
//!
//! Unlike `platform::linux::input`, this file's correctness cannot be
//! confirmed on the machine that wrote it: there is no Windows target
//! installed here (no `rustup`, no `mingw-w64` -- see the Windows
//! host-support plan's research notes), so this has never been compiled,
//! let alone run against a real `SendInput` call. The struct field names and
//! flag values below reflect the `windows` crate's documented API as best
//! confirmed via Microsoft Learn and the crate's own generated docs during
//! this session, not a local build. Treat every detail here -- field
//! names, casing, flag values -- as needing confirmation the first time
//! this actually compiles on Windows, the same way `palmtop_config`'s
//! `BCryptGenRandom` call is flagged.
//!
//! The one piece of this file that *is* independently verified is the
//! evdev->scancode mapping itself: that lives in `keymap`, which is plain
//! Rust with no Windows dependency and has real, passing unit tests.

use std::sync::mpsc::Receiver;

use anyhow::Result;
use palmtop_proto::{Button as ProtoButton, Message};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};

use super::keymap;

/// The normalized-coordinate extent `MOUSEEVENTF_ABSOLUTE` expects: 0 maps
/// to the left/top of the virtual desktop, 65535 to the right/bottom,
/// regardless of actual pixel resolution. `PointerMotionAbsolute` already
/// arrives as [0, 1] floats (see `palmtop_proto::Message`), so this is a
/// straight scale, not a coordinate-space change.
const ABSOLUTE_EXTENT: f32 = 65535.0;

/// One wheel "click" in `SendInput`'s units, per Microsoft's own constant
/// (`WHEEL_DELTA`). `Scroll`'s `dx`/`dy` are already in wl_pointer-style
/// high-resolution axis units (see the message's doc comment in
/// palmtop-proto), so this scales them into what `MOUSEEVENTF_WHEEL`
/// expects rather than assuming the units already match.
const WHEEL_DELTA: f32 = 120.0;

pub fn run(rx: Receiver<Message>) -> Result<()> {
    println!("[input] ready (Windows SendInput)");
    for msg in rx {
        match msg {
            Message::PointerMotionRelative { dx, dy } => {
                send_mouse(dx as i32, dy as i32, 0, MOUSEEVENTF_MOVE, 0);
            }
            Message::PointerMotionAbsolute { x, y } => {
                let xi = (x.clamp(0.0, 1.0) * ABSOLUTE_EXTENT) as i32;
                let yi = (y.clamp(0.0, 1.0) * ABSOLUTE_EXTENT) as i32;
                // MOUSEEVENTF_ABSOLUTE only changes how dx/dy are
                // *interpreted* for a move -- it is not itself a move event.
                // Without MOUSEEVENTF_MOVE alongside it, Win32 does nothing
                // at all: no cursor movement, and because direct-touch mode
                // taps the video by moving here and then clicking, no click
                // effect either, since the click lands wherever the cursor
                // already was. This is exactly why tapping the phone screen
                // did nothing on a real Windows machine.
                send_mouse(xi, yi, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, 0);
            }
            Message::PointerButton { button, pressed } => {
                let flags = match (button, pressed) {
                    (ProtoButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                    (ProtoButton::Left, false) => MOUSEEVENTF_LEFTUP,
                    (ProtoButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                    (ProtoButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                    (ProtoButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (ProtoButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                };
                send_mouse(0, 0, 0, flags, 0);
            }
            Message::Scroll { dx, dy } => {
                if dy != 0.0 {
                    // Vertical: positive wl_pointer axis is "down", but
                    // MOUSEEVENTF_WHEEL's positive mouseData is "up" --
                    // Windows' own sign convention is inverted from
                    // wl_pointer's, so this negates rather than passing the
                    // sign through unchanged.
                    send_mouse(0, 0, (-dy * WHEEL_DELTA) as i32, MOUSEEVENTF_WHEEL, 0);
                }
                if dx != 0.0 {
                    send_mouse(0, 0, (dx * WHEEL_DELTA) as i32, MOUSEEVENTF_HWHEEL, 0);
                }
            }
            Message::Key { evdev_code, pressed, modifiers: _ } => {
                // Unlike Linux, no bitmask needs to travel alongside the
                // keystroke: ModifierLatch already sends the modifier as a
                // real, still-held key press (see MainActivity::sendKeyTap),
                // so by the time a subsequent Key arrives here, Windows'
                // own input state already has that modifier down. The wlr
                // path additionally threads the bitmask through
                // `keyboard.modifiers()` because Wayland's virtual-keyboard
                // protocol wants depressed-modifier state stated explicitly
                // on every event; Win32's global keyboard state has no such
                // requirement.
                let Some(scancode) = keymap::scancode_for(evdev_code) else {
                    eprintln!("[input] no Windows scancode for evdev code {evdev_code}, dropped");
                    continue;
                };
                send_key(scancode, pressed);
            }
            Message::Text { utf8 } => {
                // Same documented gap as the Linux backend -- see
                // platform::linux::input's own TODO for why (per-character
                // keymap upload or a compose path, not yet built for
                // either platform).
                eprintln!("[input] TODO: text injection not yet implemented, dropped {utf8:?}");
            }
            Message::Ping { .. } | Message::Pong { .. } | Message::Hello { .. }
            | Message::HelloAck { .. } | Message::VideoConfig { .. }
            | Message::VideoFrame { .. } | Message::SetMode { .. }
            | Message::Status { .. } => {
                // Not input; the network layer handles these before forwarding here.
            }
        }
    }
    Ok(())
}

fn send_mouse(dx: i32, dy: i32, mouse_data: i32, flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, extra: usize) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                // `mouseData` is typed u32, but for MOUSEEVENTF_WHEEL/HWHEEL
                // Win32 reads it back as a *signed* delta -- scrolling up and
                // scrolling left are negative. The cast is the two's-complement
                // reinterpretation the API expects, not a lossy conversion, so
                // the parameter stays i32 and is narrowed only here at the
                // boundary.
                mouseData: mouse_data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: extra,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn send_key(scancode: keymap::Scancode, pressed: bool) {
    let flags = KEYBD_EVENT_FLAGS(keymap::key_event_flags(scancode, pressed));
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                wScan: scancode.code,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

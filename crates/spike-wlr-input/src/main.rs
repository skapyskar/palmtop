//! Phase 0 spike: prove tier-2 wlr input injection works on wlroots (Hyprland).
//!
//! This is the single biggest technical risk in the Palmtop plan: wlroots has no
//! libei, so we must inject input via the wlr virtual-pointer / virtual-keyboard
//! protocols. If this runs and the cursor visibly moves + a key is typed, the
//! tier-2 backend is proven on the target compositor.
//!
//! Run:  cargo run -p spike-wlr-input
//! Expect: the mouse cursor traces a square, left-clicks, scrolls, then the
//!         letter 'h' is typed into whatever window has keyboard focus.

use std::time::Instant;

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

/// wl_pointer button codes come from linux/input-event-codes.h.
const BTN_LEFT: u32 = 0x110;

struct State {
    start: Instant,
}

impl State {
    /// Milliseconds since spike start — the timestamps wlroots wants on events.
    fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }
}

fn main() {
    let conn = Connection::connect_to_env()
        .expect("no Wayland connection — is WAYLAND_DISPLAY set?");
    let (globals, mut queue) =
        registry_queue_init::<State>(&conn).expect("registry init failed");
    let qh = queue.handle();
    let mut state = State { start: Instant::now() };

    // A seat is required to scope the virtual devices.
    let seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=8, ())
        .expect("no wl_seat advertised");

    // --- Pointer injection (the core proof) ---
    let vpm: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).expect(
        "compositor does not advertise zwlr_virtual_pointer_manager_v1 — \
         tier-2 pointer injection unsupported here",
    );
    let pointer = vpm.create_virtual_pointer(Some(&seat), &qh, ());
    println!("[ok] bound zwlr_virtual_pointer_manager_v1 + created virtual pointer");

    // Trace a visible square with relative motion.
    let leg: &[(f64, f64)] = &[(8.0, 0.0), (0.0, 8.0), (-8.0, 0.0), (0.0, -8.0)];
    for _ in 0..30 {
        for &(dx, dy) in leg {
            pointer.motion(state.now_ms(), dx, dy);
            pointer.frame();
            conn.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
    }
    println!("[ok] injected relative pointer motion (cursor should have moved)");

    // A left click.
    use wayland_client::protocol::wl_pointer::ButtonState;
    pointer.button(state.now_ms(), BTN_LEFT, ButtonState::Pressed);
    pointer.frame();
    pointer.button(state.now_ms(), BTN_LEFT, ButtonState::Released);
    pointer.frame();
    conn.flush().unwrap();
    println!("[ok] injected a left click");

    // A scroll tick (vertical axis).
    pointer.axis(state.now_ms(), wayland_client::protocol::wl_pointer::Axis::VerticalScroll, 15.0);
    pointer.frame();
    conn.flush().unwrap();
    println!("[ok] injected a scroll tick");

    // --- Keyboard injection (best-effort; needs a keymap upload) ---
    match globals.bind::<ZwpVirtualKeyboardManagerV1, _, _>(&qh, 1..=1, ()) {
        Ok(vkm) => {
            if let Err(e) = type_letter_h(&conn, &qh, &vkm, &seat, &mut state) {
                println!("[warn] keyboard injection skipped: {e}");
            }
        }
        Err(_) => println!("[warn] no zwp_virtual_keyboard_manager_v1 — keyboard test skipped"),
    }

    // Drain any protocol errors before exit.
    queue.roundtrip(&mut state).unwrap();
    println!("[done] wlr input-injection spike completed — check that the cursor moved.");
}

/// Uploads a default US keymap and taps the 'h' key (evdev keycode 35 → +8 for XKB).
fn type_letter_h(
    conn: &Connection,
    qh: &QueueHandle<State>,
    vkm: &ZwpVirtualKeyboardManagerV1,
    seat: &wl_seat::WlSeat,
    state: &mut State,
) -> Result<(), String> {
    use std::io::Write;
    use std::os::fd::AsFd;

    let ctx = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
    let keymap = xkbcommon::xkb::Keymap::new_from_names(
        &ctx, "", "", "us", "", None, xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or("failed to build xkb keymap")?;
    let keymap_str = keymap.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1);
    let bytes = keymap_str.as_bytes();

    // wlroots reads the keymap from a shared fd; a tmpfile is the simplest carrier.
    let mut file = tempfile().map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.write_all(&[0]).map_err(|e| e.to_string())?; // NUL-terminate
    file.flush().ok();

    let vk = vkm.create_virtual_keyboard(seat, qh, ());
    vk.keymap(1 /* xkb_v1 */, file.as_fd(), (bytes.len() + 1) as u32);
    conn.flush().map_err(|e| e.to_string())?;

    // evdev 'h' == 35; wl_keyboard/xkb keycodes are evdev + 8, but the virtual
    // keyboard protocol takes the *evdev* code directly.
    const KEY_H: u32 = 35;
    vk.key(state.now_ms(), KEY_H, 1 /* pressed */);
    conn.flush().map_err(|e| e.to_string())?;
    std::thread::sleep(std::time::Duration::from_millis(20));
    vk.key(state.now_ms(), KEY_H, 0 /* released */);
    conn.flush().map_err(|e| e.to_string())?;
    println!("[ok] injected key 'h' (should appear in the focused window)");
    Ok(())
}

/// Minimal anonymous tmpfile without pulling in an extra crate.
fn tempfile() -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = std::env::temp_dir().join(format!("palmtop-keymap-{}", std::process::id()));
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

// --- Dispatch glue: this spike drives everything imperatively, so all event
// handlers are inert. ---

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

macro_rules! inert_dispatch {
    ($($ty:ty),* $(,)?) => {$(
        impl Dispatch<$ty, ()> for State {
            fn event(
                _: &mut Self,
                _: &$ty,
                _: <$ty as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {}
        }
    )*};
}

inert_dispatch!(
    wl_seat::WlSeat,
    ZwlrVirtualPointerManagerV1,
    ZwlrVirtualPointerV1,
    ZwpVirtualKeyboardManagerV1,
    ZwpVirtualKeyboardV1,
);

//! The Windows capture/input/doctor backend. See `platform::mod` for the
//! contract this implements.
//!
//! `keymap` is compiled unconditionally, on every platform: it is pure
//! table-lookup logic with no dependency on the `windows` crate, so its
//! tests (the ones that actually pin the evdev->scancode table's
//! correctness) run under plain `cargo test` here, not only on a Windows
//! target this project can cross-compile but not execute. Everything else
//! in this module -- the real `SendInput`/WGC/CNG calls -- is behind
//! `cfg(windows)`, since it depends on Windows APIs that don't exist to link
//! against anywhere else.
//!
//! `capture` and `doctor` land in later phases of the Windows host-support
//! plan; `input` is the first of the three to exist.

// `keymap` and `doctor` are both free of the `windows` crate -- pure table
// lookups/bit arithmetic and std::process calls to ffmpeg/schtasks,
// respectively -- so both are declared unconditionally for the same reason:
// their tests should run under plain `cargo test` everywhere, not only on a
// Windows target this project can cross-compile but not execute. Neither's
// public API is called from outside `platform::windows` except by `input`/
// `platform::mod`'s Windows re-export (both Windows-only), so both are
// legitimately unused on every other platform -- not a sign of dead code.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod keymap;
#[cfg_attr(not(windows), allow(dead_code))]
pub mod doctor;

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod input;

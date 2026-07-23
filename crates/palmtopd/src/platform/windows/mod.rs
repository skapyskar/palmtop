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

// `keymap`'s public API is only ever called from `input` below, which is
// Windows-only -- so on every other platform, "nothing calls this" is
// correct and expected, not a sign of dead code to clean up.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod keymap;

#[cfg(windows)]
pub mod input;

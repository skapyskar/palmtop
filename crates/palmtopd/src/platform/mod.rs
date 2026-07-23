//! The platform seam.
//!
//! Everything OS-specific -- screen capture, input injection, and the
//! preflight checks for both -- lives under `platform::linux` (and, once
//! Windows support lands, `platform::windows`). Nowhere else in the daemon
//! should need a `cfg(windows)`/`cfg(target_os = "linux")`; if it does, that
//! logic belongs in here instead, not scattered through `session.rs`/
//! `main.rs`.
//!
//! The contract each backend implements is small on purpose:
//!   - `capture::request_screencast` + `capture::run` -- get permission,
//!     then block feeding [`crate::capture::FrameSlot`] until told to stop.
//!   - `input::run` -- block draining a `Receiver<Message>`, replaying each
//!     as real input, until the channel closes.
//!   - `doctor::run` -- append this platform's preflight checks to a running
//!     diagnostic report.
//!
//! `session.rs` and `main.rs` call these through `platform::capture`/
//! `platform::input`/`platform::doctor`, never through `platform::linux`
//! directly, so neither caller needs its own `cfg` -- that's the whole
//! point of the seam.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{capture, doctor, input};

// Declared unconditionally (not `#[cfg(windows)]`) so `windows::keymap` and
// `windows::doctor` -- both free of the `windows` crate -- compile and run
// their tests on every platform, including this one.
mod windows;
#[cfg(windows)]
pub use self::windows::{capture, doctor, input};

//! The Linux capture/input/doctor backend: portal + PipeWire capture, wlr
//! tier-2 virtual-input injection. See `platform::mod` for the contract this
//! implements.

pub mod capture;
pub mod doctor;
pub mod input;

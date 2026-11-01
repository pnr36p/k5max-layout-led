//! Everything that differs between operating systems.
//!
//! Each platform provides, under the same names:
//! - `HidDevice::open(vid, pid)`, a `Link` to the keyboard's raw HID interface
//! - `list_raw_interfaces()` for the `--list` helper
//! - `run(config, config_path)`, the tray or menu bar app, until the user quits
//! - `single_instance()`, false when another copy is already running
//! - `console(command)`, so that `--list` and `--probe` can print
//! - `message_box(text)` and `timestamp()`
//!
//! `windows` is the real thing. `unsupported` lets the portable code build and be
//! tested elsewhere, until another platform gets its own module.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use unsupported::*;

/// What a HID interface reports about itself.
pub struct Interface {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub output_report_len: u16,
    pub product: String,
}

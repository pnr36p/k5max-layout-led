//! Stand-in for operating systems without a platform layer yet. The portable code
//! builds and its tests run; the app itself says it cannot start.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Interface;
use crate::config::Config;
use crate::device::{Link, PACKET_LEN};

fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "this operating system is not supported yet")
}

pub struct HidDevice(());

impl HidDevice {
    pub fn open(_vid: u16, _pid: u16) -> io::Result<Self> {
        Err(unsupported())
    }
}

impl Link for HidDevice {
    fn send(&mut self, _: &[u8; PACKET_LEN]) -> io::Result<()> {
        Err(unsupported())
    }

    fn recv(&mut self, _: Duration) -> io::Result<Option<[u8; PACKET_LEN]>> {
        Err(unsupported())
    }

    fn discard_input(&mut self) {}
}

pub fn list_raw_interfaces() -> io::Result<Vec<Interface>> {
    Err(unsupported())
}

pub fn run(_config: Config, _config_path: PathBuf) -> io::Result<()> {
    Err(unsupported())
}

pub fn single_instance() -> bool {
    true
}

pub fn console(command: impl FnOnce() -> Result<(), String>) -> i32 {
    match command() {
        Ok(()) => 0,
        Err(err) => {
            println!("{err}");
            1
        }
    }
}

pub fn message_box(text: &str) {
    eprintln!("{text}");
}

pub fn timestamp() -> String {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_secs());
    format!("unix {seconds}")
}

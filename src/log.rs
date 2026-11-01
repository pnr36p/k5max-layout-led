//! Log file next to the exe, rotated to `<name>.old` at 1 MiB. Can be switched off.

use std::fmt::Arguments;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use crate::platform::timestamp;

const MAX_LOG_SIZE: u64 = 1024 * 1024;

struct LogFile {
    file: File,
    size: u64,
}

struct State {
    path: PathBuf,
    /// `None` while logging is off.
    file: Option<LogFile>,
}

static LOG: Mutex<Option<State>> = Mutex::new(None);

fn lock() -> MutexGuard<'static, Option<State>> {
    LOG.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn open(path: &PathBuf) -> Option<LogFile> {
    let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
    let size = file.metadata().ok()?.len();
    Some(LogFile { file, size })
}

pub fn init(path: PathBuf, enabled: bool) {
    let file = if enabled { open(&path) } else { None };
    *lock() = Some(State { path, file });
}

/// Turns logging on or off at runtime. Switching off leaves a last line behind.
pub fn set_enabled(enabled: bool) {
    let mut guard = lock();
    let Some(state) = guard.as_mut() else { return };
    match (enabled, state.file.is_some()) {
        (true, false) => {
            state.file = open(&state.path);
            append(state, format_args!("log enabled"));
        }
        (false, true) => {
            append(state, format_args!("log disabled"));
            state.file = None;
        }
        _ => {}
    }
}

pub fn write(args: Arguments) {
    if let Some(state) = lock().as_mut() {
        append(state, args);
    }
}

fn append(state: &mut State, args: Arguments) {
    let Some(target) = state.file.as_mut() else { return };

    let line = format!("{} {args}\n", timestamp());
    let _ = target.file.write_all(line.as_bytes());
    target.size += line.len() as u64;

    if target.size > MAX_LOG_SIZE {
        state.file = None;
        let _ = fs::rename(&state.path, state.path.with_extension("log.old"));
        state.file = open(&state.path);
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::write(format_args!($($arg)*))
    };
}

//! Start at logon through the per-user Run key. It needs no elevation and shows up
//! in the startup list of Task Manager.

use std::mem::size_of_val;
use std::ptr::null_mut;
use std::{env, io};

use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
    RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};

use crate::app::wide;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
// Task Manager's own switch. Turning an entry off there leaves the Run value in place
// and stores a binary value here, with the low bit of the first byte set.
const APPROVED_KEY: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
const VALUE_NAME: &str = "K5MaxIndicator";

fn command() -> io::Result<String> {
    Ok(format!("\"{}\"", env::current_exe()?.display()))
}

/// A Run value pointing at another path counts as off, so that after moving the exe
/// the menu item shows the truth and one click repairs the entry.
pub fn is_enabled() -> bool {
    match (command(), read_command()) {
        (Ok(command), Some(stored)) => {
            stored.eq_ignore_ascii_case(&command) && !disabled_in_task_manager()
        }
        _ => false,
    }
}

pub fn set_enabled(enabled: bool) -> io::Result<()> {
    delete_value(APPROVED_KEY)?;
    if !enabled {
        return delete_value(RUN_KEY);
    }

    let (key, name, data) = (wide(RUN_KEY), wide(VALUE_NAME), wide(&command()?));
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    check(status)
}

fn read_command() -> Option<String> {
    let (key, name) = (wide(RUN_KEY), wide(VALUE_NAME));
    let mut buffer = [0u16; 1024];
    let mut size = size_of_val(&buffer) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..len]))
}

fn disabled_in_task_manager() -> bool {
    let (key, name) = (wide(APPROVED_KEY), wide(VALUE_NAME));
    let mut state = [0u8; 32];
    let mut size = size_of_val(&state) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            state.as_mut_ptr().cast(),
            &mut size,
        )
    };
    status == ERROR_SUCCESS && state[0] & 1 != 0
}

fn delete_value(key: &str) -> io::Result<()> {
    let (key, name) = (wide(key), wide(VALUE_NAME));
    let status =
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    match status {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(()),
        status => check(status),
    }
}

fn check(status: u32) -> io::Result<()> {
    match status {
        ERROR_SUCCESS => Ok(()),
        status => Err(io::Error::from_raw_os_error(status as i32)),
    }
}

//! Windows: Win32 raw HID, a hidden window with a tray icon, registry autostart.

mod app;
mod autostart;
mod hid;

use std::mem::zeroed;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

pub use app::run;
pub use hid::{list_raw_interfaces, HidDevice};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

pub fn message_box(text: &str) {
    let (text, title) = (wide(text), wide("K5 Max Indicator"));
    unsafe { MessageBoxW(null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

/// The HID interface is shared, so nothing else would stop two instances from
/// fighting over the LEDs. The mutex handle stays open for the process lifetime.
pub fn single_instance() -> bool {
    let name = wide("Local\\K5MaxLayoutIndicator");
    let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    mutex.is_null() || unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
}

/// Runs a command line helper. The exe has no console of its own, so it borrows the parent's.
pub fn console(command: impl FnOnce() -> Result<(), String>) -> i32 {
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
        // Without a redirect a GUI process has no stdout, even after attaching.
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            let name = wide("CONOUT$");
            let console = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if console != INVALID_HANDLE_VALUE {
                SetStdHandle(STD_OUTPUT_HANDLE, console);
            }
        }
    }
    match command() {
        Ok(()) => 0,
        Err(err) => {
            println!("{err}");
            1
        }
    }
}

/// Local time as `2026-11-01 14:50:49`.
pub fn timestamp() -> String {
    let mut time = unsafe { zeroed() };
    unsafe { GetLocalTime(&mut time) };
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

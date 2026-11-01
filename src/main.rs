#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod autostart;
mod config;
mod device;
mod hid;
mod log;

use std::path::{Path, PathBuf};
use std::{env, fs, io, process};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::CreateMutexW;

use config::{Config, DEFAULT_CONFIG};

fn main() {
    let args: Vec<String> = env::args().collect();
    let dir = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let config_path = dir.join("config.toml");

    if args.iter().any(|arg| arg == "--list") {
        process::exit(console(list));
    }
    if args.iter().any(|arg| arg == "--probe") {
        process::exit(console(|| probe(&config_path)));
    }

    // Settled before anything is logged. A config that cannot be read keeps logging on,
    // since that is when the log is needed.
    let log_enabled = fs::read_to_string(&config_path)
        .ok()
        .and_then(|text| Config::parse(&text).ok())
        .map_or(true, |config| config.log);
    log::init(dir.join("k5max-indicator.log"), log_enabled);
    std::panic::set_hook(Box::new(|info| log!("panic: {info}")));

    // Handle stays open for the process lifetime. The HID interface is shared,
    // so nothing else would stop two instances from fighting over the LEDs.
    let name = app::wide("Local\\K5MaxLayoutIndicator");
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        log!("already running, exiting");
        return;
    }

    log!("started, version {}", env!("CARGO_PKG_VERSION"));
    if let Err(err) = start(config_path) {
        log!("fatal: {err}");
        app::message_box(&err);
        process::exit(1);
    }
    log!("stopped");
}

fn start(config_path: PathBuf) -> Result<(), String> {
    if !config_path.exists() {
        fs::write(&config_path, DEFAULT_CONFIG)
            .map_err(|err| format!("{}: {err}", config_path.display()))?;
    }
    let config = read_config(&config_path)?;
    let device = &config.device;
    log!("device {:04X}:{:04X}, {} LEDs", device.vid, device.pid, device.keys.len());
    app::run(config, config_path).map_err(|err: io::Error| err.to_string())
}

fn read_config(path: &Path) -> Result<Config, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    Config::parse(&text).map_err(|err| format!("{}:\n{err}", path.display()))
}

/// Runs a command line helper. The exe has no console of its own, so it borrows the parent's.
fn console(command: impl FnOnce() -> Result<(), String>) -> i32 {
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
        // Without a redirect a GUI process has no stdout, even after attaching.
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            let name = app::wide("CONOUT$");
            let console = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
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

fn list() -> Result<(), String> {
    let interfaces = hid::list_raw_interfaces().map_err(|err| err.to_string())?;
    if interfaces.is_empty() {
        return Err("no raw HID interfaces (usage page FF60, usage 61) found".into());
    }
    for interface in interfaces {
        println!(
            "vid = 0x{:04X}  pid = 0x{:04X}  report {} bytes  {}",
            interface.vid, interface.pid, interface.output_report_len, interface.product
        );
    }
    Ok(())
}

/// Asks the keyboard named in config.toml for its protocol details.
fn probe(config_path: &Path) -> Result<(), String> {
    let config = if config_path.exists() {
        read_config(config_path)?
    } else {
        Config::parse(DEFAULT_CONFIG)?
    };
    let device = &config.device;
    println!("device             {:04X}:{:04X}", device.vid, device.pid);

    let mut link = hid::HidDevice::open(device.vid, device.pid).map_err(|err| {
        format!("cannot open it: {err}\nrun with --list to see the raw HID interfaces present")
    })?;
    device::probe(&mut link, device.keys.len()).map_err(|err| err.to_string())
}

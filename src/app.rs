//! Hidden window: layout polling, device and power notifications, tray icon.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use std::{fs, io};

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::HumanInterfaceDevice::HidD_GetHidGuid;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::autostart;
use crate::config::Config;
use crate::device::{self, Cmd, Engine};
use crate::hid::HidDevice;
use crate::log;

const WM_STATUS: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;

const LAYOUT_TIMER: usize = 1;
const LAYOUT_POLL_MS: u32 = 100;

const MENU_RELOAD: u32 = 1;
const MENU_EXIT: u32 = 2;
const MENU_AUTOSTART: u32 = 3;

const COLOR_CONNECTED: [u8; 3] = [0, 200, 100];
const COLOR_DISCONNECTED: [u8; 3] = [130, 130, 130];

struct App {
    hwnd: HWND,
    commands: Sender<Cmd>,
    worker: Option<JoinHandle<()>>,
    config: Config,
    config_path: PathBuf,
    lang: Option<u16>,
    layout: Option<usize>,
    connected: bool,
    icons: [HICON; 2],
    taskbar_created: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state. The borrow must not span anything that can dispatch
/// messages, because that re-enters the window procedure. Besides menus and message
/// boxes this includes `Shell_NotifyIconW`, a synchronous call into explorer.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with_borrow_mut(|app| app.as_mut().map(f))
}

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

pub fn message_box(text: &str) {
    let (text, title) = (wide(text), wide("K5 Max Indicator"));
    unsafe { MessageBoxW(null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

pub fn run(config: Config, config_path: PathBuf) -> io::Result<()> {
    let hwnd = unsafe { create_window()? };
    register_device_notifications(hwnd)?;

    let (commands, receiver) = mpsc::channel();
    let hwnd_value = hwnd as usize;
    let (vid, pid) = (config.device.vid, config.device.pid);
    let engine = Engine::new(
        move || HidDevice::open(vid, pid),
        move |connected| unsafe {
            PostMessageW(hwnd_value as HWND, WM_STATUS, connected as WPARAM, 0);
        },
        config.heartbeat,
        config.device.keys.len(),
        Instant::now(),
    );
    let worker = thread::spawn(move || device::run(engine, receiver));

    let app = App {
        hwnd,
        commands,
        worker: Some(worker),
        config,
        config_path,
        lang: None,
        layout: None,
        connected: false,
        icons: [make_icon(COLOR_DISCONNECTED), make_icon(COLOR_CONNECTED)],
        taskbar_created: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
    };
    APP.with_borrow_mut(|slot| *slot = Some(app));
    update_tray(NIM_ADD);
    poll_layout();

    unsafe {
        SetTimer(hwnd, LAYOUT_TIMER, LAYOUT_POLL_MS, None);
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    update_tray(NIM_DELETE);
    if let Some(mut app) = APP.take() {
        let restore = app.config.restore_on_exit;
        let _ = app.commands.send(Cmd::Shutdown { restore });
        if let Some(worker) = app.worker.take() {
            let _ = worker.join();
        }
    }
    Ok(())
}

unsafe fn create_window() -> io::Result<HWND> {
    let instance = GetModuleHandleW(null());
    let class_name = wide("K5MaxIndicatorWindow");
    let mut class: WNDCLASSW = zeroed();
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = instance;
    class.lpszClassName = class_name.as_ptr();
    RegisterClassW(&class);

    // A real top-level window, never shown: message-only windows get no broadcasts,
    // and WM_POWERBROADCAST and TaskbarCreated are broadcasts.
    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        class_name.as_ptr(),
        WS_OVERLAPPED,
        0,
        0,
        0,
        0,
        null_mut(),
        null_mut(),
        instance,
        null(),
    );
    if hwnd.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(hwnd)
}

fn register_device_notifications(hwnd: HWND) -> io::Result<()> {
    unsafe {
        let mut guid: GUID = zeroed();
        HidD_GetHidGuid(&mut guid);
        let mut filter: DEV_BROADCAST_DEVICEINTERFACE_W = zeroed();
        filter.dbcc_size = size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32;
        filter.dbcc_devicetype = DBT_DEVTYP_DEVICEINTERFACE;
        filter.dbcc_classguid = guid;
        let handle = RegisterDeviceNotificationW(
            hwnd,
            (&raw const filter).cast(),
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TIMER => {
            poll_layout();
            0
        }
        WM_DEVICECHANGE => {
            if matches!(wparam as u32, DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE) {
                with_app(|app| app.send(Cmd::DevicesChanged));
            }
            1
        }
        WM_POWERBROADCAST => {
            if matches!(wparam as u32, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND) {
                log!("system resumed");
                with_app(|app| app.send(Cmd::Resumed));
            }
            1
        }
        WM_STATUS => {
            with_app(|app| app.connected = wparam != 0);
            update_tray(NIM_MODIFY);
            0
        }
        WM_TRAY => {
            if matches!(lparam as u32, WM_LBUTTONUP | WM_RBUTTONUP) {
                show_menu(hwnd);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => {
            let restarted = with_app(|app| message == app.taskbar_created);
            if restarted == Some(true) {
                update_tray(NIM_ADD);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let Some(status) = with_app(|app| app.status_text()) else { return };
    let autostart_on = autostart::is_enabled();
    let autostart_flags = if autostart_on { MF_STRING | MF_CHECKED } else { MF_STRING };
    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide(&status).as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, autostart_flags, MENU_AUTOSTART as usize, wide("Start with Windows").as_ptr());
    AppendMenuW(menu, MF_STRING, MENU_RELOAD as usize, wide("Reload config").as_ptr());
    AppendMenuW(menu, MF_STRING, MENU_EXIT as usize, wide("Exit").as_ptr());

    // Without the foreground switch the menu would not close on an outside click.
    let mut cursor = POINT { x: 0, y: 0 };
    GetCursorPos(&mut cursor);
    SetForegroundWindow(hwnd);
    let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON;
    let choice = TrackPopupMenu(menu, flags, cursor.x, cursor.y, 0, hwnd, null());
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);

    match choice as u32 {
        MENU_AUTOSTART => match autostart::set_enabled(!autostart_on) {
            Ok(()) => log!("autostart {}", if autostart_on { "disabled" } else { "enabled" }),
            Err(err) => {
                log!("autostart change failed: {err}");
                message_box(&format!("Autostart was not changed:\n{err}"));
            }
        },
        MENU_RELOAD => {
            match with_app(App::reload_config) {
                Some(Ok(())) => update_tray(NIM_MODIFY),
                Some(Err(err)) => {
                    log!("config reload failed: {err}");
                    message_box(&format!("config.toml was not applied:\n{err}"));
                }
                None => {}
            }
        }
        MENU_EXIT => {
            DestroyWindow(hwnd);
        }
        _ => {}
    }
}

fn poll_layout() {
    if with_app(App::poll_layout) == Some(true) {
        update_tray(NIM_MODIFY);
    }
}

fn update_tray(action: u32) {
    if let Some(data) = with_app(|app| app.tray_data()) {
        unsafe { Shell_NotifyIconW(action, &data) };
    }
}

impl App {
    fn send(&self, cmd: Cmd) {
        let _ = self.commands.send(cmd);
    }

    /// Returns true when the displayed layout changed.
    fn poll_layout(&mut self) -> bool {
        let Some(lang) = foreground_lang() else { return false };
        if self.lang == Some(lang) {
            return false;
        }
        self.lang = Some(lang);
        log!("input language {lang:04X}");

        let Some(index) = self.config.layout_for(lang) else {
            log!("no layout configured for it, keeping the current colors");
            return false;
        };
        if self.layout == Some(index) {
            return false;
        }
        self.layout = Some(index);
        let layout = &self.config.layouts[index];
        log!("layout {}", layout.name);
        self.send(Cmd::Frame(layout.frame.clone()));
        true
    }

    fn reload_config(&mut self) -> Result<(), String> {
        let text = fs::read_to_string(&self.config_path).map_err(|err| err.to_string())?;
        let config = Config::parse(&text)?;
        // The link was opened for this device and frames are sized for this key list.
        if config.device != self.config.device {
            return Err("[device] changed, restart the app to apply it".into());
        }
        self.config = config;
        log::set_enabled(self.config.log);
        log!("config reloaded");
        self.send(Cmd::Heartbeat(self.config.heartbeat));
        self.lang = None;
        self.layout = None;
        self.poll_layout();
        Ok(())
    }

    fn status_text(&self) -> String {
        let link = if self.connected { "connected" } else { "not connected" };
        match self.layout {
            Some(index) => format!("K5 Max: {link}, {}", self.config.layouts[index].name),
            None => format!("K5 Max: {link}"),
        }
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        unsafe {
            let mut data: NOTIFYICONDATAW = zeroed();
            data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = self.hwnd;
            data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            data.uCallbackMessage = WM_TRAY;
            data.hIcon = self.icons[self.connected as usize];
            let tip = wide(&self.status_text());
            let len = tip.len().min(data.szTip.len() - 1);
            data.szTip[..len].copy_from_slice(&tip[..len]);
            data
        }
    }
}

/// Language of the keyboard layout in the foreground window. `None` when there is
/// nothing meaningful to read: lock screen, UAC, or this app's own tray menu.
fn foreground_lang() -> Option<u16> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let thread = GetWindowThreadProcessId(hwnd, null_mut());
        if thread == 0 || thread == GetCurrentThreadId() {
            return None;
        }
        match GetKeyboardLayout(thread) as usize as u16 {
            0 => None,
            lang => Some(lang),
        }
    }
}

/// Draws a rounded square with the letter K.
fn make_icon(color: [u8; 3]) -> HICON {
    const SIZE: usize = 32;
    const SCALE: usize = 4;
    const GLYPH: [u8; 7] = [
        0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
    ];
    let is_glyph = |x: usize, y: usize| {
        let (x, y) = (x.wrapping_sub(6) / SCALE, y.wrapping_sub(2) / SCALE);
        x < 5 && y < GLYPH.len() && GLYPH[y] & (0b10000 >> x) != 0
    };
    let is_corner = |x: usize, y: usize| {
        (x < 2 || x >= SIZE - 2) && (y < 2 || y >= SIZE - 2)
    };

    let mut pixels = Vec::with_capacity(SIZE * SIZE * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            pixels.extend_from_slice(&if is_corner(x, y) {
                [0, 0, 0, 0]
            } else if is_glyph(x, y) {
                [255, 255, 255, 255]
            } else {
                [color[2], color[1], color[0], 255]
            });
        }
    }

    let mask = [0u8; SIZE * SIZE / 8];
    unsafe {
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: CreateBitmap(SIZE as i32, SIZE as i32, 1, 1, mask.as_ptr().cast()),
            hbmColor: CreateBitmap(SIZE as i32, SIZE as i32, 1, 32, pixels.as_ptr().cast()),
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(info.hbmMask);
        DeleteObject(info.hbmColor);
        icon
    }
}

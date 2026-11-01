//! Raw HID access through the Win32 HID API.

use std::io;
use std::mem::{size_of, size_of_val, zeroed};
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HidD_FlushQueue, HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid,
    HidD_GetPreparsedData, HidD_GetProductString, HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS,
    HIDP_STATUS_SUCCESS,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, FALSE, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE, TRUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

use crate::device::{Link, PACKET_LEN};

const RAW_USAGE_PAGE: u16 = 0xFF60;
const RAW_USAGE: u16 = 0x61;

/// Report length on the wire: report ID 0 followed by the 32 byte packet.
const REPORT_LEN: usize = PACKET_LEN + 1;
const IO_TIMEOUT_MS: u32 = 1000;

pub struct HidDevice {
    handle: HANDLE,
    event: HANDLE,
}

// Handles are owned exclusively and only used from one thread at a time.
unsafe impl Send for HidDevice {}

impl HidDevice {
    pub fn open(vid: u16, pid: u16) -> io::Result<Self> {
        let path = find_raw_interface(vid, pid)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "raw HID interface not present")
        })?;
        unsafe {
            let handle = CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let event = CreateEventW(null(), TRUE, FALSE, null());
            if event.is_null() {
                let err = io::Error::last_os_error();
                CloseHandle(handle);
                return Err(err);
            }
            Ok(Self { handle, event })
        }
    }

    /// Waits for an overlapped operation. The kernel writes through `overlapped`
    /// and the buffer, so a timed out request is cancelled and waited for before
    /// the caller can release them.
    unsafe fn finish(&self, overlapped: &OVERLAPPED, timeout_ms: u32) -> io::Result<Option<u32>> {
        let mut transferred = 0;
        match WaitForSingleObject(self.event, timeout_ms) {
            WAIT_OBJECT_0 => {
                if GetOverlappedResult(self.handle, overlapped, &mut transferred, FALSE) == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(transferred))
            }
            WAIT_TIMEOUT => {
                CancelIoEx(self.handle, overlapped);
                GetOverlappedResult(self.handle, overlapped, &mut transferred, TRUE);
                Ok(None)
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Link for HidDevice {
    fn send(&mut self, packet: &[u8; PACKET_LEN]) -> io::Result<()> {
        let mut report = [0u8; REPORT_LEN];
        report[1..].copy_from_slice(packet);
        unsafe {
            let mut overlapped: OVERLAPPED = zeroed();
            overlapped.hEvent = self.event;
            ResetEvent(self.event);
            let started = WriteFile(
                self.handle,
                report.as_ptr(),
                REPORT_LEN as u32,
                null_mut(),
                &mut overlapped,
            );
            if started == 0 && GetLastError() != ERROR_IO_PENDING {
                return Err(io::Error::last_os_error());
            }
            match self.finish(&overlapped, IO_TIMEOUT_MS)? {
                Some(_) => Ok(()),
                None => Err(io::ErrorKind::TimedOut.into()),
            }
        }
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<[u8; PACKET_LEN]>> {
        let mut report = [0u8; REPORT_LEN];
        unsafe {
            let mut overlapped: OVERLAPPED = zeroed();
            overlapped.hEvent = self.event;
            ResetEvent(self.event);
            let started = ReadFile(
                self.handle,
                report.as_mut_ptr(),
                REPORT_LEN as u32,
                null_mut(),
                &mut overlapped,
            );
            if started == 0 && GetLastError() != ERROR_IO_PENDING {
                return Err(io::Error::last_os_error());
            }
            Ok(self.finish(&overlapped, timeout.as_millis() as u32)?.map(|_| {
                let mut packet = [0u8; PACKET_LEN];
                packet.copy_from_slice(&report[1..]);
                packet
            }))
        }
    }

    fn discard_input(&mut self) {
        unsafe {
            HidD_FlushQueue(self.handle);
        }
    }
}

impl Drop for HidDevice {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.event);
            CloseHandle(self.handle);
        }
    }
}

/// What a HID interface reports about itself.
pub struct Interface {
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub output_report_len: u16,
    pub product: String,
}

impl Interface {
    fn is_qmk_raw(&self) -> bool {
        self.usage_page == RAW_USAGE_PAGE && self.usage == RAW_USAGE
    }
}

/// Raw HID interfaces of all connected devices, for finding the USB ID of a keyboard.
pub fn list_raw_interfaces() -> io::Result<Vec<Interface>> {
    let mut found = Vec::new();
    for_each_interface(|path| {
        found.extend(describe(path).filter(Interface::is_qmk_raw));
        None::<()>
    })?;
    Ok(found)
}

/// Returns the device path of the keyboard's raw HID interface, if it is present.
fn find_raw_interface(vid: u16, pid: u16) -> io::Result<Option<Vec<u16>>> {
    for_each_interface(|path| {
        let interface = describe(path)?;
        let matched = interface.vid == vid
            && interface.pid == pid
            && interface.is_qmk_raw()
            && interface.output_report_len as usize == REPORT_LEN;
        matched.then(|| path.to_vec())
    })
}

/// Calls `visit` with the path of every present HID interface until it returns `Some`.
fn for_each_interface<T>(mut visit: impl FnMut(&[u16]) -> Option<T>) -> io::Result<Option<T>> {
    unsafe {
        let mut guid: GUID = zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if set as isize == INVALID_HANDLE_VALUE as isize {
            return Err(io::Error::last_os_error());
        }

        let mut found = None;
        let mut index = 0;
        loop {
            let mut data: SP_DEVICE_INTERFACE_DATA = zeroed();
            data.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null(), &guid, index, &mut data) == 0 {
                break;
            }
            index += 1;

            let mut needed = 0;
            SetupDiGetDeviceInterfaceDetailW(set, &data, null_mut(), 0, &mut needed, null_mut());
            if needed == 0 {
                continue;
            }
            // u64 alignment satisfies SP_DEVICE_INTERFACE_DETAIL_DATA_W.
            let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
            let detail = buffer.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &data, detail, needed, null_mut(), null_mut())
                == 0
            {
                continue;
            }

            let path = std::ptr::addr_of!((*detail).DevicePath).cast::<u16>();
            let len = (0..).take_while(|&i| *path.add(i) != 0).count();
            let path = std::slice::from_raw_parts(path, len + 1);
            if let Some(result) = visit(path) {
                found = Some(result);
                break;
            }
        }
        SetupDiDestroyDeviceInfoList(set);
        Ok(found)
    }
}

fn describe(path: &[u16]) -> Option<Interface> {
    // Access 0 is enough for attributes and also works on keyboards and mice,
    // which the system holds exclusively.
    unsafe {
        let handle = CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut attributes: HIDD_ATTRIBUTES = zeroed();
        attributes.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
        let mut interface = None;
        let mut preparsed = 0;
        if HidD_GetAttributes(handle, &mut attributes) && HidD_GetPreparsedData(handle, &mut preparsed)
        {
            let mut caps: HIDP_CAPS = zeroed();
            if HidP_GetCaps(preparsed, &mut caps) == HIDP_STATUS_SUCCESS {
                interface = Some(Interface {
                    vid: attributes.VendorID,
                    pid: attributes.ProductID,
                    usage_page: caps.UsagePage,
                    usage: caps.Usage,
                    output_report_len: caps.OutputReportByteLength,
                    product: product_name(handle),
                });
            }
            HidD_FreePreparsedData(preparsed);
        }
        CloseHandle(handle);
        interface
    }
}

unsafe fn product_name(handle: HANDLE) -> String {
    let mut buffer = [0u16; 128];
    if !HidD_GetProductString(handle, buffer.as_mut_ptr().cast(), size_of_val(&buffer) as u32) {
        return String::new();
    }
    let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

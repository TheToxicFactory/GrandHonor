use super::wmi::{change_wmi_brightness, try_step_acpi_brightness};
use crate::settings;
use grandhonor_core::locale::t;
use grandhonor_core::protocol::{TouchpadEdgeGesture, TouchpadHaptics};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetAttributes,
    HidD_GetHidGuid, HidD_GetPreparsedData, HidP_GetCaps, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows_core::HSTRING;

pub const REPORT_ID: u8 = 0x0E;
pub const HAPTICS_COMMAND: u8 = 0x02;
pub const GESTURE_EVENT_COMMAND: u8 = 0x03;
const DEFAULT_REPORT_LEN: usize = 9;
const VENDOR_USAGE_PAGE: u16 = 0xFF00;
const VENDOR_USAGE: u16 = 0x0001;
const DIRECTION_UP: u8 = 0x01;
const DIRECTION_DOWN: u8 = 0x02;
const REPEAT_SUPPRESS_MS: u128 = 150;
const BRIGHTNESS_STEP_PERCENT: i32 = 3;

pub const SUPPORTED_DEVICES: [(u16, u16); 2] = [(0x27C6, 0x0F9A), (0x35CC, 0x0104)];

#[derive(Clone, Debug)]
pub struct HidDeviceInfo {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_report_len: u16,
    pub output_report_len: u16,
}

struct OwnedHandle(HANDLE);
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct DevInfoSet(HDEVINFO);
impl Drop for DevInfoSet {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }
}

pub fn enumerate_hid_devices() -> Result<Vec<HidDeviceInfo>, String> {
    let hid_guid = unsafe { HidD_GetHidGuid() };
    let raw_set = unsafe {
        SetupDiGetClassDevsW(
            Some(&hid_guid),
            None,
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    }
    .map_err(|e| e.to_string())?;
    let set = DevInfoSet(raw_set);

    let mut devices = Vec::new();
    let mut index = 0u32;
    loop {
        let mut iface = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInterfaces(set.0, None, &hid_guid, index, &mut iface) }.is_err()
        {
            break;
        }
        index += 1;

        let Some(path) = get_device_path(set.0, &mut iface) else {
            continue;
        };
        let wide = HSTRING::from(path.as_str());
        let handle = match unsafe {
            CreateFileW(
                &wide,
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                Default::default(),
                None,
            )
        } {
            Ok(h) if h != INVALID_HANDLE_VALUE => OwnedHandle(h),
            _ => continue,
        };

        let mut attrs = HIDD_ATTRIBUTES {
            Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32,
            ..Default::default()
        };
        if !unsafe { HidD_GetAttributes(handle.0, &mut attrs) } {
            continue;
        }

        let (mut usage_page, mut usage, mut input_len, mut output_len) = (0u16, 0u16, 64u16, 0u16);
        let mut preparsed = PHIDP_PREPARSED_DATA::default();
        if unsafe { HidD_GetPreparsedData(handle.0, &mut preparsed) } {
            let mut caps = HIDP_CAPS::default();
            if unsafe { HidP_GetCaps(preparsed, &mut caps) } == HIDP_STATUS_SUCCESS {
                usage_page = caps.UsagePage;
                usage = caps.Usage;
                input_len = caps.InputReportByteLength;
                output_len = caps.OutputReportByteLength;
            }
            unsafe {
                let _ = HidD_FreePreparsedData(preparsed);
            }
        }

        devices.push(HidDeviceInfo {
            path,
            vendor_id: attrs.VendorID,
            product_id: attrs.ProductID,
            usage_page,
            usage,
            input_report_len: input_len,
            output_report_len: output_len,
        });
    }
    Ok(devices)
}

fn get_device_path(set: HDEVINFO, iface: &mut SP_DEVICE_INTERFACE_DATA) -> Option<String> {
    let mut required = 0u32;
    unsafe {
        let _ = SetupDiGetDeviceInterfaceDetailW(set, iface, None, 0, Some(&mut required), None);
    }
    if required < std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 {
        return None;
    }
    let mut buf = vec![0u8; required as usize];
    let detail_ptr = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    unsafe {
        (*detail_ptr).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        SetupDiGetDeviceInterfaceDetailW(set, iface, Some(detail_ptr), required, None, None).ok()?;
        let char_offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
        let wchar_ptr = buf.as_ptr().add(char_offset) as *const u16;
        let max_wchars = (required as usize - char_offset) / 2;
        let slice = std::slice::from_raw_parts(wchar_ptr, max_wchars);
        let len = slice.iter().position(|&c| c == 0).unwrap_or(max_wchars);
        Some(String::from_utf16_lossy(&slice[..len]))
    }
}

static DEVICE_CACHE: OnceLock<Mutex<Option<Option<HidDeviceInfo>>>> = OnceLock::new();

fn device_cache() -> &'static Mutex<Option<Option<HidDeviceInfo>>> {
    DEVICE_CACHE.get_or_init(|| Mutex::new(None))
}

pub fn invalidate_cache() {
    if let Ok(mut guard) = device_cache().lock() {
        *guard = None;
    }
}

pub fn is_supported() -> bool {
    find_vendor_device().is_some()
}

pub fn is_supported_device_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    SUPPORTED_DEVICES
        .iter()
        .any(|(vid, _)| lower.contains(&format!("vid_{vid:04x}")))
}

fn find_vendor_device() -> Option<HidDeviceInfo> {
    let Ok(mut guard) = device_cache().lock() else {
        return None;
    };
    if let Some(cached) = guard.as_ref() {
        return cached.clone();
    }
    let found = match enumerate_hid_devices() {
        Ok(list) => list.into_iter().find(|d| {
            SUPPORTED_DEVICES.contains(&(d.vendor_id, d.product_id))
                && d.usage_page == VENDOR_USAGE_PAGE
                && d.usage == VENDOR_USAGE
                && d.output_report_len >= 3
        }),
        Err(err) => {
            crate::log::error(&format!(
                "Failed to enumerate HID devices for the Honor touchpad: {err}"
            ));
            None
        }
    };
    *guard = Some(found.clone());
    found
}

pub fn send_vendor_command(command: u8, value: u8) -> Result<(), String> {
    for attempt in 0..2 {
        let device = find_vendor_device().ok_or_else(|| {
            t(
                "Совместимый тачпад Honor не найден.",
                "No compatible Honor touchpad was found.",
                "未找到兼容的荣耀触控板。",
            )
            .to_string()
        })?;
        match try_send(&device, command, value) {
            Ok(()) => return Ok(()),
            Err(err) => {
                invalidate_cache();
                if attempt > 0 {
                    return Err(err);
                }
            }
        }
    }
    unreachable!()
}

fn try_send(device: &HidDeviceInfo, command: u8, value: u8) -> Result<(), String> {
    let wide = HSTRING::from(device.path.as_str());
    let handle = unsafe {
        CreateFileW(
            &wide,
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
    }
    .map_err(|_| {
        t(
            "Не удалось открыть тачпад для записи.",
            "Could not open the touchpad for writing.",
            "无法打开触控板进行写入。",
        )
        .to_string()
    })?;
    let owned = OwnedHandle(handle);

    let len = if device.output_report_len > 0 {
        device.output_report_len as usize
    } else {
        DEFAULT_REPORT_LEN
    };
    let mut report = vec![0u8; len];
    report[0] = REPORT_ID;
    report[1] = command;
    report[2] = value;

    let mut written = 0u32;
    let res = unsafe { WriteFile(owned.0, Some(&report), Some(&mut written), None) };
    if res.is_err() || written as usize != report.len() {
        return Err(t(
            "Не удалось записать настройку в тачпад.",
            "Could not write the setting to the touchpad.",
            "无法将设置写入触控板。",
        )
        .to_string());
    }
    Ok(())
}

pub fn set_haptics(level: TouchpadHaptics) -> Result<(), String> {
    send_vendor_command(HAPTICS_COMMAND, level as u8)
}

pub fn apply_haptics(level: TouchpadHaptics) -> Result<(), String> {
    set_haptics(level)?;
    settings::set_touchpad_haptics(Some(level));
    Ok(())
}

pub fn reapply_haptics() {
    let Some(level) = settings::touchpad_haptics() else {
        return;
    };
    if let Err(err) = set_haptics(level) {
        crate::log::error(&format!("Failed to reapply touchpad haptics level: {err}"));
    }
}

pub fn is_gesture_enabled(gesture: TouchpadEdgeGesture) -> bool {
    settings::touchpad_edge_gesture(gesture).unwrap_or(true)
}

pub fn apply_gesture(gesture: TouchpadEdgeGesture, enabled: bool) -> Result<(), String> {
    send_vendor_command(gesture as u8, if enabled { 1 } else { 0 })?;
    settings::set_touchpad_edge_gesture(gesture, Some(enabled));
    Ok(())
}

pub fn reapply_gestures() {
    for gesture in [TouchpadEdgeGesture::Brightness, TouchpadEdgeGesture::Volume] {
        let Some(enabled) = settings::touchpad_edge_gesture(gesture) else {
            continue;
        };
        if let Err(err) = send_vendor_command(gesture as u8, if enabled { 1 } else { 0 }) {
            crate::log::error(&format!(
                "Failed to reapply touchpad edge gesture {:?}: {err}",
                gesture
            ));
        }
    }
}

struct ReaderGroup {
    stop_event: OwnedHandle,
    stopped: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl ReaderGroup {
    fn stop(mut self) {
        self.stopped.store(true, Ordering::Release);
        unsafe {
            let _ = SetEvent(self.stop_event.0);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

pub struct TouchpadBrightnessService {
    lifecycle: Mutex<Option<ReaderGroup>>,
    disposed: Arc<AtomicBool>,
    action_lock: Arc<Mutex<Option<(u8, u8, Instant)>>>,
}

impl TouchpadBrightnessService {
    pub fn new() -> Self {
        Self {
            lifecycle: Mutex::new(None),
            disposed: Arc::new(AtomicBool::new(false)),
            action_lock: Arc::new(Mutex::new(None)),
        }
    }

    pub fn start(&self) {
        let Ok(mut guard) = self.lifecycle.lock() else {
            return;
        };
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        if let Some(prev) = guard.take() {
            prev.stop();
        }
        match self.spawn_readers() {
            Ok(Some(group)) => *guard = Some(group),
            Ok(None) => crate::log::error(t(
                "Совместимый тачпад Honor не найден.",
                "No compatible Honor touchpad was found.",
                "未找到兼容的荣耀触控板。",
            )),
            Err(err) => crate::log::error(&format!(
                "{}{err}",
                t(
                    "Ошибка запуска тачпада: ",
                    "Touchpad startup failed: ",
                    "触控板启动失败："
                )
            )),
        }
    }

    pub fn restart(&self) {
        let Ok(mut guard) = self.lifecycle.lock() else {
            return;
        };
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        if let Some(prev) = guard.take() {
            prev.stop();
        }
        if let Ok(Some(group)) = self.spawn_readers() {
            *guard = Some(group);
            crate::log::info("Touchpad readers restarted after device change");
        }
    }

    fn spawn_readers(&self) -> Result<Option<ReaderGroup>, String> {
        let candidates: Vec<HidDeviceInfo> = enumerate_hid_devices()?
            .into_iter()
            .filter(|d| {
                SUPPORTED_DEVICES.contains(&(d.vendor_id, d.product_id))
                    && d.usage_page >= VENDOR_USAGE_PAGE
            })
            .collect();
        if candidates.is_empty() {
            return Ok(None);
        }

        let stop_raw = unsafe { CreateEventW(None, true, false, None) }.map_err(|e| e.to_string())?;
        let stop_handle = Arc::new(OwnedHandle(stop_raw));
        let stopped = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::with_capacity(candidates.len());

        for dev in candidates {
            let stop_flag = stopped.clone();
            let stop_ev = stop_handle.clone();
            let action = self.action_lock.clone();
            if let Ok(h) = thread::Builder::new()
                .name("touchpad-hid".into())
                .spawn(move || {
                    read_device_loop(dev, stop_flag, stop_ev, action);
                })
            {
                threads.push(h);
            }
        }

        let owned_stop = match Arc::try_unwrap(stop_handle) {
            Ok(h) => h,
            Err(shared) => {
                // Keep another handle duplicate? Simpler: create ReaderGroup storing Arc or duplicate:
                // wait - we can't unwrap while threads hold clones, so let's duplicate or store HANDLE!
                // Actually let's store Arc<OwnedHandle> in ReaderGroup — cleanest!
                return Ok(Some(ReaderGroup {
                    stop_event: OwnedHandle(unsafe {
                        let mut dup = HANDLE::default();
                        let cur = windows::Win32::System::Threading::GetCurrentProcess();
                        let _ = windows::Win32::Foundation::DuplicateHandle(
                            cur,
                            shared.0,
                            cur,
                            &mut dup,
                            0,
                            false,
                            windows::Win32::Foundation::DUPLICATE_SAME_ACCESS,
                        );
                        dup
                    }),
                    stopped,
                    threads,
                }));
            }
        };
        Ok(Some(ReaderGroup {
            stop_event: owned_stop,
            stopped,
            threads,
        }))
    }
}

impl Drop for TouchpadBrightnessService {
    fn drop(&mut self) {
        self.disposed.store(true, Ordering::Release);
        if let Ok(mut guard) = self.lifecycle.lock()
            && let Some(group) = guard.take()
        {
            group.stop();
        }
    }
}

fn read_device_loop(
    device: HidDeviceInfo,
    stopped: Arc<AtomicBool>,
    stop_event: Arc<OwnedHandle>,
    action_lock: Arc<Mutex<Option<(u8, u8, Instant)>>>,
) {
    let wide = HSTRING::from(device.path.as_str());
    let Ok(raw_handle) = (unsafe {
        CreateFileW(
            &wide,
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            None,
        )
    }) else {
        return;
    };
    if raw_handle == INVALID_HANDLE_VALUE {
        return;
    }
    let handle = OwnedHandle(raw_handle);

    let Ok(read_ev_raw) = (unsafe { CreateEventW(None, true, false, None) }) else {
        return;
    };
    let read_event = OwnedHandle(read_ev_raw);

    let buf_len = (device.input_report_len as usize).max(64);
    let mut buffer = vec![0u8; buf_len];

    while !stopped.load(Ordering::Acquire) {
        unsafe {
            let _ = windows::Win32::System::Threading::ResetEvent(read_event.0);
        }
        let mut overlapped = OVERLAPPED {
            hEvent: read_event.0,
            ..Default::default()
        };
        let mut bytes_read = 0u32;
        let read_res = unsafe {
            ReadFile(
                handle.0,
                Some(&mut buffer),
                Some(&mut bytes_read),
                Some(&mut overlapped),
            )
        };
        if read_res.is_err() {
            let err = unsafe { windows::Win32::Foundation::GetLastError() };
            if err != ERROR_IO_PENDING {
                crate::log::error(&format!("Touchpad reader stopped: {:?}", err));
                break;
            }
            let handles = [read_event.0, stop_event.0];
            let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
            if wait != WAIT_OBJECT_0 {
                unsafe {
                    // The cancelled read still owns `overlapped` and `buffer` until it completes:
                    // wait for that before they go out of scope (ERROR_OPERATION_ABORTED is fine).
                    if CancelIoEx(handle.0, Some(&overlapped)).is_ok() {
                        let _ = GetOverlappedResult(handle.0, &overlapped, &mut bytes_read, true);
                    }
                }
                break;
            }
            if unsafe { GetOverlappedResult(handle.0, &overlapped, &mut bytes_read, false) }
                .is_err()
            {
                break;
            }
        }
        if bytes_read == 0 {
            break;
        }
        if bytes_read >= 3 && buffer[0] == REPORT_ID {
            process_gesture(&action_lock, buffer[1], buffer[2]);
        }
    }
    let _ = ptr::addr_of!(stopped);
    let _ = size_of::<c_void>();
}

fn process_gesture(
    action_lock: &Mutex<Option<(u8, u8, Instant)>>,
    gesture_type: u8,
    direction: u8,
) {
    if gesture_type != GESTURE_EVENT_COMMAND || !matches!(direction, DIRECTION_UP | DIRECTION_DOWN) {
        return;
    }
    let Ok(mut last) = action_lock.lock() else {
        return;
    };
    let now = Instant::now();
    if let Some((prev_type, prev_dir, prev_time)) = *last
        && prev_type == gesture_type
        && prev_dir == direction
        && now.duration_since(prev_time).as_millis() < REPEAT_SUPPRESS_MS
    {
        return;
    }
    *last = Some((gesture_type, direction, now));

    let up = direction == DIRECTION_UP;
    if !try_step_acpi_brightness(up) {
        let delta = if up {
            BRIGHTNESS_STEP_PERCENT
        } else {
            -BRIGHTNESS_STEP_PERCENT
        };
        if change_wmi_brightness(delta) < 0 {
            crate::log::error(t(
                "Не удалось изменить яркость.",
                "Could not change brightness.",
                "无法调整亮度。",
            ));
        }
    }
}

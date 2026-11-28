use super::wmi::{change_wmi_brightness, try_step_acpi_brightness};
use crate::settings;
use grandhonor_core::locale::t;
use grandhonor_core::protocol::{TouchpadEdgeGesture, TouchpadHaptics};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_BUTTON_CAPS, HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData,
    HidD_GetAttributes, HidD_GetHidGuid, HidD_GetPreparsedData, HidP_GetCaps, HidP_GetSpecificButtonCaps,
    HidP_Input, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, HWND, LPARAM, WAIT_OBJECT_0,
    WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows_core::HSTRING;

mod gestures;
pub mod input;

pub const REPORT_ID: u8 = 0x0E;
pub const HAPTICS_COMMAND: u8 = 0x02;
pub const GESTURE_EVENT_COMMAND: u8 = 0x03;
const VOLUME_EVENT: u8 = 0x04;
const GESTURE_SUPPORT_QUERY: u8 = 0xF0;
const DEVICE_IO_WAIT_MS: u32 = 750;
const VENDOR_USAGE_PAGE: u16 = 0xFF00;
const VENDOR_USAGE: u16 = 0x0001;
const DIRECTION_UP: u8 = 0x01;
const DIRECTION_DOWN: u8 = 0x02;
const REPEAT_SUPPRESS_MS: u128 = 150;
const BRIGHTNESS_STEP_PERCENT: i32 = 3;

pub const SUPPORTED_DEVICES: [(u16, u16); 2] = [(0x27C6, 0x0F9A), (0x35CC, 0x0104)];
pub const WM_APP_TOUCHPAD_ACTION: u32 = WM_APP + 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Gesture {
    Inward,
    LeftCorner,
    RightCorner,
    BottomLeft,
    BottomRight,
    LeftOne,
    RightOne,
    TopOne,
    BottomOne,
    LeftTwo,
    TopTwo,
    BottomTwo,
}

impl Gesture {
    pub const ALL: [Self; 12] = [
        Self::Inward, Self::LeftCorner, Self::RightCorner, Self::BottomLeft, Self::BottomRight,
        Self::LeftOne, Self::RightOne, Self::TopOne, Self::BottomOne,
        Self::LeftTwo, Self::TopTwo, Self::BottomTwo,
    ];

    pub fn from_action_id(id: usize) -> Option<Self> {
        Self::ALL.get(id).copied()
    }

    fn from_firmware(event: u8, direction: u8) -> Option<Self> {
        match (event, direction) {
            (0x0A, 0x03) => Some(Self::Inward),
            (0x08, _) => Some(Self::LeftCorner),
            (0x09, _) => Some(Self::RightCorner),
            _ => None,
        }
    }

    fn firmware_bit(self) -> Option<u32> {
        match self {
            Self::Inward => Some(1 << 18),
            Self::LeftCorner => Some(1 << 15),
            Self::RightCorner => Some(1 << 16),
            _ => None,
        }
    }

    fn firmware_enabled(self) -> bool {
        self.firmware_bit().is_some_and(|bit| GESTURES.load(Ordering::Acquire) & bit != 0)
    }

    fn output_command(self) -> Option<u8> {
        match self {
            Self::Inward if self.firmware_enabled() => Some(0x09),
            Self::LeftCorner | Self::RightCorner => Some(0x0A),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Inward => "Inward",
            Self::LeftCorner => "LeftCorner",
            Self::RightCorner => "RightCorner",
            Self::BottomLeft => "BottomLeft",
            Self::BottomRight => "BottomRight",
            Self::LeftOne => "LeftOne",
            Self::RightOne => "RightOne",
            Self::TopOne => "TopOne",
            Self::BottomOne => "BottomOne",
            Self::LeftTwo => "LeftTwo",
            Self::TopTwo => "TopTwo",
            Self::BottomTwo => "BottomTwo",
        }
    }

    pub fn is_supported(self) -> bool {
        self.firmware_enabled()
            || (self.output_command().is_none() && input::supports(self.fingers()))
    }

    fn fingers(self) -> u8 {
        match self {
            Self::Inward | Self::LeftTwo | Self::TopTwo | Self::BottomTwo => 2,
            _ => 1,
        }
    }
}

pub(super) fn dispatch(gesture: Gesture, window: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(window as *mut _)), WM_APP_TOUCHPAD_ACTION, WPARAM(gesture as usize), LPARAM(0));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureAction {
    None,
    Panel,
    Minimize,
    Screenshot,
}

impl GestureAction {
    pub const ALL: [Self; 4] = [Self::None, Self::Panel, Self::Minimize, Self::Screenshot];

    pub fn current(gesture: Gesture) -> Self {
        settings::touchpad_assignment(gesture.key())
            .and_then(|saved| Self::parse(&saved))
            .unwrap_or(Self::None)
    }

    fn parse(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.code() == code)
    }

    fn code(self) -> &'static str {
        match self {
            Self::None => "off",
            Self::Panel => "panel",
            Self::Minimize => "minimize",
            Self::Screenshot => "screenshot",
        }
    }
}

static GESTURES: AtomicU32 = AtomicU32::new(0);
static ASSIGNMENT_WRITE: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
pub struct HidDeviceInfo {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_report_len: u16,
    pub output_report_len: u16,
    /// A separate standard HID collection also offers volume up/down.
    consumer_volume: bool,
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
        if let Some(info) = inspect_hid_device(path) {
            devices.push(info);
        }
    }
    Ok(devices)
}

fn inspect_hid_device(path: String) -> Option<HidDeviceInfo> {
    let wide = HSTRING::from(path.as_str());
    let handle = OwnedHandle(unsafe {
        CreateFileW(&wide, 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, Default::default(), None)
    }.ok()?);
    let mut attrs = HIDD_ATTRIBUTES { Size: size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
    if !unsafe { HidD_GetAttributes(handle.0, &mut attrs) } {
        return None;
    }

    let mut caps = HIDP_CAPS::default();
    let mut preparsed = PHIDP_PREPARSED_DATA::default();
    if !unsafe { HidD_GetPreparsedData(handle.0, &mut preparsed) } {
        return None;
    }
    let ok = unsafe { HidP_GetCaps(preparsed, &mut caps) } == HIDP_STATUS_SUCCESS;
    let consumer_volume = ok && caps.UsagePage == 0x0c && caps.Usage == 1 && [0xe9, 0xea].iter().any(|&usage| {
        let mut count = caps.NumberInputButtonCaps;
        let mut matches = vec![HIDP_BUTTON_CAPS::default(); usize::from(count)];
        count > 0 && unsafe {
            HidP_GetSpecificButtonCaps(HidP_Input, Some(0x0c), None, Some(usage), matches.as_mut_ptr(), &mut count, preparsed)
        } == HIDP_STATUS_SUCCESS && count > 0
    });
    unsafe { let _ = HidD_FreePreparsedData(preparsed); }
    ok.then_some(HidDeviceInfo {
        path,
        vendor_id: attrs.VendorID,
        product_id: attrs.ProductID,
        usage_page: caps.UsagePage,
        usage: caps.Usage,
        input_report_len: caps.InputReportByteLength,
        output_report_len: caps.OutputReportByteLength,
        consumer_volume,
    })
}

fn get_device_path(set: HDEVINFO, iface: &mut SP_DEVICE_INTERFACE_DATA) -> Option<String> {
    let mut required = 0u32;
    unsafe {
        let _ = SetupDiGetDeviceInterfaceDetailW(set, iface, None, 0, Some(&mut required), None);
    }
    if required < std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 {
        return None;
    }
    let mut buf = vec![0u32; (required as usize).div_ceil(size_of::<u32>())];
    let detail_ptr = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    unsafe {
        (*detail_ptr).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        SetupDiGetDeviceInterfaceDetailW(set, iface, Some(detail_ptr), required, None, None).ok()?;
        let char_offset = std::mem::offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
        let wchar_ptr = buf.as_ptr().cast::<u8>().add(char_offset).cast::<u16>();
        let max_wchars = (required as usize - char_offset) / 2;
        let slice = std::slice::from_raw_parts(wchar_ptr, max_wchars);
        let len = slice.iter().position(|&c| c == 0).unwrap_or(max_wchars);
        Some(String::from_utf16_lossy(&slice[..len]))
    }
}

static DEVICE_CACHE: OnceLock<Mutex<Option<Option<HidDeviceInfo>>>> = OnceLock::new();
static KNOWN_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static DEVICE_CHANGES: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

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

/// Queue arrivals for off-thread identification. A removed interface no longer opens, so use
/// paths of the collections actually identified when the reader was started.
pub fn queue_device_change(path: String, arrived: bool) -> bool {
    if path.is_empty() || (!arrived && !KNOWN_PATHS.lock().is_ok_and(|known| {
        known.iter().any(|old| old.eq_ignore_ascii_case(&path))
    })) {
        return false;
    }
    let Ok(mut changes) = DEVICE_CHANGES.lock() else { return false };
    if !changes.iter().any(|(old, previous)| *previous == arrived && old.eq_ignore_ascii_case(&path)) {
        changes.push((path, arrived));
    }
    true
}

pub fn take_device_changes() -> bool {
    let changes = DEVICE_CHANGES.lock().map(|mut changes| std::mem::take(&mut *changes)).unwrap_or_default();
    changes.into_iter().any(|(path, arrived)| !arrived || inspect_hid_device(path).is_some_and(|d| supported_identity(&d)))
}

fn supported_identity(d: &HidDeviceInfo) -> bool {
    SUPPORTED_DEVICES.contains(&(d.vendor_id, d.product_id))
}

fn vendor_control(d: &HidDeviceInfo) -> bool {
    supported_identity(d) && d.usage_page == VENDOR_USAGE_PAGE && d.usage == VENDOR_USAGE
        && d.input_report_len >= 3 && d.output_report_len >= 3
}

fn find_vendor_device() -> Option<HidDeviceInfo> {
    let Ok(mut guard) = device_cache().lock() else {
        return None;
    };
    if let Some(cached) = guard.as_ref() {
        return cached.clone();
    }
    let found = match enumerate_hid_devices() {
        Ok(list) => list.into_iter().find(vendor_control),
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
            FILE_FLAG_OVERLAPPED,
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

    let mut report = vec![0u8; usize::from(device.output_report_len)];
    report[0] = REPORT_ID;
    report[1] = command;
    report[2] = value;

    let evt = OwnedHandle(unsafe { CreateEventW(None, true, false, None) }.map_err(|e| e.to_string())?);
    let mut overlapped = OVERLAPPED { hEvent: evt.0, ..Default::default() };
    let mut written = 0u32;
    let res = unsafe { WriteFile(owned.0, Some(&report), Some(&mut written), Some(&mut overlapped)) };
    let success = if res.is_ok() {
        true
    } else if res.is_err_and(|err| err.code() == ERROR_IO_PENDING.to_hresult()) {
        if unsafe { WaitForSingleObject(evt.0, DEVICE_IO_WAIT_MS) } == WAIT_OBJECT_0 {
            unsafe { GetOverlappedResult(owned.0, &overlapped, &mut written, false) }.is_ok()
        } else {
            cancel_pending(owned.0, &overlapped);
            false
        }
    } else {
        false
    };
    if !success || written as usize != report.len() {
        return Err(t(
            "Не удалось записать настройку в тачпад.",
            "Could not write the setting to the touchpad.",
            "无法将设置写入触控板。",
        )
        .to_string());
    }
    Ok(())
}

fn cancel_pending(handle: HANDLE, overlapped: &OVERLAPPED) {
    // A completion can race cancellation. Always drain it before freeing the buffer.
    unsafe {
        let _ = CancelIoEx(handle, Some(overlapped));
        let _ = GetOverlappedResult(handle, overlapped, &mut 0, true);
    }
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

pub fn assign_gesture(gesture: Gesture, action: GestureAction) -> Result<(), String> {
    let _guard = ASSIGNMENT_WRITE.lock().map_err(|_| t("Не удалось изменить назначение жеста.", "Could not change the gesture action.", "无法更改手势操作。").to_string())?;
    if !gesture.is_supported() {
        return Err(t("Тачпад не сообщил о поддержке этого жеста.", "The touchpad did not report support for this gesture.", "触控板未报告支持此手势。").to_string());
    }
    let Some(command) = gesture.output_command() else {
        return if settings::set_touchpad_assignment(gesture.key(), action.code()) {
            Ok(())
        } else {
            Err(t("Не удалось сохранить назначение жеста.", "Could not save the gesture action.", "无法保存手势操作。").to_string())
        };
    };
    let other_enabled = Gesture::ALL.into_iter().any(|other| {
        other != gesture && other.output_command() == Some(command) && other.firmware_enabled() && GestureAction::current(other) != GestureAction::None
    });
    let previous = GestureAction::current(gesture) != GestureAction::None || other_enabled;
    let enabled = action != GestureAction::None || other_enabled;
    send_vendor_command(command, u8::from(enabled))?;
    if settings::set_touchpad_assignment(gesture.key(), action.code()) {
        return Ok(());
    }
    if previous != enabled && let Err(err) = send_vendor_command(command, u8::from(previous)) {
        crate::log::error(&format!("Could not restore touchpad gesture setting: {err}"));
    }
    Err(t("Не удалось сохранить назначение жеста.", "Could not save the gesture action.", "无法保存手势操作。").to_string())
}

fn reapply_assignments(device: &HidDeviceInfo) {
    for (command, gestures) in [(0x09, &[Gesture::Inward][..]), (0x0A, &[Gesture::LeftCorner, Gesture::RightCorner][..])] {
        let mut choices = gestures.iter().copied().filter(|g| g.firmware_enabled()).filter_map(|g| {
            settings::touchpad_assignment(g.key()).and_then(|saved| GestureAction::parse(&saved))
                .map(|action| action != GestureAction::None)
        }).peekable();
        if choices.peek().is_some()
            && let Err(err) = try_send(device, command, u8::from(choices.any(|enabled| enabled)))
        {
            crate::log::error(&format!("Could not restore touchpad gesture setting: {err}"));
        }
    }
}

struct Reader {
    stop_event: Arc<OwnedHandle>,
    thread: JoinHandle<()>,
}

impl Reader {
    fn stop(self) {
        unsafe {
            let _ = SetEvent(self.stop_event.0);
        }
        let _ = self.thread.join();
    }
}

pub struct TouchpadService {
    lifecycle: Mutex<Option<Reader>>,
    window: isize,
}

impl TouchpadService {
    pub fn new(window: isize) -> Self {
        Self {
            lifecycle: Mutex::new(None),
            window,
        }
    }

    pub fn start(&self) {
        let Ok(mut guard) = self.lifecycle.lock() else {
            return;
        };
        if let Some(prev) = guard.take() {
            prev.stop();
        }
        match self.spawn_reader() {
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
        if let Some(prev) = guard.take() {
            prev.stop();
        }
        if let Ok(Some(group)) = self.spawn_reader() {
            *guard = Some(group);
            crate::log::info("Touchpad reader restarted");
        }
    }

    fn spawn_reader(&self) -> Result<Option<Reader>, String> {
        GESTURES.store(0, Ordering::Release);
        let candidates = enumerate_hid_devices()?;
        if let Ok(mut paths) = KNOWN_PATHS.lock() {
            *paths = candidates.iter().filter(|d| supported_identity(d)).map(|d| d.path.clone()).collect();
        }
        let device = candidates.iter().find(|d| vendor_control(d)).cloned();
        if let Ok(mut cached) = device_cache().lock() {
            *cached = Some(device.clone());
        }
        let Some(device) = device else { return Ok(None) };
        let consumer_volume = candidates.iter().any(|d| {
            d.vendor_id == device.vendor_id && d.product_id == device.product_id && d.consumer_volume
        });

        let stop_event = Arc::new(OwnedHandle(unsafe { CreateEventW(None, true, false, None) }.map_err(|e| e.to_string())?));
        let wide = HSTRING::from(device.path.as_str());
        let reader = OwnedHandle(unsafe {
            CreateFileW(&wide, GENERIC_READ.0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, None)
        }.map_err(|e| e.to_string())?);
        let stop = stop_event.clone();
        let window = self.window;
        let read_device = device.clone();
        let thread = thread::Builder::new().name("touchpad-hid".into()).stack_size(256 * 1024)
            .spawn(move || read_device_loop(read_device, reader, stop, consumer_volume, window))
            .map_err(|e| e.to_string())?;
        // This report only asks for the supported gesture bitmask. The already-open reader
        // receives the reply in the same input collection as its regular gesture events.
        if let Err(err) = try_send(&device, GESTURE_SUPPORT_QUERY, 0) {
            crate::log::error(&format!("Touchpad gesture capabilities unavailable: {err}"));
        };
        Ok(Some(Reader { stop_event, thread }))
    }
}

impl Drop for TouchpadService {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.lifecycle.lock()
            && let Some(group) = guard.take()
        {
            group.stop();
        }
    }
}

fn read_device_loop(
    device: HidDeviceInfo,
    handle: OwnedHandle,
    stop_event: Arc<OwnedHandle>,
    consumer_volume: bool,
    window: isize,
) {
    let Ok(read_ev_raw) = (unsafe { CreateEventW(None, true, false, None) }) else {
        return;
    };
    let read_event = OwnedHandle(read_ev_raw);

    let buf_len = usize::from(device.input_report_len);
    let mut buffer = vec![0u8; buf_len];
    let mut last = None;

    while unsafe { WaitForSingleObject(stop_event.0, 0) } != WAIT_OBJECT_0 {
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
        if let Err(err) = read_res {
            if err.code() != ERROR_IO_PENDING.to_hresult() {
                crate::log::error(&format!("Touchpad reader stopped: {:?}", err));
                break;
            }
            let handles = [read_event.0, stop_event.0];
            let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
            if wait != WAIT_OBJECT_0 {
                cancel_pending(handle.0, &overlapped);
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
        if let Some(report) = buffer.get(..bytes_read as usize)
            && let Some((&event, data)) = report.strip_prefix(&[REPORT_ID]).and_then(|payload| payload.split_first())
        {
            if event == GESTURE_SUPPORT_QUERY {
                if let Some(mask) = gesture_mask(data)
                    && let Ok(_guard) = ASSIGNMENT_WRITE.lock()
                {
                    GESTURES.store(mask, Ordering::Release);
                    reapply_assignments(&device);
                    input::settings_changed(window);
                }
            } else {
                process_gesture(&mut last, event, data.first().copied().unwrap_or_default(), consumer_volume, window);
            }
        }
    }
    GESTURES.store(0, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::{Gesture, gesture_mask};

    #[test]
    fn reported_capabilities_distinguish_edge_and_corner_gestures() {
        assert_eq!(gesture_mask(&[0x78, 0x80, 0x15]), None);
        let reported = gesture_mask(&[0x78, 0x80, 0x15, 0x08, 0, 0, 0]).unwrap();
        for gesture in [Gesture::Inward, Gesture::LeftCorner, Gesture::RightCorner] {
            assert_ne!(reported & gesture.firmware_bit().unwrap(), 0);
        }
        assert_eq!(Gesture::from_firmware(8, 0), Some(Gesture::LeftCorner));
        assert_eq!(Gesture::from_firmware(9, 0), Some(Gesture::RightCorner));
        assert_eq!(Gesture::from_firmware(0x0A, 3), Some(Gesture::Inward));
        assert_eq!(Gesture::from_firmware(0x0A, 4), None);
        assert_eq!(Gesture::from_firmware(7, 0), None);
    }
}

fn gesture_mask(data: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(..4)?.try_into().ok()?))
}

fn process_gesture(
    last: &mut Option<(u8, u8, Instant)>,
    gesture_type: u8,
    direction: u8,
    consumer_volume: bool,
    window: isize,
) {
    let assignment = Gesture::from_firmware(gesture_type, direction).filter(|g| g.firmware_enabled());
    let edge = matches!(gesture_type, GESTURE_EVENT_COMMAND | VOLUME_EVENT)
        && matches!(direction, DIRECTION_UP | DIRECTION_DOWN);
    if assignment.is_none() && !edge {
        return;
    }
    let now = Instant::now();
    if let Some((prev_type, prev_dir, prev_time)) = *last
        && prev_type == gesture_type
        && prev_dir == direction
        && now.duration_since(prev_time).as_millis() < REPEAT_SUPPRESS_MS
    {
        return;
    }
    *last = Some((gesture_type, direction, now));
    if let Some(gesture) = assignment {
        if GestureAction::current(gesture) != GestureAction::None {
            dispatch(gesture, window);
        }
        return;
    }

    let up = direction == DIRECTION_UP;
    if gesture_type == VOLUME_EVENT {
        // Prefer the existing Windows input route where the device also exposes volume buttons.
        // A vendor-only report needs a host action exactly like the vendor brightness report.
        if !consumer_volume && is_gesture_enabled(TouchpadEdgeGesture::Volume)
            && !super::audio::step_output_volume(up)
        {
            crate::log::error("Could not step speaker volume for a touchpad gesture");
        }
    } else if is_gesture_enabled(TouchpadEdgeGesture::Brightness) && !try_step_acpi_brightness(up) {
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

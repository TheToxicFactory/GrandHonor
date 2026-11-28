use super::display;
use crate::settings;
use grandhonor_core::locale::t;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_PATH_INFO,
    DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo,
};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::ColorSystem::{
    CPST_EXTENDED_DISPLAY_COLOR_MODE, CPT_ICC, ColorProfileGetDisplayDefault, ColorProfileGetDisplayList,
    ColorProfileGetDisplayUserScope, ColorProfileSetDisplayDefaultAssociation,
};
use windows_core::{HSTRING, PWSTR};

// Windows SDK wingdi.h. These packet types are newer than the bindings in windows 0.61.
const GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
const SET_HDR_STATE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(16);

#[repr(C)]
struct AdvancedColorInfo {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

#[repr(C)]
struct SetHdrState {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
}

#[derive(Clone, Copy)]
pub struct HdrState {
    pub supported: bool,
    pub enabled: bool,
    pub active: bool,
    pub limited: bool,
}

fn query(path: &DISPLAYCONFIG_PATH_INFO) -> Option<HdrState> {
    let mut info = AdvancedColorInfo {
        header: display::target_header::<AdvancedColorInfo>(path, GET_ADVANCED_COLOR_INFO_2),
        flags: 0,
        color_encoding: 0,
        bits_per_color_channel: 0,
        active_color_mode: 0,
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut info.header) } != 0 {
        return None;
    }
    Some(HdrState {
        supported: info.flags & (1 << 4) != 0,
        enabled: info.flags & (1 << 5) != 0,
        active: info.active_color_mode == 2,
        limited: info.flags & (1 << 3) != 0,
    })
}

pub fn status() -> Option<HdrState> {
    query(&display::built_in_path()?)
}

/// Magnification applies to the whole desktop, even if HDR is only on one output.
pub fn any_hdr_active() -> bool {
    display::active_paths().is_some_and(|paths| {
        paths.iter().any(|path| query(path).is_some_and(|state| state.active || state.enabled))
    })
}

fn unavailable() -> &'static str {
    t("HDR недоступен на встроенном экране.", "HDR is unavailable on the built-in display.", "内置显示屏无法使用 HDR。")
}

fn set_state(path: &DISPLAYCONFIG_PATH_INFO, enabled: bool) -> Result<(), String> {
    let Some(state) = query(path).filter(|state| state.supported) else {
        return Err(unavailable().into());
    };
    if state.enabled == enabled {
        return Ok(());
    }
    let request = SetHdrState {
        header: display::target_header::<SetHdrState>(path, SET_HDR_STATE),
        flags: u32::from(enabled),
    };
    let error = unsafe { DisplayConfigSetDeviceInfo(&request.header) };
    if error != 0 {
        return Err(format!("{} ({error})", t("Windows отклонила переключение HDR.", "Windows refused to switch HDR.", "Windows 拒绝切换 HDR。")));
    }
    if !query(path).is_some_and(|state| state.enabled == enabled) {
        return Err(t("Windows не подтвердила новый режим HDR.", "Windows did not confirm the new HDR setting.", "Windows 未确认新的 HDR 状态。").into());
    }
    super::display_filter::apply_saved();
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Policy {
    Manual = 0,
    Ac = 1,
    Programs = 2,
}

pub fn policy() -> Policy {
    match settings::hdr_policy() {
        1 => Policy::Ac,
        2 => Policy::Programs,
        _ => Policy::Manual,
    }
}

struct Override {
    monitor: String,
    restore: bool,
}

struct Scenario {
    requested: Option<Policy>,
    overridden: Option<Override>,
    manual: bool,
}

static SCENARIO: Mutex<Scenario> = Mutex::new(Scenario {
    requested: None,
    overridden: None,
    manual: false,
});
static PROGRAM_RUNNING: AtomicBool = AtomicBool::new(false);
static POWER_SOURCE: AtomicU8 = AtomicU8::new(255);
static STOPPED: AtomicBool = AtomicBool::new(false);

fn requested_scenario() -> Option<Policy> {
    match policy() {
        Policy::Ac if power_source() == 1 => Some(Policy::Ac),
        Policy::Programs if PROGRAM_RUNNING.load(Ordering::Acquire) => Some(Policy::Programs),
        _ => None,
    }
}

pub fn set_manual(enabled: bool) -> Result<(), String> {
    let path = display::built_in_path().ok_or_else(unavailable)?;
    let mut scenario = SCENARIO.lock().map_err(|_| unavailable())?;
    set_state(&path, enabled)?;
    scenario.requested = requested_scenario();
    scenario.manual = true;
    scenario.overridden = None;
    Ok(())
}

pub fn set_policy(policy: Policy) {
    settings::set_hdr_policy(policy as u32);
    super::app_profiles::settings_changed();
    refresh();
}

pub fn watched_programs() -> Vec<String> {
    if policy() == Policy::Programs { settings::hdr_programs() } else { Vec::new() }
}

pub fn program_state_changed(active: bool) {
    if PROGRAM_RUNNING.swap(active, Ordering::AcqRel) != active && policy() == Policy::Programs {
        refresh();
    }
}

fn power_source() -> u8 {
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_ok() { status.ACLineStatus } else { 255 }
}

pub fn power_state_changed() {
    if policy() == Policy::Ac {
        let source = power_source();
        if POWER_SOURCE.swap(source, Ordering::AcqRel) != source {
            refresh();
        }
    }
}

pub fn topology_changed() {
    CALIBRATION.clear();
    if policy() != Policy::Manual {
        refresh();
    }
}

fn refresh() {
    if STOPPED.load(Ordering::Acquire) {
        return;
    }
    static WORK: OnceLock<Option<mpsc::SyncSender<()>>> = OnceLock::new();
    let worker = WORK.get_or_init(|| {
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("display-hdr".into()).stack_size(256 * 1024).spawn(move || {
            while receive.recv().is_ok() {
                if let Err(err) = reconcile(requested_scenario()) {
                    crate::log::error(&err);
                }
            }
        }).ok().map(|_| send)
    });
    if let Some(worker) = worker {
        let _ = worker.try_send(());
    }
}

fn reconcile(requested: Option<Policy>) -> Result<(), String> {
    let mut scenario = SCENARIO.lock().map_err(|_| unavailable())?;
    let requested = requested.filter(|_| !STOPPED.load(Ordering::Acquire));
    if requested != scenario.requested {
        scenario.manual = false;
        scenario.requested = requested;
    }
    if requested.is_none() && scenario.overridden.is_none() {
        return Ok(());
    }
    let Some(path) = display::built_in_path() else { return Ok(()) };
    let Some(monitor) = display::monitor_path(&path) else { return Ok(()) };
    let Some(state) = query(&path).filter(|state| state.supported) else { return Ok(()) };
    if let Some(owned) = scenario.overridden.as_ref() {
        if owned.monitor != monitor {
            scenario.overridden = None;
        } else if !state.enabled {
            // Windows or another program changed what we set.
            scenario.overridden = None;
            scenario.manual = true;
        } else if requested.is_none() {
            set_state(&path, owned.restore)?;
            scenario.overridden = None;
        }
    }
    if requested.is_some() && !scenario.manual && scenario.overridden.is_none() {
        if !state.enabled {
            set_state(&path, true)?;
        }
        scenario.overridden = Some(Override { monitor, restore: state.enabled });
    }
    Ok(())
}

pub fn restore_on_exit() {
    STOPPED.store(true, Ordering::Release);
    if let Err(err) = reconcile(None) {
        crate::log::error(&err);
    }
}

fn default_profile(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    let adapter = path.targetInfo.adapterId;
    let source = path.sourceInfo.id;
    unsafe {
        let scope = ColorProfileGetDisplayUserScope(adapter, source).ok()?;
        let name = ColorProfileGetDisplayDefault(scope, adapter, source, CPT_ICC, CPST_EXTENDED_DISPLAY_COLOR_MODE).ok()?;
        let result = (!name.is_null()).then(|| name.to_string().ok()).flatten();
        let _ = LocalFree(Some(HLOCAL(name.0.cast())));
        result
    }
}

/// The association list also contains SDR profiles. Windows HDR Calibration saves an MHC2 tag.
fn has_mhc2_profile(name: &str) -> bool {
    let Some(file_name) = std::path::Path::new(name).file_name() else { return false };
    let path = crate::known_dirs::system32().join("spool").join("drivers").join("color").join(file_name);
    let Ok(mut profile) = std::fs::File::open(path) else { return false };
    let mut header = [0; 132];
    if profile.read_exact(&mut header).is_err() || &header[36..40] != b"acsp" {
        return false;
    }
    let count = u32::from_be_bytes(header[128..132].try_into().unwrap());
    for _ in 0..count.min(4096) {
        let mut entry = [0; 12];
        if profile.read_exact(&mut entry).is_err() {
            return false;
        }
        if &entry[..4] == b"MHC2" {
            return true;
        }
    }
    false
}

fn associated_profiles(path: &DISPLAYCONFIG_PATH_INFO) -> Vec<String> {
    let adapter = path.targetInfo.adapterId;
    let source = path.sourceInfo.id;
    let mut raw: *mut PWSTR = std::ptr::null_mut();
    let mut count = 0;
    unsafe {
        let Ok(scope) = ColorProfileGetDisplayUserScope(adapter, source) else { return Vec::new() };
        if ColorProfileGetDisplayList(scope, adapter, source, &mut raw, &mut count).is_err() || raw.is_null() {
            return Vec::new();
        }
        let list = std::slice::from_raw_parts(raw, count.min(256) as usize)
            .iter().filter_map(|name| (!name.is_null()).then(|| name.to_string().ok()).flatten())
            .collect();
        let _ = LocalFree(Some(HLOCAL(raw.cast())));
        list
    }
}

type Calibration = (Vec<String>, Option<String>);
static CALIBRATION: crate::cache::Ttl<Calibration> = crate::cache::Ttl::new(std::time::Duration::from_secs(5));

pub fn calibration_profiles() -> Calibration {
    CALIBRATION.get(read_calibration_profiles)
}

fn read_calibration_profiles() -> Calibration {
    let Some(path) = display::built_in_path() else { return (Vec::new(), None) };
    (associated_profiles(&path).into_iter().filter(|name| has_mhc2_profile(name)).collect(), default_profile(&path))
}

pub fn select_calibration(name: &str) -> Result<(), String> {
    let path = display::built_in_path().ok_or_else(unavailable)?;
    if !associated_profiles(&path).iter().any(|profile| profile == name) || !has_mhc2_profile(name) {
        return Err(t("Профиль HDR больше не доступен.", "The HDR profile is no longer available.", "HDR 配置文件已不可用。").into());
    }
    let adapter = path.targetInfo.adapterId;
    let source = path.sourceInfo.id;
    unsafe {
        let scope = ColorProfileGetDisplayUserScope(adapter, source).map_err(|e| e.to_string())?;
        ColorProfileSetDisplayDefaultAssociation(scope, &HSTRING::from(name), CPT_ICC, CPST_EXTENDED_DISPLAY_COLOR_MODE, adapter, source)
            .map_err(|e| e.to_string())?;
    }
    CALIBRATION.clear();
    if !default_profile(&path).is_some_and(|actual| actual.eq_ignore_ascii_case(name)) {
        return Err(t("Windows не подтвердила профиль HDR.", "Windows did not confirm the HDR profile.", "Windows 未确认 HDR 配置文件。").into());
    }
    Ok(())
}

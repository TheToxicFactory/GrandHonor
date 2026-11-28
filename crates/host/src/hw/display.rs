//! Built-in display refresh rate, for the HONOR refresh-rate key (event 0x2A7).
//! Toggles the built-in display between its highest rate and 60 Hz (or its lowest rate)
//! at the current resolution, the same switch Windows Settings → Display → Advanced offers.

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HLOCAL, LocalFree};
use windows::Win32::Graphics::Gdi::{
    CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DM_DISPLAYFREQUENCY, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW,
};
use windows::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, PowerSetActiveScheme,
    PowerWriteACValueIndex, PowerWriteDCValueIndex,
};
use windows::Win32::UI::Shell::{ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage};
use windows_core::{GUID, PCWSTR};

pub(super) fn active_paths() -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    let flags = QDC_ONLY_ACTIVE_PATHS;
    // Hotplug can change the required lengths between the two API calls.
    for _ in 0..3 {
        let (mut paths, mut modes) = (0, 0);
        unsafe { GetDisplayConfigBufferSizes(flags, &mut paths, &mut modes) }.ok().ok()?;
        let mut path_info = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
        let mut mode_info = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
        let result = unsafe {
            QueryDisplayConfig(flags, &mut paths, path_info.as_mut_ptr(), &mut modes, mode_info.as_mut_ptr(), None)
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        result.ok().ok()?;
        path_info.truncate(paths as usize);
        return Some(path_info);
    }
    None
}

pub(super) fn built_in_path() -> Option<DISPLAYCONFIG_PATH_INFO> {
    active_paths()?.into_iter().find(is_internal)
}

pub fn built_in_active() -> bool {
    built_in_path().is_some()
}

pub fn built_in_only() -> bool {
    active_paths().is_some_and(|paths| paths.len() == 1 && is_internal(&paths[0]))
}

pub(super) fn target_header<T>(path: &DISPLAYCONFIG_PATH_INFO, kind: DISPLAYCONFIG_DEVICE_INFO_TYPE) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size_of::<T>() as u32,
        adapterId: path.targetInfo.adapterId,
        id: path.targetInfo.id,
    }
}

pub(super) fn monitor_path(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: target_header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(path, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME),
        ..Default::default()
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } != 0 {
        return None;
    }
    let end = name.monitorDevicePath.iter().position(|&ch| ch == 0).unwrap_or(name.monitorDevicePath.len());
    (end > 0).then(|| String::from_utf16_lossy(&name.monitorDevicePath[..end]))
}

fn source_name() -> Option<DISPLAYCONFIG_SOURCE_DEVICE_NAME> {
    let paths = active_paths()?;
    let path = paths.iter().find(|path| is_internal(path))?;
    // A cloned source would change the external screen at the same time.
    if paths.iter().filter(|other| {
        other.sourceInfo.adapterId == path.sourceInfo.adapterId && other.sourceInfo.id == path.sourceInfo.id
    }).count() != 1 {
        return None;
    }
    let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
            r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
            adapterId: path.sourceInfo.adapterId,
            id: path.sourceInfo.id,
        },
        ..Default::default()
    };
    (unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } == 0 && name.viewGdiDeviceName[0] != 0).then_some(name)
}

fn is_internal(path: &DISPLAYCONFIG_PATH_INFO) -> bool {
    path.targetInfo.targetAvailable.as_bool() && matches!(
        path.targetInfo.outputTechnology,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS
            | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
    )
}

fn mode(name: PCWSTR, index: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe { EnumDisplaySettingsW(name, index, &mut dm) }.as_bool().then_some(dm)
}

/// Rates the built-in display supports at its current resolution and color depth.
fn supported_rates(name: PCWSTR, current: &DEVMODEW) -> Vec<u32> {
    let mut rates = Vec::new();
    for i in 0..1000 {
        let Some(m) = mode(name, ENUM_DISPLAY_SETTINGS_MODE(i)) else { break };
        if m.dmPelsWidth == current.dmPelsWidth
            && m.dmPelsHeight == current.dmPelsHeight
            && m.dmBitsPerPel == current.dmBitsPerPel
            && m.dmDisplayFrequency > 1
            && !rates.contains(&m.dmDisplayFrequency)
        {
            rates.push(m.dmDisplayFrequency);
        }
    }
    rates.sort_unstable();
    rates
}

static RATES: crate::cache::Ttl<Option<(Vec<u32>, u32)>> = crate::cache::Ttl::new(std::time::Duration::from_secs(30));

/// Supported refresh rates and the current one, cached for 30 s (up to a thousand mode
/// queries); every rate change made here clears it.
pub fn rates() -> Option<(Vec<u32>, u32)> {
    RATES.get(rates_now)
}

pub fn topology_changed() {
    RATES.clear();
}

fn rates_now() -> Option<(Vec<u32>, u32)> {
    let display = source_name()?;
    let name = PCWSTR(display.viewGdiDeviceName.as_ptr());
    let current = mode(name, ENUM_CURRENT_SETTINGS)?;
    let rates = supported_rates(name, &current);
    (rates.len() >= 2).then_some((rates, current.dmDisplayFrequency))
}

pub fn current_rate() -> Option<u32> {
    let display = source_name()?;
    mode(PCWSTR(display.viewGdiDeviceName.as_ptr()), ENUM_CURRENT_SETTINGS).map(|mode| mode.dmDisplayFrequency)
}

/// Sets an exact refresh rate if the display supports it at the current resolution.
pub fn set_refresh_rate(hz: u32) -> bool {
    RATES.clear();
    let Some(display) = source_name() else { return false };
    let name = PCWSTR(display.viewGdiDeviceName.as_ptr());
    let Some(mut current) = mode(name, ENUM_CURRENT_SETTINGS) else { return false };
    if current.dmDisplayFrequency == hz {
        return true;
    }
    if !supported_rates(name, &current).contains(&hz) {
        return false;
    }
    current.dmDisplayFrequency = hz;
    current.dmFields = DM_DISPLAYFREQUENCY;
    let result = unsafe { ChangeDisplaySettingsExW(name, Some(&current), None, CDS_UPDATEREGISTRY, None) };
    result == DISP_CHANGE_SUCCESSFUL && mode(name, ENUM_CURRENT_SETTINGS).is_some_and(|mode| mode.dmDisplayFrequency == hz)
}

/// Switches to the other rate and returns it, or `None` if there is nothing to switch.
pub fn toggle_refresh_rate() -> Option<u32> {
    RATES.clear();
    let display = source_name()?;
    let name = PCWSTR(display.viewGdiDeviceName.as_ptr());
    let current = mode(name, ENUM_CURRENT_SETTINGS)?;
    let rates = supported_rates(name, &current);
    let max = *rates.last()?;
    let low = if rates.contains(&60) { 60 } else { *rates.first()? };
    if low == max {
        return None;
    }
    let target = if current.dmDisplayFrequency >= max { low } else { max };
    set_refresh_rate(target).then_some(target)
}

const VIDEO_SUBGROUP: GUID = GUID::from_u128(0x7516b95f_f776_4464_8c53_06167f40cc99);
const VIDEO_IDLE: GUID = GUID::from_u128(0x3c0bc021_c8a8_4e07_a973_6b14cbcb2b7e);

fn scheme() -> Option<GUID> {
    let mut raw = std::ptr::null_mut();
    unsafe {
        PowerGetActiveScheme(None, &mut raw).ok().ok()?;
        let scheme = raw.as_ref().copied();
        let _ = LocalFree(Some(HLOCAL(raw.cast())));
        scheme
    }
}

fn read_timeout(scheme: &GUID, ac: bool) -> Option<u32> {
    let mut seconds = 0;
    let code = unsafe {
        if ac {
            PowerReadACValueIndex(None, Some(scheme), Some(&VIDEO_SUBGROUP), Some(&VIDEO_IDLE), &mut seconds).0
        } else {
            PowerReadDCValueIndex(None, Some(scheme), Some(&VIDEO_SUBGROUP), Some(&VIDEO_IDLE), &mut seconds)
        }
    };
    (code == 0).then_some(seconds)
}

pub fn display_timeouts() -> Option<(Option<u32>, Option<u32>)> {
    let scheme = scheme()?;
    Some((read_timeout(&scheme, true), read_timeout(&scheme, false)))
}

pub fn set_display_timeout(ac: bool, seconds: u32) -> bool {
    super::power::with_power_scheme_write(|| set_timeout_on_active_scheme(ac, seconds))
}

fn set_timeout_on_active_scheme(ac: bool, seconds: u32) -> bool {
    let Some(active) = scheme() else { return false };
    let Some(original) = read_timeout(&active, ac) else { return false };
    if original == seconds {
        return true;
    }
    let write = |value| unsafe {
        if ac {
            PowerWriteACValueIndex(None, &active, Some(&VIDEO_SUBGROUP), Some(&VIDEO_IDLE), value).is_ok()
        } else {
            PowerWriteDCValueIndex(None, &active, Some(&VIDEO_SUBGROUP), Some(&VIDEO_IDLE), value) == 0
        }
    };
    if !write(seconds) {
        return false;
    }
    if scheme() != Some(active)
        || unsafe { PowerSetActiveScheme(None, Some(&active)) }.is_err()
        || read_timeout(&active, ac) != Some(seconds)
    {
        let _ = write(original);
        if scheme() == Some(active) {
            let _ = unsafe { PowerSetActiveScheme(None, Some(&active)) };
        }
        return false;
    }
    true
}

pub fn taskbar_auto_hidden() -> bool {
    let mut taskbar = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, ..Default::default() };
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut taskbar) & ABS_AUTOHIDE as usize != 0 }
}

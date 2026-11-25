use super::wmi::{WmiError, honor_wmi_call};
use crate::settings;
use grandhonor_core::locale::t;
use std::sync::OnceLock;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_core::{GUID, s, w};

const POWER_UNLOCK_SET_COMMAND: u64 = 0x0000_0F04;
pub const WINDOWS_EFFICIENCY: GUID = GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a);
pub const WINDOWS_BALANCED: GUID = GUID::zeroed();
pub const WINDOWS_PERFORMANCE: GUID = GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238);

type GetPowerMode = unsafe extern "system" fn(*mut GUID) -> u32;
type SetPowerMode = unsafe extern "system" fn(*const GUID) -> u32;

struct PowerModeApi {
    ac: (GetPowerMode, SetPowerMode),
    dc: (GetPowerMode, SetPowerMode),
}

fn current_windows_mode_api() -> Option<(GetPowerMode, SetPowerMode)> {
    static API: OnceLock<Option<PowerModeApi>> = OnceLock::new();
    // Resolve these entry points on demand for Windows builds that lack them.
    // The system DLL remains loaded for the lifetime of the cached function pointers.
    let api = API
        .get_or_init(|| unsafe {
            let module =
                LoadLibraryExW(w!("powrprof.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            let get_ac = GetProcAddress(module, s!("PowerGetUserConfiguredACPowerMode"))?;
            let set_ac = GetProcAddress(module, s!("PowerSetUserConfiguredACPowerMode"))?;
            let get_dc = GetProcAddress(module, s!("PowerGetUserConfiguredDCPowerMode"))?;
            let set_dc = GetProcAddress(module, s!("PowerSetUserConfiguredDCPowerMode"))?;
            Some(PowerModeApi {
                ac: (
                    std::mem::transmute::<_, GetPowerMode>(get_ac),
                    std::mem::transmute::<_, SetPowerMode>(set_ac),
                ),
                dc: (
                    std::mem::transmute::<_, GetPowerMode>(get_dc),
                    std::mem::transmute::<_, SetPowerMode>(set_dc),
                ),
            })
        })
        .as_ref()?;
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    match status.ACLineStatus {
        0 => Some(api.dc),
        1 => Some(api.ac),
        _ => None,
    }
}

pub fn windows_power_mode() -> Option<GUID> {
    let (get, _) = current_windows_mode_api()?;
    let mut mode = GUID::zeroed();
    (unsafe { get(&mut mode) } == 0).then_some(mode)
}

pub fn set_windows_power_mode(mode: GUID) -> Result<(), String> {
    let (_, set) = current_windows_mode_api().ok_or_else(|| {
        t(
            "Режим питания Windows недоступен.",
            "Windows power mode is unavailable.",
            "Windows 电源模式不可用。",
        )
        .to_string()
    })?;
    let result = unsafe { set(&mode) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(result as i32).to_string())
    }
}

/// A short notice for the OSD when Fn+P set Windows Performance but HONOR Performance could
/// not be turned on: the Windows mode did change, so it is named first.
pub fn performance_refusal_short() -> &'static str {
    let mut status = SYSTEM_POWER_STATUS::default();
    let read = unsafe { GetSystemPowerStatus(&mut status) }.is_ok();
    if crate::settings::hunter_mode_active() {
        t("Производительность · HONOR: идёт HUNTER", "Performance · HONOR: HUNTER is on", "性能 · HONOR：HUNTER 已开启")
    } else if read && status.ACLineStatus != 1 {
        t("Производительность · HONOR: без зарядки", "Performance · HONOR: on battery", "性能 · HONOR：电池供电")
    } else if read && status.BatteryLifePercent < 20 {
        t("Производительность · HONOR: заряд ниже 20%", "Performance · HONOR: charge below 20%", "性能 · HONOR：电量低于 20%")
    } else {
        t("Производительность · HONOR недоступен", "Performance · HONOR unavailable", "性能 · HONOR 不可用")
    }
}

pub fn can_enable_performance_mode() -> Result<(), String> {
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return Err(t(
            "Не удалось определить состояние питания.",
            "Could not read system power status.",
            "无法读取电源状态。",
        )
        .to_string());
    }
    if status.ACLineStatus != 1 {
        return Err(t(
            "«Производительный HONOR» доступен только от сети.",
            "HONOR Performance needs the charger.",
            "HONOR 高性能仅在接通电源时可用。",
        )
        .to_string());
    }
    if status.BatteryLifePercent <= 100 && status.BatteryLifePercent < 20 {
        return Err(t(
            "Для «Производительного HONOR» заряд должен быть не ниже 20%.",
            "HONOR Performance needs at least 20% charge.",
            "HONOR 高性能要求电量不低于 20%。",
        )
        .to_string());
    }
    if status.BatteryLifePercent == 255 {
        crate::log::info(
            "Battery charge is unknown; performance mode allowed because AC power is connected",
        );
    }
    Ok(())
}

/// When GrandHonor itself switched the HONOR profile: the firmware then reports the change
/// as an OEM event that must not be mistaken for a Fn+P press. The time is kept in the
/// settings store on the system-wide tick clock, so a change made by another GrandHonor
/// process (a CLI helper or a hardware check) is recognised by the tray host too.
fn system_tick() -> u32 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() as u32 }
}

/// True for ~1.5 s after any GrandHonor process changed the HONOR profile.
pub fn is_software_echo() -> bool {
    settings::profile_write_tick().is_some_and(|at| system_tick().wrapping_sub(at) < 1_500)
}

/// The three profiles Fn+P cycles through: Windows power mode plus the HONOR profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Economy,
    Balanced,
    Performance,
}

pub fn current_profile() -> Profile {
    match windows_power_mode() {
        Some(mode) if mode == WINDOWS_EFFICIENCY => Profile::Economy,
        Some(mode) if mode == WINDOWS_PERFORMANCE => Profile::Performance,
        _ => Profile::Balanced,
    }
}

/// Applies a profile: Economy = Windows Efficiency, Balanced = Windows Balanced (HONOR normal),
/// Performance = Windows Performance plus HONOR Performance when the charger and 20 % allow it.
/// Returns whether HONOR Performance could be turned on (only meaningful for Performance).
pub fn apply_profile(profile: Profile) -> bool {
    let (mode, honor) = match profile {
        Profile::Economy => (WINDOWS_EFFICIENCY, false),
        Profile::Balanced => (WINDOWS_BALANCED, false),
        Profile::Performance => (WINDOWS_PERFORMANCE, true),
    };
    if let Err(err) = set_windows_power_mode(mode) {
        crate::log::error(&format!("Could not set Windows power mode: {err}"));
    }
    let honor = honor && can_enable_performance_mode().is_ok() && !settings::hunter_mode_active();
    // Fn+P changes HONOR Performance for now but keeps the choice made in the panel, so a
    // step to Efficiency does not turn the saved choice off (it comes back on plug-in/resume).
    if settings::performance_mode_active() != honor {
        match set_power_unlock(honor) {
            Ok(()) => settings::set_performance_mode_active(honor),
            Err(err) => crate::log::error_with("Could not switch the HONOR profile", &err),
        }
    }
    honor
}

pub fn set_power_unlock(enabled: bool) -> Result<(), WmiError> {
    if enabled {
        can_enable_performance_mode().map_err(WmiError::Unavailable)?;
        if settings::hunter_mode_active() {
            return Err(WmiError::Unavailable(
                t("Включён режим HUNTER.", "HUNTER mode is on.", "HUNTER 模式已开启。").to_string(),
            ));
        }
    }
    let command = POWER_UNLOCK_SET_COMMAND | ((if enabled { 1u64 } else { 0u64 }) << 16);
    settings::set_profile_write_tick(system_tick());
    honor_wmi_call(command)?;
    Ok(())
}

/// Turns HONOR Performance off for now (sleep, battery) without changing the user's choice.
pub fn suspend_performance() -> Result<(), WmiError> {
    set_power_unlock(false)?;
    settings::set_performance_mode_active(false);
    Ok(())
}

/// Brings HONOR Performance back when the user chose it and the charger, 20 % and no HUNTER
/// allow it: at start-up (the firmware forgets it on reboot), after sleep and on plug-in.
/// Returns true when it was turned on.
pub fn restore_performance_preference() -> bool {
    if !settings::power_unlock_preference()
        || settings::performance_mode_active()
        || settings::hunter_mode_active()
        || can_enable_performance_mode().is_err()
    {
        return false;
    }
    match set_power_unlock(true) {
        Ok(()) => {
            settings::set_performance_mode_active(true);
            crate::log::info("HONOR Performance restored from the saved choice");
            true
        }
        Err(err) => {
            crate::log::error_with("Could not restore HONOR Performance", &err);
            false
        }
    }
}

pub fn apply_power_unlock_and_save(enabled: bool) -> Result<(), WmiError> {
    set_power_unlock(enabled)?;
    settings::set_power_unlock(Some(enabled));
    settings::set_performance_mode_active(enabled);
    Ok(())
}

//! Built-in display refresh rate, for the HONOR refresh-rate key (event 0x2A7).
//! Toggles the primary display between its highest rate and 60 Hz (or its lowest rate)
//! at the current resolution, the same switch Windows Settings → Display → Advanced offers.

use windows::Win32::Graphics::Gdi::{
    CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DM_DISPLAYFREQUENCY, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW,
};
use windows_core::PCWSTR;

fn mode(index: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe { EnumDisplaySettingsW(PCWSTR::null(), index, &mut dm) }.as_bool().then_some(dm)
}

/// Rates the primary display supports at its current resolution and color depth.
fn supported_rates(current: &DEVMODEW) -> Vec<u32> {
    let mut rates = Vec::new();
    for i in 0..1000 {
        let Some(m) = mode(ENUM_DISPLAY_SETTINGS_MODE(i)) else { break };
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

fn rates_now() -> Option<(Vec<u32>, u32)> {
    let current = mode(ENUM_CURRENT_SETTINGS)?;
    let rates = supported_rates(&current);
    (rates.len() >= 2).then_some((rates, current.dmDisplayFrequency))
}

/// Sets an exact refresh rate if the display supports it at the current resolution.
pub fn set_refresh_rate(hz: u32) -> bool {
    RATES.clear();
    let Some(mut current) = mode(ENUM_CURRENT_SETTINGS) else { return false };
    if current.dmDisplayFrequency == hz {
        return true;
    }
    if !supported_rates(&current).contains(&hz) {
        return false;
    }
    current.dmDisplayFrequency = hz;
    current.dmFields = DM_DISPLAYFREQUENCY;
    let result = unsafe { ChangeDisplaySettingsExW(PCWSTR::null(), Some(&current), None, CDS_UPDATEREGISTRY, None) };
    result == DISP_CHANGE_SUCCESSFUL
}

/// Switches to the other rate and returns it, or `None` if there is nothing to switch.
pub fn toggle_refresh_rate() -> Option<u32> {
    RATES.clear();
    let mut current = mode(ENUM_CURRENT_SETTINGS)?;
    let rates = supported_rates(&current);
    let max = *rates.last()?;
    let low = if rates.contains(&60) { 60 } else { *rates.first()? };
    if low == max {
        return None;
    }
    let target = if current.dmDisplayFrequency >= max { low } else { max };
    current.dmDisplayFrequency = target;
    current.dmFields = DM_DISPLAYFREQUENCY;
    let result = unsafe { ChangeDisplaySettingsExW(PCWSTR::null(), Some(&current), None, CDS_UPDATEREGISTRY, None) };
    (result == DISP_CHANGE_SUCCESSFUL).then_some(target)
}

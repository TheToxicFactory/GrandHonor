//! Presets: settings applied automatically when the charger is plugged in or removed.
//!
//! Each power source ("Ac", "Dc") may set the Windows power mode, the keyboard backlight,
//! the display brightness and refresh rate; an unset value is left alone. Nothing runs on a timer: Windows sends
//! `PBT_APMPOWERSTATUSCHANGE` (tray.rs), and only a real charger change triggers a preset.

use crate::hw::power::{WINDOWS_BALANCED, WINDOWS_EFFICIENCY, WINDOWS_PERFORMANCE, set_windows_power_mode};
use crate::settings;
use grandhonor_core::protocol::BacklightLevel;
use std::sync::atomic::{AtomicU8, Ordering};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

pub const MODE: &str = "Mode"; // 1 efficiency, 2 balanced, 3 performance
pub const BACKLIGHT: &str = "Backlight"; // 1 off, 2 weak, 3 strong
pub const BRIGHTNESS: &str = "Brightness"; // percent + 1 (1 = 0 %)
pub const REFRESH: &str = "Refresh"; // refresh rate in Hz

/// 0 unknown, 1 battery, 2 charger.
static LAST_SOURCE: AtomicU8 = AtomicU8::new(0);

fn current_source() -> u8 {
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return 0;
    }
    match status.ACLineStatus {
        0 => 1,
        1 => 2,
        _ => 0,
    }
}

/// Remembers the source at start-up; presets fire only on a later change.
pub fn start() {
    let now = current_source();
    LAST_SOURCE.store(now, Ordering::Release);
    if now != 0 {
        crate::hw::smart_charge::on_source(now == 2);
    }
}

pub fn source_key(on_ac: bool) -> &'static str {
    if on_ac { "Ac" } else { "Dc" }
}

/// Called for every power status broadcast (also sent for each battery percent change).
pub fn on_power_status_change() {
    let now = current_source();
    let before = LAST_SOURCE.swap(now, Ordering::AcqRel);
    if now == 0 || now == before || before == 0 {
        return;
    }
    crate::hw::smart_charge::on_source(now == 2);
    if !settings::preset_enabled() {
        return;
    }
    let key = source_key(now == 2);
    let _ = std::thread::Builder::new()
        .name("preset".into())
        .stack_size(256 * 1024)
        .spawn(move || apply(key));
}

/// Applies one source's preset now (also used by the panel's "apply now").
pub fn apply(key: &'static str) {
    if let Some(mode) = settings::preset_value(key, MODE) {
        let guid = match mode {
            1 => Some(WINDOWS_EFFICIENCY),
            2 => Some(WINDOWS_BALANCED),
            3 => Some(WINDOWS_PERFORMANCE),
            _ => None,
        };
        if let Some(guid) = guid {
            if let Err(err) = set_windows_power_mode(guid) {
                crate::log::error(&format!("Preset power mode failed: {err}"));
            }
        }
    }
    if let Some(level) = settings::preset_value(key, BACKLIGHT) {
        let level = match level {
            1 => Some(BacklightLevel::Off),
            2 => Some(BacklightLevel::Low),
            3 => Some(BacklightLevel::High),
            _ => None,
        };
        if let Some(level) = level {
            match crate::hw::backlight::set_backlight_level(level) {
                Ok(()) => settings::set_keyboard_backlight(Some(level)),
                Err(err) => crate::log::error_with("Preset backlight failed", &err),
            }
        }
    }
    if let Some(hz) = settings::preset_value(key, REFRESH)
        && !crate::hw::display::set_refresh_rate(hz)
    {
        crate::log::error(&format!("Preset refresh rate {hz} Hz is not available"));
    }
    if let Some(value) = settings::preset_value(key, BRIGHTNESS).filter(|&v| v >= 1) {
        let _ = crate::hw::wmi::set_wmi_brightness(value - 1);
    }
    // Quiet nights outrank a preset's power mode: let them re-check after it.
    crate::hw::night_mode::poke();
}

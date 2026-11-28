//! Presets: settings applied automatically when the charger is plugged in or removed.
//!
//! Each power source ("Ac", "Dc") may set the Windows power mode, the keyboard backlight,
//! the display brightness and refresh rate; an unset value is left alone. Nothing runs on a timer: Windows sends
//! `PBT_APMPOWERSTATUSCHANGE` (tray.rs), and only a real charger change triggers a preset.

use crate::hw::power::{self, Applied, Profile};
use crate::settings;
use grandhonor_core::protocol::BacklightLevel;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

pub const MODE: &str = "Mode"; // 1 efficiency, 2 balanced, 3 performance
pub const BACKLIGHT: &str = "Backlight"; // 1 off, 2 weak, 3 strong
pub const BRIGHTNESS: &str = "Brightness"; // percent + 1 (1 = 0 %)
pub const REFRESH: &str = "Refresh"; // refresh rate in Hz

/// 0 unknown, 1 battery, 2 charger.
static LAST_SOURCE: AtomicU8 = AtomicU8::new(0);
static APPLY_HARDWARE: Mutex<()> = Mutex::new(());

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

pub fn source_change_pending() -> bool {
    let source = current_source();
    source != 0 && source != LAST_SOURCE.load(Ordering::Acquire)
}

/// Called for every power status broadcast (also sent for each battery percent change).
pub fn on_power_status_change() -> bool {
    let now = current_source();
    if now == 0 {
        return false;
    }
    let before = LAST_SOURCE.swap(now, Ordering::AcqRel);
    if now == before {
        return false;
    }
    let ac = now == 2;
    crate::hw::smart_charge::on_source(ac);
    if before != 0 && settings::preset_enabled() {
        let key = source_key(ac);
        if let Some(profile) = settings::preset_value(key, MODE).and_then(Profile::from_code) {
            power::apply_automatic_legacy_preset(ac, profile);
        }
        if let Err(err) = apply_hardware(ac) {
            crate::log::error(&err);
        }
    }
    true
}

pub fn apply(key: &'static str) {
    if let Err(err) = apply_now(key) {
        crate::log::error(&err);
    }
}

/// Apply the profile to its named source. Device settings wait for that source to be active.
pub fn apply_now(key: &'static str) -> Result<Option<Applied>, String> {
    let ac = key == "Ac";
    let profile = settings::preset_value(key, MODE).and_then(Profile::from_code);
    let result = if let Some(profile) = profile { power::apply_preset_profile(ac, profile)? } else { None };
    let devices = apply_hardware(ac);
    crate::hw::night_mode::poke();
    devices?;
    Ok(result)
}

fn apply_hardware(ac: bool) -> Result<(), String> {
    let _guard = APPLY_HARDWARE.lock().unwrap_or_else(|err| err.into_inner());
    let active = || power::current_source() == Some(ac);
    let key = source_key(ac);
    let mut ok = true;
    if let Some(level) = settings::preset_value(key, BACKLIGHT) {
        let level = match level {
            1 => Some(BacklightLevel::Off),
            2 => Some(BacklightLevel::Low),
            3 => Some(BacklightLevel::High),
            _ => None,
        };
        if let Some(level) = level && active() {
            match crate::hw::backlight::set_backlight_level(level) {
                Ok(()) => settings::set_keyboard_backlight(Some(level)),
                Err(err) => {
                    crate::log::error_with("Preset backlight failed", &err);
                    ok = false;
                }
            }
        }
    }
    if let Some(hz) = settings::preset_value(key, REFRESH)
        && active()
        && !crate::hw::display::set_refresh_rate(hz)
    {
        crate::log::error(&format!("Preset refresh rate {hz} Hz is not available"));
        ok = false;
    }
    if let Some(value) = settings::preset_value(key, BRIGHTNESS).filter(|&v| (1..=101).contains(&v))
        && active()
    {
        ok &= crate::hw::wmi::set_wmi_brightness(value - 1);
    }
    if ok {
        Ok(())
    } else {
        Err(grandhonor_core::locale::t(
            "Часть настроек пресета не удалось применить.",
            "Some preset settings could not be applied.",
            "无法应用部分预设设置。",
        ).into())
    }
}

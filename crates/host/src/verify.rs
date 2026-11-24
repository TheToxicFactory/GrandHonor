//! Verification hooks for hardware checks (run elevated, output redirected).
//! They use the same code paths as the tray host and the panel; nothing here runs in normal use.

use crate::hw::power::{WINDOWS_BALANCED, WINDOWS_EFFICIENCY, WINDOWS_PERFORMANCE, set_windows_power_mode};
use crate::{hw, presets, settings};
use serde_json::json;
use std::io::Write;
use std::time::{Duration, Instant};

fn emit(value: serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// Prints `count` panel samples (`sensors::sample_live`, the same call the open Power tab
/// makes) as JSON lines, one every 0.5 s, each with the elapsed milliseconds.
pub fn sample_live(count: u32) -> i32 {
    // Same setup as panel_host: one COM apartment and one WMI connection for all samples.
    let _com = hw::com::ComGuard::mta();
    let _live_session = hw::sensors::keep_session_on_this_thread();
    let start = Instant::now();
    for index in 0..count {
        let due = Duration::from_millis(500 * u64::from(index));
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        let sample = hw::sensors::sample_live();
        emit(json!({ "elapsed_ms": start.elapsed().as_millis() as u64, "sample": sample }));
    }
    0
}

/// Prints the Windows power mode, refresh rate, presets and the settings file location.
pub fn print_state() -> i32 {
    let preset = |key: &str| {
        json!({
            "mode": settings::preset_value(key, presets::MODE),
            "backlight": settings::preset_value(key, presets::BACKLIGHT),
            "brightness": settings::preset_value(key, presets::BRIGHTNESS),
            "refresh": settings::preset_value(key, presets::REFRESH),
        })
    };
    let (rates, current) = hw::display::rates().unwrap_or_default();
    emit(json!({
        "windows_mode": hw::power::windows_power_mode().map(|g| format!("{g:?}")),
        "refresh_rates": rates,
        "refresh_rate": current,
        "presets_enabled": settings::preset_enabled(),
        "preset_ac": preset("Ac"),
        "preset_dc": preset("Dc"),
        "settings_file": settings::store::path(),
        "settings_count": settings::all_user_settings().len(),
    }));
    0
}

pub fn set_windows_mode(mode: u32) -> i32 {
    let guid = match mode {
        1 => WINDOWS_EFFICIENCY,
        3 => WINDOWS_PERFORMANCE,
        _ => WINDOWS_BALANCED,
    };
    match set_windows_power_mode(guid) {
        Ok(()) => 0,
        Err(err) => {
            crate::log::error(&err);
            1
        }
    }
}

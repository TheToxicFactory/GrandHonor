use super::com::ComGuard;
use std::cell::RefCell;
use super::wmi::{HonorWmiSession, WmiError, read_battery_power_watts};
use crate::settings;
use grandhonor_core::locale::t;
use grandhonor_core::protocol::{
    BacklightLevel, BatteryMode, SensorSnapshot, TelemetrySummary, now_dotnet_ticks,
};

const FAN_SPEED_GET_COMMAND: u64 = 0x0000_0802;
const TEMPERATURE_GET_COMMAND: u64 = 0x0000_0202;
const KEYBOARD_BACKLIGHT_GET_COMMAND: u64 = 0x0000_1306;
const BATTERY_THRESHOLDS_GET_COMMAND: u64 = 0x0000_1103;
const PERFORMANCE_MODE_GET_COMMAND: u64 = 0x0000_0E04;

const FAN_PLAUSIBLE_MAX_RPM: i32 = 8000;
const SMART_MODE: i32 = 0;
const PERFORMANCE_MODE: i32 = 1;
const HUNTER_MODE: i32 = 3;

thread_local! {
    /// A WMI connection kept alive on the panel IPC thread while the panel is open, so each
    /// six-second sample costs only the BIOS calls instead of a new WMI connection.
    static LIVE_SESSION: RefCell<Option<HonorWmiSession>> = const { RefCell::new(None) };
}

/// Keeps one WMI session on the calling thread until the returned guard is dropped.
/// The caller must hold a `ComGuard` for longer than the guard.
pub fn keep_session_on_this_thread() -> LiveSessionGuard {
    LIVE_SESSION.with(|cell| *cell.borrow_mut() = Some(HonorWmiSession::new()));
    LiveSessionGuard
}

pub struct LiveSessionGuard;

impl Drop for LiveSessionGuard {
    fn drop(&mut self) {
        LIVE_SESSION.with(|cell| cell.borrow_mut().take());
    }
}

pub fn read_and_store(request_id: &str) -> Result<SensorSnapshot, WmiError> {
    let _com = ComGuard::mta();
    LIVE_SESSION.with(|cell| match cell.borrow_mut().as_mut() {
        Some(session) => read_with(session, request_id),
        None => read_with(&mut HonorWmiSession::new(), request_id),
    })
}

/// Slow-changing readings (battery temperature, charge limit, firmware profile, backlight)
/// are refreshed at most this often; fast ones are read on every panel sample.
const SLOW_REFRESH_MS: u64 = 5_000;
static LAST_FULL_READ_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Charger contract from the HONOR USB-C controller, refreshed with the slow readings:
/// watts (0 = none) and when they were read. Shown only while fresh and on AC.
static CHARGER_WATTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static CHARGER_READ_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Port 0 (USB-C) voltage in mV and current in mA, as HONOR's HardwareSdk reads them
/// (HotInterface::GetVoltageUSB0 / GetCurrentUSB0). Byte 0 is the status, bytes 2–3 the value.
const USB0_VOLTAGE_COMMAND: u64 = 0x0000_0902;
const USB0_CURRENT_COMMAND: u64 = 0x0010_0902;

fn read_charger(session: &mut HonorWmiSession) {
    let value = |session: &mut HonorWmiSession, command: u64| -> Option<u32> {
        let out = session.call(command).ok()?;
        if out.first() != Some(&0) || out.get(1) == Some(&1) {
            return None;
        }
        let v = u32::from(u16::from_le_bytes([*out.get(2)?, *out.get(3)?]));
        (v > 0).then_some(v)
    };
    let watts = match (value(session, USB0_VOLTAGE_COMMAND), value(session, USB0_CURRENT_COMMAND)) {
        // A sane USB-C PD contract: 5–48 V, up to 5 A.
        (Some(mv), Some(ma)) if (4_500..=48_500).contains(&mv) && ma <= 5_000 => (mv * ma + 500_000) / 1_000_000,
        _ => 0,
    };
    CHARGER_WATTS.store(watts, std::sync::atomic::Ordering::Release);
    CHARGER_READ_MS.store(super::events::monotonic_ms(), std::sync::atomic::Ordering::Release);
}

fn fresh_charger_watts() -> Option<u32> {
    use std::sync::atomic::Ordering;
    let at = CHARGER_READ_MS.load(Ordering::Acquire);
    let age = super::events::monotonic_ms().saturating_sub(at);
    (at != 0 && age <= SLOW_REFRESH_MS + 1_000)
        .then(|| CHARGER_WATTS.load(Ordering::Acquire))
        .filter(|&w| w > 0)
}

/// One live sample for the open Power tab (every 0.5 s): CPU temperature and both fans are
/// read from the firmware each time (3 BIOS calls); the rest comes from a full read that is
/// repeated every five seconds. Every returned value is from the latest successful read.
pub fn sample_live() -> TelemetrySummary {
    use std::sync::atomic::Ordering;
    let now = super::events::monotonic_ms();
    let last_full = LAST_FULL_READ_MS.load(Ordering::Acquire);
    let previous = settings::read_tooltip_state()
        .sensor_snapshot
        .as_deref()
        .and_then(SensorSnapshot::try_parse)
        .filter(SensorSnapshot::is_fresh);
    let need_full = previous.is_none() || last_full == 0 || now.saturating_sub(last_full) >= SLOW_REFRESH_MS;

    let _com = ComGuard::mta();
    LIVE_SESSION.with(|cell| {
        let mut slot = cell.borrow_mut();
        let mut fresh_session;
        let session = match slot.as_mut() {
            Some(session) => session,
            None => {
                fresh_session = HonorWmiSession::new();
                &mut fresh_session
            }
        };
        if need_full {
            let _ = read_with(session, "panel");
            if system_power_status().1 == Some(true) {
                read_charger(session);
            } else {
                CHARGER_WATTS.store(0, std::sync::atomic::Ordering::Release);
            }
        } else if let Some(mut snapshot) = previous {
            let (fan1_rpm, fan1_target_rpm) = read_fan(session, 0);
            let (fan2_rpm, fan2_target_rpm) = read_fan(session, 1);
            snapshot.cpu_temp = read_temperature(session, 0x00);
            snapshot.fan1_rpm = fan1_rpm;
            snapshot.fan1_target_rpm = fan1_target_rpm;
            snapshot.fan2_rpm = fan2_rpm;
            snapshot.fan2_target_rpm = fan2_target_rpm;
            snapshot.sampled_ticks = now_dotnet_ticks();
            settings::set_sensor_snapshot(Some(&snapshot.serialize("panel")));
        }
    });
    build_telemetry(true).1
}

fn read_with(session: &mut HonorWmiSession, request_id: &str) -> Result<SensorSnapshot, WmiError> {
    let backlight_mode = read_value(session, KEYBOARD_BACKLIGHT_GET_COMMAND, 1, "keyboard backlight mode");
    let (charge_start, charge_end) = read_battery_thresholds(session);
    let (fan1_rpm, fan1_target_rpm) = read_fan(session, 0);
    let (fan2_rpm, fan2_target_rpm) = read_fan(session, 1);
    let performance_mode = read_performance_mode(session);

    let snapshot = SensorSnapshot {
        sampled_ticks: now_dotnet_ticks(),
        fan1_rpm,
        fan2_rpm,
        cpu_temp: read_temperature(session, 0x00),
        battery_temp: read_temperature(session, 0x0E),
        keyboard_backlight_mode: backlight_mode,
        charge_start,
        charge_end,
        fan1_target_rpm,
        fan2_target_rpm,
        performance_mode,
    };

    settings::set_sensor_snapshot(Some(&snapshot.serialize(request_id)));
    LAST_FULL_READ_MS.store(super::events::monotonic_ms(), std::sync::atomic::Ordering::Release);

    if let Some(mode) = performance_mode
        && matches!(mode, SMART_MODE | PERFORMANCE_MODE | HUNTER_MODE)
    {
        settings::set_hunter_mode_active(mode == HUNTER_MODE);
        settings::set_performance_mode_active(mode == PERFORMANCE_MODE);
    }

    if let Some(level) = backlight_mode.and_then(BacklightLevel::from_wmi_byte) {
        settings::set_keyboard_backlight(Some(level));
    }

    if let Some(mode) = BatteryMode::from_thresholds(charge_start, charge_end) {
        settings::set_battery_protection(Some(mode));
    }

    Ok(snapshot)
}

fn read_fan(session: &mut HonorWmiSession, index: u8) -> (Option<i32>, Option<i32>) {
    match session.call(FAN_SPEED_GET_COMMAND | ((index as u64) << 16)) {
        // Byte 0 is the firmware status (HONOR's HardwareSdk discards the reading unless it is 0);
        // 0 rpm with status 0 is real: the fans stop when the laptop is cool.
        Ok(out) if out.first() != Some(&0) => (None, None),
        Ok(out) => {
            let actual = if out.len() >= 3 {
                Some(i32::from(u16::from_le_bytes([out[1], out[2]])))
            } else {
                None
            };
            let target = if out.len() >= 5 {
                Some(i32::from(u16::from_le_bytes([out[3], out[4]])))
            } else {
                None
            };
            (
                actual.filter(|&rpm| rpm <= FAN_PLAUSIBLE_MAX_RPM),
                target,
            )
        }
        Err(err) => {
            crate::log::error_with(&format!("Could not read fan {} speed", index + 1), &err);
            (None, None)
        }
    }
}

fn read_performance_mode(session: &mut HonorWmiSession) -> Option<i32> {
    match session.call(PERFORMANCE_MODE_GET_COMMAND) {
        Ok(out) => out.get(1).copied().map(i32::from),
        Err(err) if err.is_rejected().is_some() => None,
        Err(err) => {
            crate::log::error_with("Could not read performance mode", &err);
            None
        }
    }
}

/// Battery temperature alone (zone 0x0E), for the overheat notice on power broadcasts.
pub fn battery_temperature() -> Option<i32> {
    let _com = ComGuard::mta();
    read_temperature(&mut HonorWmiSession::new(), 0x0E)
}

fn read_temperature(session: &mut HonorWmiSession, zone: u8) -> Option<i32> {
    match session.call(TEMPERATURE_GET_COMMAND | ((zone as u64) << 16)) {
        // Status in byte 0, sign in byte 1 (1 = below zero), degrees in byte 2, as HONOR reads it.
        Ok(out) if out.first() != Some(&0) || out.get(1) == Some(&1) => None,
        Ok(out) => out
            .get(2)
            .copied()
            .map(i32::from)
            .filter(|&deg| deg > 0 && deg <= 125),
        Err(err) => {
            crate::log::error_with(&format!("Could not read temperature zone 0x{zone:02X}"), &err);
            None
        }
    }
}

fn read_battery_thresholds(session: &mut HonorWmiSession) -> (Option<i32>, Option<i32>) {
    match session.call(BATTERY_THRESHOLDS_GET_COMMAND) {
        Ok(out) => (
            out.get(1).copied().map(i32::from),
            out.get(2).copied().map(i32::from),
        ),
        Err(err) => {
            crate::log::error_with("Could not read battery charge thresholds", &err);
            (None, None)
        }
    }
}

fn read_value(
    session: &mut HonorWmiSession,
    command: u64,
    offset: usize,
    name: &str,
) -> Option<i32> {
    match session.call(command) {
        Ok(out) => out.get(offset).copied().map(i32::from),
        Err(err) => {
            crate::log::error_with(&format!("Could not read {name}"), &err);
            None
        }
    }
}

pub fn build_telemetry(include_power_watts: bool) -> (String, TelemetrySummary) {
    let state = settings::read_tooltip_state();
    let hw = state
        .sensor_snapshot
        .as_deref()
        .and_then(SensorSnapshot::try_parse)
        .filter(SensorSnapshot::is_fresh);

    let mode_label = if state.hunter_mode_active {
        "HUNTER".to_string()
    } else if state.performance_mode_active {
        t("Производительный HONOR", "HONOR Performance", "HONOR 高性能").to_string()
    } else {
        t("Обычный HONOR", "HONOR Standard", "HONOR 标准").to_string()
    };

    let backlight_level = hw
        .as_ref()
        .and_then(|s| s.keyboard_backlight_mode)
        .and_then(BacklightLevel::from_wmi_byte)
        .or(state.keyboard_backlight);
    let backlight_label = match backlight_level {
        Some(BacklightLevel::Off) => t("выкл.", "off", "关").to_string(),
        Some(BacklightLevel::Low) => t("слабая", "weak", "弱").to_string(),
        Some(BacklightLevel::High) => t("сильная", "strong", "强").to_string(),
        None => "—".to_string(),
    };

    let charge_label = match hw.as_ref().and_then(|s| Some((s.charge_start?, s.charge_end?))) {
        Some((start, end)) => format!("{start}–{end}%"),
        None => match state.battery_protection {
            Some(BatteryMode::Home) => "40–70%".to_string(),
            Some(BatteryMode::Office) => "70–90%".to_string(),
            Some(BatteryMode::Travel) => "95–100%".to_string(),
            Some(BatteryMode::Disabled) => t("выкл.", "off", "关").to_string(),
            None => "—".to_string(),
        },
    };

    let power_watts = if include_power_watts {
        read_battery_power_watts()
    } else {
        None
    };
    let power_label = power_watts.map(format_power_watts);
    let (battery_percent, ac_online) = system_power_status();

    let cpu_temp = hw.as_ref().and_then(|s| s.cpu_temp);
    let battery_temp = hw.as_ref().and_then(|s| s.battery_temp);
    let fan1_rpm = hw.as_ref().and_then(|s| s.fan1_rpm);
    let fan1_target = hw.as_ref().and_then(|s| s.fan1_target_rpm).filter(|&v| v > 0);
    let fan2_rpm = hw.as_ref().and_then(|s| s.fan2_rpm);
    let fan2_target = hw.as_ref().and_then(|s| s.fan2_target_rpm).filter(|&v| v > 0);

    let mut lines = vec![
        format!("{}{}", t("Режим: ", "Mode: ", "模式："), mode_label),
        format!("{}{}", t("Подсветка: ", "Backlight: ", "背光："), backlight_label),
        format!("{}{}", t("Заряд: ", "Charge limit: ", "充电限制："), charge_label),
    ];
    if let Some(ref pw) = power_label {
        lines.push(format!("{}{}", t("Батарея: ", "Battery: ", "电池功率："), pw));
    }
    if cpu_temp.is_some() || battery_temp.is_some() {
        let cpu = cpu_temp.map(|v| format!("{v}°C")).unwrap_or_else(|| "—".into());
        let bat = battery_temp
            .map(|v| format!("{v}°C"))
            .unwrap_or_else(|| "—".into());
        lines.push(format!(
            "CPU: {cpu}{}{bat}",
            t("; батарея: ", "; battery: ", "；电池：")
        ));
    }
    if fan1_rpm.is_some() || fan2_rpm.is_some() {
        let f1 = format_fan(fan1_rpm, fan1_target);
        let f2 = format_fan(fan2_rpm, fan2_target);
        if fan1_target.is_some() || fan2_target.is_some() {
            lines.push(format!("{}{} / {}", t("Вент.: ", "Fans: ", "风扇："), f1, f2));
        } else {
            lines.push(format!(
                "{}{}/{}{}",
                t("Вент.: ", "Fans: ", "风扇："),
                f1,
                f2,
                t(" об/мин", " RPM", " 转/分")
            ));
        }
    }

    let summary = TelemetrySummary {
        sampled_ticks: hw.as_ref().map(|snapshot| snapshot.sampled_ticks),
        mode_label,
        performance_active: state.performance_mode_active,
        hunter_active: state.hunter_mode_active,
        charge_label,
        backlight_label,
        cpu_temp,
        battery_temp,
        fan1_rpm,
        fan1_target,
        fan2_rpm,
        fan2_target,
        power_label,
        power_watts,
        battery_percent,
        ac_online,
        cpu_watts: if include_power_watts { super::cpu_power::package_watts() } else { None },
        charger_watts: if include_power_watts && ac_online == Some(true) { fresh_charger_watts() } else { None },
    };

    (lines.join("\n"), summary)
}

/// Battery level and charger state from the kernel: no WMI, no I/O, microseconds.
fn system_power_status() -> (Option<u8>, Option<bool>) {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return (None, None);
    }
    let percent = (status.BatteryLifePercent <= 100).then_some(status.BatteryLifePercent);
    let ac = match status.ACLineStatus {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    };
    (percent, ac)
}

fn format_fan(rpm: Option<i32>, target: Option<i32>) -> String {
    let base = rpm.map(|v| v.to_string()).unwrap_or_else(|| "—".into());
    match target {
        Some(tgt) => format!("{base}→{tgt}"),
        None => base,
    }
}

fn format_power_watts(watts: f64) -> String {
    if watts.abs() < 0.05 {
        return t("0 Вт", "0 W", "0 瓦").to_string();
    }
    let sign = if watts > 0.0 { "+" } else { "" };
    format!("{sign}{watts:.1}{}", t(" Вт", " W", " 瓦"))
}

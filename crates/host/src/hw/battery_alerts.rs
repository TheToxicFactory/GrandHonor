//! Battery notices and one-off charging: "Charge to 100 % once" (lifts the limit until the
//! battery is full or the charger is removed), a notice when charging stops at the limit and
//! when the battery runs low, the charge lost while asleep, and a monthly record of capacity.
//! Everything runs on Windows power broadcasts (tray.rs); nothing is polled.

use crate::hw::battery::set_battery_mode;
use crate::osd::Icon;
use crate::settings;
use grandhonor_core::locale::t;
use grandhonor_core::protocol::BatteryMode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetLocalTime;

/// The battery is called hot from this temperature (Li-ion should charge below 45 °C).
pub const HOT_C: i32 = 50;
/// The low-battery notice threshold, percent.
pub const LOW_PERCENT: u8 = 20;
/// Sleep shorter than this is not reported.
const MIN_SLEEP_SECS: u64 = 30 * 60;
/// Months of capacity history kept (one value per month).
const HISTORY_MONTHS: usize = 18;

static LAST_PERCENT: AtomicU8 = AtomicU8::new(u8::MAX);
static LIMIT_SHOWN: AtomicBool = AtomicBool::new(false);
static LOW_SHOWN: AtomicBool = AtomicBool::new(false);
/// When the last overheat notice was shown (wall-clock seconds), at most one an hour.
static HOT_SHOWN_AT: AtomicU64 = AtomicU64::new(0);
/// (wall-clock seconds, percent, on charger) when the laptop went to sleep.
static SUSPENDED: Mutex<Option<(u64, u8, bool)>> = Mutex::new(None);

/// (on the charger, battery percent), None without a battery or when Windows does not know.
pub fn status() -> Option<(bool, u8)> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    (s.BatteryLifePercent <= 100 && s.ACLineStatus <= 1).then_some((s.ACLineStatus == 1, s.BatteryLifePercent))
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Should a notice fire: the level crossed `at` in the given direction since the last broadcast.
fn crossed_up(before: u8, now: u8, at: u8) -> bool {
    before != u8::MAX && before < at && now >= at
}

fn crossed_down(before: u8, now: u8, at: u8) -> bool {
    before != u8::MAX && before > at && now <= at
}

/// Called for every power status broadcast (charger change and each percent change).
pub fn on_power_status() {
    let Some((on_ac, percent)) = status() else { return };
    let before = LAST_PERCENT.swap(percent, Ordering::AcqRel);
    if settings::full_charge_once() && (!on_ac || percent >= 100) {
        end_full_charge(on_ac && percent >= 100);
    }
    if on_ac {
        LOW_SHOWN.store(false, Ordering::Release);
    } else {
        LIMIT_SHOWN.store(false, Ordering::Release);
    }
    if !settings::charge_notices_enabled() {
        return;
    }
    // A percent step is the natural moment to look at the battery temperature: no timer.
    if before != percent
        && let Some(temp) = crate::hw::sensors::battery_temperature()
        && temp >= HOT_C
        && now_secs().saturating_sub(HOT_SHOWN_AT.load(Ordering::Acquire)) >= 3600
    {
        HOT_SHOWN_AT.store(now_secs(), Ordering::Release);
        crate::log::info(&format!("Battery is hot: {temp} C"));
        let text = format!("{} {temp} °C", t("Батарея нагрелась:", "Battery is hot:", "电池温度过高："));
        crate::osd::notify(Icon::Warning, &text);
    }
    if on_ac {
        let mode = settings::battery_protection().unwrap_or(BatteryMode::Disabled);
        // The firmware stops one percent short of the limit on some batteries.
        let end = mode.thresholds().1.saturating_sub(1);
        if mode != BatteryMode::Disabled && crossed_up(before, percent, end) && !LIMIT_SHOWN.swap(true, Ordering::AcqRel) {
            let text = format!("{} {}%", t("Заряд остановлен на", "Charging stopped at", "充电已停止于"), mode.thresholds().1);
            crate::osd::notify(Icon::Battery, &text);
        }
    } else if crossed_down(before, percent, LOW_PERCENT) && !LOW_SHOWN.swap(true, Ordering::AcqRel) {
        let text = format!("{percent}% · {}", t("подключите зарядку", "plug in the charger", "请连接充电器"));
        crate::osd::notify(Icon::Warning, &text);
    }
}

/// Charges to 100 % once: lifts the limit until the battery is full or the charger is removed.
pub fn start_full_charge() -> Result<(), String> {
    match status() {
        Some((true, _)) => {}
        _ => return Err(t("Сначала подключите зарядку.", "Plug in the charger first.", "请先连接充电器。").to_string()),
    }
    set_battery_mode(BatteryMode::Disabled).map_err(|e| e.to_string())?;
    settings::set_full_charge_once(true);
    settings::set_battery_protection(Some(BatteryMode::Disabled));
    crate::log::info("Full charge once: limit lifted until 100% or unplug");
    Ok(())
}

/// Puts the chosen limit back after a one-off full charge.
pub fn end_full_charge(full: bool) {
    if !settings::full_charge_once() {
        return;
    }
    settings::set_full_charge_once(false);
    let preferred = settings::preferred_battery_protection().unwrap_or(BatteryMode::Disabled);
    let mode = crate::hw::smart_charge::effective(preferred);
    match set_battery_mode(mode) {
        Ok(()) => settings::set_battery_protection(Some(mode)),
        Err(err) => crate::log::error_with("Full charge once: could not restore the limit", &err),
    }
    if full {
        crate::osd::notify(Icon::Battery, t("Батарея заряжена до 100%", "Battery charged to 100%", "电池已充至 100%"));
    }
}

pub fn on_suspend() {
    if let (Some((on_ac, percent)), Ok(mut slot)) = (status(), SUSPENDED.lock()) {
        *slot = Some((now_secs(), percent, on_ac));
    }
}

/// After waking: how much charge the sleep cost, when the laptop slept on battery.
pub fn on_resume() {
    let Some((at, before, was_ac)) = SUSPENDED.lock().ok().and_then(|mut s| s.take()) else { return };
    let Some((on_ac, percent)) = status() else { return };
    let slept = now_secs().saturating_sub(at);
    if was_ac || on_ac || slept < MIN_SLEEP_SECS {
        return;
    }
    let lost = before.saturating_sub(percent);
    let span = if slept >= 3600 {
        format!("{} {}", (slept + 1800) / 3600, t("ч", "h", "小时"))
    } else {
        format!("{} {}", slept / 60, t("мин", "min", "分钟"))
    };
    crate::log::info(&format!("Sleep on battery: {slept} s, {before}% -> {percent}%"));
    if settings::charge_notices_enabled() {
        let text = format!("{} {span}: −{lost}%", t("Сон", "Sleep", "睡眠"));
        crate::osd::notify(Icon::Battery, &text);
    }
}

fn month_now() -> u32 {
    let st = unsafe { GetLocalTime() };
    u32::from(st.wYear) * 100 + u32::from(st.wMonth)
}

/// Monthly full-charge capacity, oldest first: (yyyymm, mWh).
pub fn history() -> Vec<(u32, u32)> {
    parse_history(&settings::battery_wear().unwrap_or_default())
}

fn parse_history(text: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for (m, v) in text.split(',').filter_map(|p| {
        let (m, v) = p.split_once('=')?;
        Some((m.trim().parse::<u32>().ok()?, v.trim().parse::<u32>().ok()?))
    }) {
        if (200_001..=999_912).contains(&m) && v > 0 {
            record(&mut out, m, v);
        }
    }
    out
}

fn record(history: &mut Vec<(u32, u32)>, month: u32, mwh: u32) {
    match history.iter_mut().find(|e| e.0 == month) {
        Some(e) => e.1 = mwh,
        None => history.push((month, mwh)),
    }
    history.sort_unstable();
    let extra = history.len().saturating_sub(HISTORY_MONTHS);
    history.drain(..extra);
}

/// Stores this month's capacity (at start and after waking; one value per month).
pub fn record_capacity() {
    let Some(health) = crate::hw::battery_health::read() else { return };
    if health.full_mwh == 0 {
        return;
    }
    let mut h = history();
    record(&mut h, month_now(), health.full_mwh);
    let text = h.iter().map(|(m, v)| format!("{m}={v}")).collect::<Vec<_>>().join(",");
    settings::set_battery_wear(Some(&text));
}

/// "−1% over 6 months": capacity change against the oldest month, as a share of design.
pub fn wear_trend(design_mwh: u32) -> Option<(u32, i32)> {
    let h = history();
    let (first, last) = (h.first()?, h.last()?);
    let months = (last.0 / 100 * 12 + last.0 % 100).checked_sub(first.0 / 100 * 12 + first.0 % 100)?;
    if months == 0 || design_mwh == 0 {
        return None;
    }
    let delta = (i64::from(last.1) - i64::from(first.1)) * 100 / i64::from(design_mwh);
    Some((months, delta as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossings_fire_once_per_direction() {
        assert!(crossed_up(78, 79, 79));
        assert!(!crossed_up(79, 80, 79));
        assert!(!crossed_up(u8::MAX, 80, 79));
        assert!(crossed_down(21, 20, 20));
        assert!(!crossed_down(20, 19, 20));
    }

    #[test]
    fn history_keeps_one_value_per_month_and_drops_the_oldest() {
        let mut h = parse_history("202602=1,202601=50000, junk,202602=49900");
        assert_eq!(h, vec![(202601, 50000), (202602, 49900)]);
        record(&mut h, 202602, 49800);
        assert_eq!(h.last(), Some(&(202602, 49800)));
        for m in 1..=24 {
            record(&mut h, 202700 + m.min(12) + (m / 13) * 100, 40000);
        }
        assert!(h.len() <= HISTORY_MONTHS);
    }
}

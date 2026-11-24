//! Smart charging, GrandHonor's version of HONOR PC Manager's "Smart charge" / "AC long-duration
//! protection": a laptop that stays on the charger for hours wears its battery at 100 %. With the
//! switch on, after `HOLD_AFTER` on the charger GrandHonor holds the battery at 70–90 % (Office)
//! while the chosen mode is Unrestricted or Travel, and restores that mode when the charger is
//! removed. One sleeping thread waits for the hold time; nothing runs while on battery.

use crate::hw::battery::set_battery_mode;
use crate::settings;
use grandhonor_core::protocol::BatteryMode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const HOLD_AFTER: Duration = Duration::from_secs(3 * 60 * 60);
const HOLD_MODE: BatteryMode = BatteryMode::Office;

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The thresholds the firmware should have now: the hold while it is active, else the choice.
pub fn effective(preferred: BatteryMode) -> BatteryMode {
    if settings::full_charge_once() {
        BatteryMode::Disabled
    } else if settings::smart_charge_holding() {
        HOLD_MODE
    } else {
        preferred
    }
}

fn wants_hold(preferred: Option<BatteryMode>) -> bool {
    matches!(preferred, None | Some(BatteryMode::Disabled) | Some(BatteryMode::Travel))
}

/// Called when the charger is connected or removed (and once at start).
pub fn on_source(on_ac: bool) {
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    if !on_ac {
        release();
        return;
    }
    if !settings::smart_charge_enabled() || settings::smart_charge_holding() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("smart-charge".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            let start = Instant::now();
            // Sleep in long steps so a charger change or the switch going off ends the wait.
            while start.elapsed() < HOLD_AFTER {
                std::thread::sleep(Duration::from_secs(60));
                if GENERATION.load(Ordering::Acquire) != generation {
                    return;
                }
            }
            if !settings::smart_charge_enabled()
                || settings::full_charge_once()
                || !wants_hold(settings::preferred_battery_protection())
            {
                return;
            }
            match set_battery_mode(HOLD_MODE) {
                Ok(()) => {
                    settings::set_smart_charge_holding(true);
                    crate::log::info("Smart charging: holding the battery at 70-90% after 3 h on the charger");
                    crate::osd::show(
                        crate::osd::Icon::Battery,
                        grandhonor_core::locale::t(
                            "Умная зарядка: держим 70–90%",
                            "Smart charging: holding 70–90%",
                            "智能充电：保持 70–90%",
                        ),
                        None,
                    );
                }
                Err(err) => crate::log::error_with("Smart charging could not set the hold", &err),
            }
        });
}

/// Ends a hold and puts the chosen mode back (charger removed, switch turned off).
pub fn release() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    if !settings::smart_charge_holding() {
        return;
    }
    let preferred = settings::preferred_battery_protection().unwrap_or(BatteryMode::Disabled);
    match set_battery_mode(preferred) {
        Ok(()) => {
            settings::set_smart_charge_holding(false);
            crate::log::info("Smart charging: hold released");
        }
        Err(err) => crate::log::error_with("Smart charging could not restore the battery mode", &err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_full_charge_modes_are_held() {
        assert!(wants_hold(Some(BatteryMode::Disabled)));
        assert!(wants_hold(Some(BatteryMode::Travel)));
        assert!(!wants_hold(Some(BatteryMode::Home)));
        assert!(!wants_hold(Some(BatteryMode::Office)));
    }
}

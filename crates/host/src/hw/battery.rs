use super::com::ComGuard;
use super::sensors;
use super::wmi::WmiError;
use crate::settings;
use grandhonor_core::protocol::BatteryMode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const SET_THRESHOLDS_COMMAND: u64 = 0x0000_1003;
const CHECK_DELAY: Duration = Duration::from_secs(3);
const CHECK_COUNT: usize = 3;

pub fn set_battery_mode(mode: BatteryMode) -> Result<(), WmiError> {
    let _com = ComGuard::mta();
    let (start, end) = mode.thresholds();
    let mut session = super::wmi::HonorWmiSession::new();
    if mode == BatteryMode::Disabled {
        let cmd = SET_THRESHOLDS_COMMAND;
        session.call(cmd)?;
        thread::sleep(Duration::from_secs(1));
    }
    let cmd = SET_THRESHOLDS_COMMAND | ((start as u64) << 16) | ((end as u64) << 24);
    session.call(cmd)?;
    Ok(())
}

pub fn apply_battery_mode_and_save(mode: BatteryMode) -> Result<(), WmiError> {
    set_battery_mode(mode)?;
    settings::set_battery_protection(Some(mode));
    settings::set_preferred_battery_protection(Some(mode));
    // A choice made by hand ends a smart-charging hold.
    settings::set_smart_charge_holding(false);
    settings::set_full_charge_once(false);
    Ok(())
}

pub struct BatteryProtectionService {
    disposed: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    restore_busy: Arc<Mutex<()>>,
}

impl BatteryProtectionService {
    pub fn new() -> Self {
        Self {
            disposed: Arc::new(AtomicBool::new(false)),
            generation: Arc::new(AtomicU64::new(0)),
            restore_busy: Arc::new(Mutex::new(())),
        }
    }

    pub fn cancel_pending(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn restore_after_resume(&self, wake_gen_id: u64) {
        let Some(preferred) = settings::preferred_battery_protection() else {
            return;
        };
        let preferred = super::smart_charge::effective(preferred);
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        // Wait for an earlier restore instead of skipping this one; it re-checks the generation.
        let Ok(_lock) = self.restore_busy.lock() else {
            return;
        };
        if self.generation.load(Ordering::Acquire) != wake_gen_id {
            return;
        }

        let expected = preferred.thresholds();
        for _ in 0..CHECK_COUNT {
            for _ in 0..30 {
                if self.disposed.load(Ordering::Acquire)
                    || self.generation.load(Ordering::Acquire) != wake_gen_id
                {
                    return;
                }
                thread::sleep(CHECK_DELAY / 30);
            }
            if self.disposed.load(Ordering::Acquire)
                || self.generation.load(Ordering::Acquire) != wake_gen_id
            {
                return;
            }
            // Compare like with like: `preferred` is already the effective mode (a smart-charge
            // hold or "charge to 100 % once" replaces the stored choice).
            let still = settings::preferred_battery_protection().map(super::smart_charge::effective);
            if still != Some(preferred) {
                return;
            }
            if let Ok(snap) = sensors::read_and_store("resume-battery")
                && snap.charge_start == Some(expected.0 as i32)
                && snap.charge_end == Some(expected.1 as i32)
            {
                continue;
            }
            if self.generation.load(Ordering::Acquire) != wake_gen_id {
                return;
            }

            crate::log::info(&format!(
                "Charge thresholds were lost after resume, restoring {}",
                preferred.as_str()
            ));
            match set_battery_mode(preferred) {
                Ok(()) => settings::set_battery_protection(Some(preferred)),
                Err(err) => crate::log::error_with("Charge limit restore after resume failed", &err),
            }
        }
    }

    pub fn current_gen(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

impl Drop for BatteryProtectionService {
    fn drop(&mut self) {
        self.disposed.store(true, Ordering::Release);
        self.cancel_pending();
    }
}

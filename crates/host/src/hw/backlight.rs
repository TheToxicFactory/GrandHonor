use super::com::ComGuard;
use super::wmi::{HonorWmiSession, WmiError, honor_wmi_call};
use crate::settings;
use grandhonor_core::protocol::BacklightLevel;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::System::SystemInformation::GetLocalTime;

const SET_MODE_COMMAND: u64 = 0x0000_1406;
const SET_AUTO_LEVEL_COMMAND: u64 = 0x0000_1506;
const SET_TIMEOUT_COMMAND: u64 = 0x0000_1106;
const MODE_OFF: u64 = 0x02;
const MODE_LOW: u64 = 0x03;
const MODE_HIGH: u64 = 0x04;
const MODE_AUTO: u64 = 0x10;

pub fn set_backlight_level(level: BacklightLevel) -> Result<(), WmiError> {
    let _com = ComGuard::mta();
    let mut session = HonorWmiSession::new();
    set_level_with_session(&mut session, level)
}

pub fn set_backlight_timeout(seconds: u16) -> Result<(), WmiError> {
    honor_wmi_call(SET_TIMEOUT_COMMAND | ((seconds as u64) << 16))?;
    Ok(())
}

pub fn set_backlight_state(level: BacklightLevel, timeout_seconds: u16) -> Result<(), WmiError> {
    let _com = ComGuard::mta();
    let mut session = HonorWmiSession::new();
    set_level_with_session(&mut session, level)?;
    session.call(SET_TIMEOUT_COMMAND | ((timeout_seconds as u64) << 16))?;
    Ok(())
}

fn set_level_with_session(
    session: &mut HonorWmiSession,
    level: BacklightLevel,
) -> Result<(), WmiError> {
    let mode = match level {
        BacklightLevel::Off => MODE_OFF,
        BacklightLevel::Low => MODE_LOW,
        BacklightLevel::High => MODE_HIGH,
    };
    match session.call(SET_MODE_COMMAND | (mode << 16)) {
        Ok(_) => Ok(()),
        Err(err) if err.is_rejected() == Some(1) => {
            let brightness = match level {
                BacklightLevel::Off => 0u64,
                BacklightLevel::Low => 50u64,
                BacklightLevel::High => 100u64,
            };
            session.call(SET_MODE_COMMAND | (MODE_AUTO << 16))?;
            thread::sleep(Duration::from_millis(10));
            session.call(SET_AUTO_LEVEL_COMMAND | (brightness << 16))?;
            Ok(())
        }
        Err(err) => Err(err),
    }
}

#[derive(Default)]
struct ScheduleState {
    manual_override_until_epoch_s: Option<u64>,
    generation: u64,
}

pub struct BacklightScheduleService {
    disposed: Arc<AtomicBool>,
    wake_gen: Arc<AtomicU64>,
    state: Arc<(Mutex<ScheduleState>, Condvar)>,
    apply_lock: Arc<Mutex<()>>,
    worker: Option<JoinHandle<()>>,
}

impl BacklightScheduleService {
    pub fn start() -> Self {
        let disposed = Arc::new(AtomicBool::new(false));
        let wake_gen = Arc::new(AtomicU64::new(0));
        let state = Arc::new((Mutex::new(ScheduleState::default()), Condvar::new()));
        let apply_lock = Arc::new(Mutex::new(()));

        let d = disposed.clone();
        let st = state.clone();
        let al = apply_lock.clone();
        let worker = thread::Builder::new()
            .name("backlight-schedule".into())
            .spawn(move || {
                run_schedule_loop(d, st, al);
            })
            .ok();

        Self {
            disposed,
            wake_gen,
            state,
            apply_lock,
            worker,
        }
    }

    pub fn settings_changed(&self) {
        if let Ok(mut guard) = self.state.0.lock() {
            guard.manual_override_until_epoch_s = None;
            guard.generation = guard.generation.wrapping_add(1);
            self.state.1.notify_all();
        }
        let _ = apply_if_needed(&self.state, &self.apply_lock, true);
    }

    pub fn set_manual_override(&self) {
        if !settings::backlight_schedule_enabled() {
            return;
        }
        let until = local_now_secs() + secs_until_next_boundary();
        if let Ok(mut guard) = self.state.0.lock() {
            guard.manual_override_until_epoch_s = Some(until);
            guard.generation = guard.generation.wrapping_add(1);
            self.state.1.notify_all();
        }
    }

    pub fn cancel_wake(&self) {
        self.wake_gen.fetch_add(1, Ordering::AcqRel);
    }

    pub fn current_wake_gen(&self) -> u64 {
        self.wake_gen.load(Ordering::Acquire)
    }

    pub fn restore_after_resume(&self, wake_gen_id: u64) {
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut guard) = self.state.0.lock() {
            guard.manual_override_until_epoch_s = None;
        }
        for _ in 0..3 {
            for _ in 0..20 {
                if self.disposed.load(Ordering::Acquire)
                    || self.wake_gen.load(Ordering::Acquire) != wake_gen_id
                {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            if self.disposed.load(Ordering::Acquire)
                || self.wake_gen.load(Ordering::Acquire) != wake_gen_id
            {
                return;
            }
            let level = desired_level_now();
            let timeout = settings::keyboard_backlight_timeout();
            match set_backlight_state(level, timeout) {
                Ok(()) if self.wake_gen.load(Ordering::Acquire) == wake_gen_id => {
                    settings::set_keyboard_backlight(Some(level));
                }
                Ok(()) => return,
                Err(err) => crate::log::error_with("Backlight restore after resume failed", &err),
            }
        }
        if let Ok(mut guard) = self.state.0.lock() {
            guard.generation = guard.generation.wrapping_add(1);
            self.state.1.notify_all();
        }
    }
}

impl Drop for BacklightScheduleService {
    fn drop(&mut self) {
        self.disposed.store(true, Ordering::Release);
        self.cancel_wake();
        if let Ok(mut guard) = self.state.0.lock() {
            guard.generation = guard.generation.wrapping_add(1);
            self.state.1.notify_all();
        }
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

fn run_schedule_loop(
    disposed: Arc<AtomicBool>,
    state: Arc<(Mutex<ScheduleState>, Condvar)>,
    apply_lock: Arc<Mutex<()>>,
) {
    let _ = apply_if_needed(&state, &apply_lock, false);

    while !disposed.load(Ordering::Acquire) {
        let wait_duration = if settings::backlight_schedule_enabled() {
            let now = local_now_secs();
            let override_until = state
                .0
                .lock()
                .ok()
                .and_then(|g| g.manual_override_until_epoch_s);
            let secs = match override_until {
                Some(until) if until > now => until - now,
                _ => secs_until_next_boundary(),
            };
            Duration::from_secs(secs.max(1)).min(Duration::from_secs(3600))
        } else {
            Duration::from_secs(3600)
        };

        let Ok(guard) = state.0.lock() else { break };
        let initial_gen = guard.generation;
        if !settings::backlight_schedule_enabled() {
            // Schedule off: sleep until a setting changes, no timer.
            let guard = state.1.wait_while(guard, |g| g.generation == initial_gen).unwrap_or_else(|e| e.into_inner());
            drop(guard);
            continue;
        }
        let (guard, timed_out) = state.1.wait_timeout(guard, wait_duration).unwrap_or_else(|e| e.into_inner());
        let gen_changed = guard.generation != initial_gen;
        drop(guard);

        if disposed.load(Ordering::Acquire) {
            break;
        }
        if timed_out.timed_out() && !gen_changed && settings::backlight_schedule_enabled() {
            let _ = apply_if_needed(&state, &apply_lock, true);
        }
    }
}

fn apply_if_needed(
    state: &Arc<(Mutex<ScheduleState>, Condvar)>,
    apply_lock: &Arc<Mutex<()>>,
    force: bool,
) -> bool {
    let Ok(_lock) = apply_lock.try_lock() else {
        return false;
    };
    if !settings::backlight_schedule_enabled() {
        return false;
    }

    let now = local_now_secs();
    if let Ok(mut guard) = state.0.lock() {
        if let Some(until) = guard.manual_override_until_epoch_s
            && now < until
        {
            return false;
        }
        guard.manual_override_until_epoch_s = None;
    }

    let level = if should_be_on_now() {
        settings::backlight_schedule_level()
    } else {
        BacklightLevel::Off
    };

    if force || settings::keyboard_backlight() != Some(level) {
        match set_backlight_level(level) {
            Ok(()) => settings::set_keyboard_backlight(Some(level)),
            Err(err) => {
                crate::log::error_with("Backlight schedule hardware command failed", &err);
                return false;
            }
        }
    }
    true
}

fn desired_level_now() -> BacklightLevel {
    if settings::backlight_schedule_enabled() {
        if should_be_on_now() {
            settings::backlight_schedule_level()
        } else {
            BacklightLevel::Off
        }
    } else {
        settings::keyboard_backlight().unwrap_or(BacklightLevel::Off)
    }
}

fn should_be_on_now() -> bool {
    let st = unsafe { GetLocalTime() };
    let hour = st.wHour as u32;
    let on = settings::backlight_on_hour();
    let off = settings::backlight_off_hour();
    if on < off {
        hour >= on && hour < off
    } else {
        hour >= on || hour < off
    }
}

/// Seconds on a steady clock for override deadlines (UNIX time: monotonic across months and
/// years, unlike a sum of year, month and day).
fn local_now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn secs_until_next_boundary() -> u64 {
    let st = unsafe { GetLocalTime() };
    let cur_secs = (st.wHour as u64) * 3600 + (st.wMinute as u64) * 60 + (st.wSecond as u64);
    let on_secs = (settings::backlight_on_hour() as u64) * 3600;
    let off_secs = (settings::backlight_off_hour() as u64) * 3600;

    let next_on = if on_secs > cur_secs {
        on_secs - cur_secs
    } else {
        86_400 - cur_secs + on_secs
    };
    let next_off = if off_secs > cur_secs {
        off_secs - cur_secs
    } else {
        86_400 - cur_secs + off_secs
    };
    next_on.min(next_off).max(1)
}

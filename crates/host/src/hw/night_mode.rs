//! Power → Quiet nights: between two hours the Windows power mode is Efficiency (quiet fans,
//! cool and frugal); afterwards the mode from before comes back, unless it was changed by hand
//! during the night. One thread sleeps until the next boundary (at most an hour).

use crate::hw::power::{WINDOWS_BALANCED, WINDOWS_EFFICIENCY, WINDOWS_PERFORMANCE, set_windows_power_mode, windows_power_mode};
use crate::settings;
use grandhonor_core::locale::t;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows_core::GUID;

fn shared() -> &'static (Mutex<u64>, Condvar) {
    static S: OnceLock<(Mutex<u64>, Condvar)> = OnceLock::new();
    S.get_or_init(|| (Mutex::new(0), Condvar::new()))
}

fn code(mode: GUID) -> u32 {
    if mode == WINDOWS_EFFICIENCY { 1 } else if mode == WINDOWS_PERFORMANCE { 3 } else { 2 }
}

fn mode(code: u32) -> GUID {
    match code {
        1 => WINDOWS_EFFICIENCY,
        3 => WINDOWS_PERFORMANCE,
        _ => WINDOWS_BALANCED,
    }
}

/// Is `hour` inside [from, to), a window that may wrap past midnight (from == to: never).
pub fn in_window(hour: u32, from: u32, to: u32) -> bool {
    if from < to { hour >= from && hour < to } else if from > to { hour >= from || hour < to } else { false }
}

fn secs_until_boundary(from: u32, to: u32) -> u64 {
    let st = unsafe { GetLocalTime() };
    let now = u64::from(st.wHour) * 3600 + u64::from(st.wMinute) * 60 + u64::from(st.wSecond);
    [from, to]
        .iter()
        .map(|&h| {
            let at = u64::from(h) * 3600;
            if at > now { at - now } else { 86_400 - now + at }
        })
        .min()
        .unwrap_or(3600)
}

pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new().name("quiet-nights".into()).stack_size(128 * 1024).spawn(run);
    });
}

/// Settings changed or the laptop woke: check the window now.
pub fn poke() {
    let (lock, cv) = shared();
    if let Ok(mut g) = lock.lock() {
        *g += 1;
    }
    cv.notify_all();
}

fn on_ac() -> bool {
    crate::hw::battery_alerts::status().is_none_or(|(ac, _)| ac)
}

/// Brings the current power source in line: Efficiency inside the window (unless an app
/// profile holds Performance), the saved mode back outside it. Windows keeps one mode per
/// source, so each source has its own saved mode.
fn apply() {
    let (from, to) = settings::night_mode_hours();
    let hour = u32::from(unsafe { GetLocalTime() }.wHour);
    let source = crate::presets::source_key(on_ac());
    let want = settings::night_mode_enabled() && in_window(hour, from, to) && !crate::hw::app_profiles::active();
    let saved = settings::night_mode_saved(source);
    let Some(current) = windows_power_mode() else { return };
    if want && saved.is_none() {
        settings::set_night_mode_saved(source, Some(code(current)));
        if current != WINDOWS_EFFICIENCY {
            match set_windows_power_mode(WINDOWS_EFFICIENCY) {
                Ok(()) => {
                    let text = format!("{} {to:02}:00", t("Тихий режим до", "Quiet until", "安静模式至"));
                    crate::osd::notify(crate::osd::Icon::Normal, &text);
                }
                Err(err) => crate::log::error(&format!("Quiet nights: {err}")),
            }
        }
    } else if !want && let Some(before) = saved {
        settings::set_night_mode_saved(source, None);
        // Changed by hand during the night: leave the person's choice alone.
        if current == WINDOWS_EFFICIENCY
            && before != 1
            && let Err(err) = set_windows_power_mode(mode(before))
        {
            crate::log::error(&format!("Quiet nights restore: {err}"));
        }
    }
}

fn run() {
    let (lock, cv) = shared();
    let mut seen = 0;
    loop {
        apply();
        let busy = settings::night_mode_enabled()
            || settings::night_mode_saved("Ac").is_some()
            || settings::night_mode_saved("Dc").is_some();
        let Ok(guard) = lock.lock() else { return };
        let guard = if busy {
            // Until the next boundary (at most an hour, so a changed clock is caught too).
            let (from, to) = settings::night_mode_hours();
            let wait = Duration::from_secs(secs_until_boundary(from, to).clamp(1, 3600));
            cv.wait_timeout_while(guard, wait, |g| *g == seen).unwrap_or_else(|e| e.into_inner()).0
        } else {
            // Off and nothing to restore: sleep until the setting changes.
            cv.wait_while(guard, |g| *g == seen).unwrap_or_else(|e| e.into_inner())
        };
        seen = *guard;
    }
}

#[cfg(test)]
mod tests {
    use super::in_window;

    #[test]
    fn windows_wrap_past_midnight() {
        assert!(in_window(23, 23, 7));
        assert!(in_window(3, 23, 7));
        assert!(!in_window(7, 23, 7));
        assert!(!in_window(12, 23, 7));
        assert!(in_window(13, 12, 14));
        assert!(!in_window(5, 5, 5));
    }
}

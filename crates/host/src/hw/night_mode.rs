//! Power → Quiet nights: between two hours the Windows power mode is Efficiency (quiet fans,
//! cool and frugal); afterwards the mode from before comes back, unless it was changed by hand
//! during the night. One thread sleeps until the next boundary (at most an hour).

use super::power::{self, Profile, WINDOWS_EFFICIENCY, current_source, set_windows_mode_for_source, windows_mode_for_source};
use crate::settings;
use grandhonor_core::locale::t;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;
use windows::Win32::System::SystemInformation::GetLocalTime;

fn shared() -> &'static (Mutex<u64>, Condvar) {
    static S: OnceLock<(Mutex<u64>, Condvar)> = OnceLock::new();
    S.get_or_init(|| (Mutex::new(0), Condvar::new()))
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

pub(super) fn requested_now() -> bool {
    let (from, to) = settings::night_mode_hours();
    let hour = u32::from(unsafe { GetLocalTime() }.wHour);
    settings::night_mode_enabled() && in_window(hour, from, to)
}

fn apply() {
    if power::linked() {
        if let Some(applied) = power::reconcile(false) {
            power::notify_applied(applied);
        }
    } else {
        let _ = power::if_legacy(apply_legacy);
    }
}

/// Existing configurations retain the Windows-only behavior until their first linked choice.
fn apply_legacy() {
    let Some(ac) = current_source() else { return };
    let want = requested_now() && !crate::hw::app_profiles::active();
    if !want {
        for source in [true, false] {
            restore_legacy(source);
        }
        return;
    }
    let source = crate::presets::source_key(ac);
    let saved = settings::night_mode_saved(source);
    let Some(current) = windows_mode_for_source(ac) else { return };
    if saved.is_none() && current != WINDOWS_EFFICIENCY {
        match set_windows_mode_for_source(ac, WINDOWS_EFFICIENCY) {
            Ok(()) => {
                settings::set_night_mode_saved(source, Some(Profile::from_windows_mode(current) as u32));
                let (_, to) = settings::night_mode_hours();
                let text = format!("{} {to:02}:00", t("Экономия до", "Efficiency until", "能效至"));
                crate::osd::notify(crate::osd::Icon::Normal, &text);
            }
            Err(err) => crate::log::error(&format!("Quiet nights: {err}")),
        }
    }
}

fn restore_legacy(ac: bool) {
    let source = crate::presets::source_key(ac);
    let Some(before) = settings::night_mode_saved(source).and_then(Profile::from_code) else { return };
    let Some(current) = windows_mode_for_source(ac) else { return };
    if current == WINDOWS_EFFICIENCY
        && let Err(err) = set_windows_mode_for_source(ac, before.windows_mode())
    {
        crate::log::error(&format!("Quiet nights restore: {err}"));
        return;
    }
    settings::set_night_mode_saved(source, None);
}

fn run() {
    let (lock, cv) = shared();
    let mut seen = 0;
    loop {
        apply();
        let busy = settings::night_mode_enabled() || !power::linked() && (
            settings::night_mode_saved("Ac").is_some() || settings::night_mode_saved("Dc").is_some()
        );
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

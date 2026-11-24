//! Display → Eye breaks: every N minutes of work, an on-screen reminder to look into the
//! distance for 20 seconds (the 20-20-20 rule). Five idle minutes count as a break and restart
//! the count; a full-screen game, video or presentation postpones the reminder. One thread
//! that wakes once a minute only while the reminder is on and sleeps without a timer when it is off.

use crate::settings;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};

pub const CHOICES: [u32; 5] = [0, 20, 30, 45, 60];
const BREAK: Duration = Duration::from_secs(5 * 60);
const STEP: Duration = Duration::from_secs(60);

struct State {
    generation: u64,
}

fn shared() -> &'static (Mutex<State>, Condvar) {
    static S: OnceLock<(Mutex<State>, Condvar)> = OnceLock::new();
    S.get_or_init(|| (Mutex::new(State { generation: 0 }), Condvar::new()))
}

/// Time since the last keyboard, mouse or touchpad input.
fn idle() -> Duration {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return Duration::ZERO;
    }
    // dwTime is the low 32 bits of the tick count.
    let now = unsafe { GetTickCount64() } as u32;
    Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime)))
}

fn full_screen_busy() -> bool {
    unsafe { SHQueryUserNotificationState() }
        .is_ok_and(|s| s == QUNS_BUSY || s == QUNS_RUNNING_D3D_FULL_SCREEN || s == QUNS_PRESENTATION_MODE)
}

/// Starts the reminder thread once (it sleeps for an hour at a time while the reminder is off).
pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("eye-breaks".into())
            .stack_size(128 * 1024)
            .spawn(run);
    });
}

/// The interval changed or the laptop woke: start counting again.
pub fn settings_changed() {
    let (lock, cv) = shared();
    if let Ok(mut s) = lock.lock() {
        s.generation += 1;
    }
    cv.notify_all();
}

fn run() {
    let (lock, cv) = shared();
    let mut since = Instant::now();
    let mut seen = 0;
    loop {
        let minutes = settings::break_reminder_minutes();
        let interval = Duration::from_secs(u64::from(minutes) * 60);
        let Ok(guard) = lock.lock() else { return };
        let guard = if minutes == 0 {
            // Off: sleep until the setting changes.
            cv.wait_while(guard, |s| s.generation == seen).unwrap_or_else(|e| e.into_inner())
        } else {
            let wait = interval.saturating_sub(since.elapsed()).min(STEP).max(Duration::from_secs(1));
            cv.wait_timeout_while(guard, wait, |s| s.generation == seen).unwrap_or_else(|e| e.into_inner()).0
        };
        if guard.generation != seen {
            seen = guard.generation;
            since = Instant::now();
            continue;
        }
        drop(guard);
        if minutes == 0 {
            continue;
        }
        if idle() >= BREAK {
            since = Instant::now();
            continue;
        }
        if since.elapsed() >= interval {
            if full_screen_busy() {
                continue;
            }
            crate::osd::notify(
                crate::osd::Icon::Display,
                grandhonor_core::locale::t(
                    "Отдых для глаз: посмотрите вдаль 20 секунд",
                    "Eye break: look into the distance for 20 seconds",
                    "护眼休息：请远眺 20 秒",
                ));
            since = Instant::now();
        }
    }
}

//! Power → App profiles: while one of the chosen programs runs (a game, a video editor), the
//! Windows mode is Performance and the screen runs at its highest refresh rate; when the last
//! one closes, the mode and rate from before come back (unless changed by hand meanwhile).
//! Process start and stop arrive as WMI events (`Win32_ProcessStartTrace` / `StopTrace`, kernel
//! trace, no polling); with no program chosen there is no subscription at all.

use super::com::{ComGuard, connect_wmi, get_prop_string, get_prop_u64};
use crate::hw::power::{WINDOWS_PERFORMANCE, set_windows_power_mode, windows_power_mode};
use crate::settings;
use grandhonor_core::locale::t;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use std::time::Duration;
use windows::Win32::System::Wmi::{IWbemClassObject, IWbemObjectSink, IWbemObjectSink_Impl, WBEM_FLAG_SEND_STATUS};
use windows_core::{BSTR, GUID, HRESULT, Ref, implement};

const ROOT_CIMV2: &str = r"\\.\root\cimv2";

enum Event {
    Started(u32, String),
    Stopped(u32),
}

#[implement(IWbemObjectSink)]
struct Sink {
    sender: mpsc::Sender<Event>,
    failed: Arc<AtomicBool>,
}

impl IWbemObjectSink_Impl for Sink_Impl {
    fn Indicate(&self, count: i32, objects: *const Option<IWbemClassObject>) -> windows_core::Result<()> {
        if count <= 0 || objects.is_null() {
            return Ok(());
        }
        for obj in unsafe { std::slice::from_raw_parts(objects, count as usize) }.iter().flatten() {
            let class = get_prop_string(obj, "__CLASS").unwrap_or_default();
            let Some(pid) = get_prop_u64(obj, "ProcessID").map(|p| p as u32) else { continue };
            let event = if class.eq_ignore_ascii_case("Win32_ProcessStartTrace") {
                Event::Started(pid, get_prop_string(obj, "ProcessName").unwrap_or_default().to_ascii_lowercase())
            } else {
                Event::Stopped(pid)
            };
            let _ = self.sender.send(event);
        }
        Ok(())
    }

    fn SetStatus(&self, _flags: i32, hresult: HRESULT, _s: &BSTR, _o: Ref<'_, IWbemClassObject>) -> windows_core::Result<()> {
        if hresult.is_err() {
            self.failed.store(true, Ordering::Release);
            wake();
        }
        Ok(())
    }
}

fn shared() -> &'static (Mutex<u64>, Condvar) {
    static S: OnceLock<(Mutex<u64>, Condvar)> = OnceLock::new();
    S.get_or_init(|| (Mutex::new(0), Condvar::new()))
}

fn wake() {
    let (lock, cv) = shared();
    if let Ok(mut g) = lock.lock() {
        *g += 1;
    }
    cv.notify_all();
}

/// The list of programs changed: subscribe again (or stop when it is empty).
pub fn settings_changed() {
    wake();
}

pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new().name("app-profiles".into()).stack_size(256 * 1024).spawn(move || watch(tx));
        let _ = std::thread::Builder::new().name("app-profiles-apply".into()).stack_size(128 * 1024).spawn(move || dispatch(rx));
    });
}

fn watch(sender: mpsc::Sender<Event>) {
    let _com = ComGuard::mta();
    let (lock, cv) = shared();
    let mut seen = lock.lock().map(|g| *g).unwrap_or(0);
    loop {
        let mut retry = false;
        if !settings::app_profiles().is_empty() {
            let failed = Arc::new(AtomicBool::new(false));
            let sink: IWbemObjectSink = Sink { sender: sender.clone(), failed: failed.clone() }.into();
            let subscribed = connect_wmi(ROOT_CIMV2).and_then(|services| unsafe {
                let wql = BSTR::from("WQL");
                for query in ["SELECT ProcessID, ProcessName FROM Win32_ProcessStartTrace", "SELECT ProcessID FROM Win32_ProcessStopTrace"] {
                    services.ExecNotificationQueryAsync(&wql, &BSTR::from(query), WBEM_FLAG_SEND_STATUS, None, &sink)?;
                }
                Ok(services)
            });
            match subscribed {
                Ok(services) => {
                    let Ok(guard) = lock.lock() else { return };
                    let guard = cv.wait_while(guard, |g| *g == seen && !failed.load(Ordering::Acquire)).unwrap_or_else(|e| e.into_inner());
                    seen = *guard;
                    drop(guard);
                    unsafe {
                        let _ = services.CancelAsyncCall(&sink);
                    }
                    retry = failed.load(Ordering::Acquire);
                }
                Err(err) => {
                    crate::log::error(&format!("App profiles: could not watch programs: {err}"));
                    retry = true;
                }
            }
        }
        let Ok(guard) = lock.lock() else { return };
        let wait = if retry { Duration::from_secs(30) } else { Duration::from_secs(24 * 3600) };
        let (guard, _) = cv
            .wait_timeout_while(guard, wait, |g| *g == seen && (retry || settings::app_profiles().is_empty()))
            .unwrap_or_else(|e| e.into_inner());
        seen = *guard;
    }
}

/// What to put back when the last watched program closes.
struct Saved {
    mode: GUID,
    rate: Option<u32>,
}

fn dispatch(events: mpsc::Receiver<Event>) {
    let mut running: HashSet<u32> = HashSet::new();
    let mut saved: Option<Saved> = None;
    for event in events {
        match event {
            Event::Started(pid, name) => {
                if !settings::app_profiles().contains(&name) {
                    continue;
                }
                running.insert(pid);
                if saved.is_none() {
                    saved = Some(enter(&name));
                }
            }
            Event::Stopped(pid) => {
                if running.remove(&pid) && running.is_empty() && let Some(before) = saved.take() {
                    leave(before);
                }
            }
        }
    }
}

fn enter(name: &str) -> Saved {
    let mode = windows_power_mode().unwrap_or(crate::hw::power::WINDOWS_BALANCED);
    let rates = crate::hw::display::rates();
    if mode != WINDOWS_PERFORMANCE
        && let Err(err) = set_windows_power_mode(WINDOWS_PERFORMANCE)
    {
        crate::log::error(&format!("App profile: {err}"));
    }
    let mut rate = None;
    if let Some((list, current)) = rates
        && let Some(&max) = list.last()
        && max != current
        && crate::hw::display::set_refresh_rate(max)
    {
        rate = Some(current);
    }
    let program = name.trim_end_matches(".exe");
    crate::log::info(&format!("App profile on for {name}"));
    crate::osd::show(
        crate::osd::Icon::Performance,
        &format!("{program}: {}", t("производительность", "performance", "高性能")),
        None,
    );
    Saved { mode, rate }
}

fn leave(before: Saved) {
    if before.mode != WINDOWS_PERFORMANCE
        && windows_power_mode() == Some(WINDOWS_PERFORMANCE)
        && let Err(err) = set_windows_power_mode(before.mode)
    {
        crate::log::error(&format!("App profile restore: {err}"));
    }
    if let Some(hz) = before.rate {
        let _ = crate::hw::display::set_refresh_rate(hz);
    }
    crate::log::info("App profile off");
}

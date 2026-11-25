//! Power → App profiles: while one of the chosen programs runs (a game, a video editor), the
//! Windows mode is Performance and the screen runs at its highest refresh rate; when the last
//! one closes, the mode and rate from before come back (unless changed by hand meanwhile).
//! Process start and stop arrive as WMI events (`Win32_ProcessStartTrace` / `StopTrace`, kernel
//! trace, no polling). One subscription lives while the list is not empty; the running programs
//! are listed once at start, after a list change and after a lost subscription, so nothing is
//! missed. What to restore is also saved (`AppProfileSaved`), so a crash or restart mid-game
//! still puts the mode back.

use super::com::{ComGuard, connect_wmi, get_prop_string, get_prop_u64};
use crate::hw::power::{WINDOWS_BALANCED, WINDOWS_EFFICIENCY, WINDOWS_PERFORMANCE, set_windows_power_mode, windows_power_mode};
use crate::settings;
use grandhonor_core::locale::t;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Wmi::{IWbemClassObject, IWbemObjectSink, IWbemObjectSink_Impl, WBEM_FLAG_SEND_STATUS};
use windows_core::{BSTR, GUID, HRESULT, Ref, implement};

const ROOT_CIMV2: &str = r"\\.\root\cimv2";

/// True while a profile holds Performance (Quiet nights then stay out of the way).
static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

enum Event {
    Started(u32, String),
    Stopped(u32),
    /// List the running processes again (start, list changed, subscription renewed).
    Rescan,
}

/// WQL condition matching the listed names, full or cut to 15 characters as the trace reports.
fn name_filter(names: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for name in names {
        let quoted = |n: &str| format!("ProcessName = '{}'", n.replace('\\', "\\\\").replace('\'', "\\'"));
        parts.push(quoted(name));
        let cut: String = name.chars().take(15).collect();
        if cut != *name {
            parts.push(quoted(&cut));
        }
    }
    if parts.is_empty() { "ProcessID = 0".into() } else { parts.join(" OR ") }
}

/// The kernel trace reports image names cut to 15 characters ("cyberpunk2077.e").
fn name_matches(listed: &str, reported: &str) -> bool {
    listed == reported || (reported.len() >= 14 && listed.starts_with(reported))
}

fn listed(name: &str) -> bool {
    settings::app_profiles().iter().any(|l| name_matches(l, name))
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

static SENDER: OnceLock<Mutex<mpsc::Sender<Event>>> = OnceLock::new();

fn send(event: Event) {
    if let Some(tx) = SENDER.get().and_then(|m| m.lock().ok()) {
        let _ = tx.send(event);
    }
}

/// The list of programs changed: re-check what runs, subscribe or unsubscribe.
pub fn settings_changed() {
    send(Event::Rescan);
    wake();
}

pub fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let _ = SENDER.set(Mutex::new(tx.clone()));
        let _ = tx.send(Event::Rescan);
        let _ = std::thread::Builder::new().name("app-profiles".into()).stack_size(256 * 1024).spawn(move || watch(tx));
        let _ = std::thread::Builder::new().name("app-profiles-apply".into()).stack_size(128 * 1024).spawn(move || dispatch(rx));
    });
}

/// Keeps one subscription while the list is not empty; sleeps without a timeout otherwise.
fn watch(sender: mpsc::Sender<Event>) {
    let _com = ComGuard::mta();
    let (lock, cv) = shared();
    let mut seen = lock.lock().map(|g| *g).unwrap_or(0);
    loop {
        if settings::app_profiles().is_empty() {
            let Ok(guard) = lock.lock() else { return };
            let guard = cv.wait_while(guard, |g| *g == seen).unwrap_or_else(|e| e.into_inner());
            seen = *guard;
            continue;
        }
        let failed = Arc::new(AtomicBool::new(false));
        let sink: IWbemObjectSink = Sink { sender: sender.clone(), failed: failed.clone() }.into();
        let subscribed = connect_wmi(ROOT_CIMV2).and_then(|services| unsafe {
            let wql = BSTR::from("WQL");
            // Filtered in WMI: the host wakes only for the chosen programs, not every process.
            let filter = name_filter(&settings::app_profiles());
            for class in ["Win32_ProcessStartTrace", "Win32_ProcessStopTrace"] {
                let query = format!("SELECT ProcessID, ProcessName FROM {class} WHERE {filter}");
                services.ExecNotificationQueryAsync(&wql, &BSTR::from(query.as_str()), WBEM_FLAG_SEND_STATUS, None, &sink)?;
            }
            Ok(services)
        });
        let services = match subscribed {
            Ok(services) => services,
            Err(err) => {
                crate::log::error(&format!("App profiles: could not watch programs: {err}"));
                let Ok(guard) = lock.lock() else { return };
                let (guard, _) = cv
                    .wait_timeout_while(guard, std::time::Duration::from_secs(60), |g| *g == seen)
                    .unwrap_or_else(|e| e.into_inner());
                seen = *guard;
                continue;
            }
        };
        // Events from before the subscription are not delivered: look at what runs now.
        let _ = sender.send(Event::Rescan);
        // The query names the programs, so a list change, a failure or an empty list ends this
        // subscription and the loop subscribes again with the new list.
        let Ok(guard) = lock.lock() else { return };
        let guard = cv.wait_while(guard, |g| *g == seen && !failed.load(Ordering::Acquire)).unwrap_or_else(|e| e.into_inner());
        seen = *guard;
        drop(guard);
        unsafe {
            let _ = services.CancelAsyncCall(&sink);
        }
    }
}

/// Process ids of the chosen programs that run now.
fn running_listed() -> HashSet<u32> {
    let mut out = HashSet::new();
    let list = settings::app_profiles();
    if list.is_empty() {
        return out;
    }
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]).to_ascii_lowercase();
            if list.iter().any(|l| name_matches(l, &name)) {
                out.insert(entry.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// What to put back when the last watched program closes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Saved {
    mode: GUID,
    rate: Option<u32>,
}

fn mode_code(mode: GUID) -> u32 {
    if mode == WINDOWS_EFFICIENCY { 1 } else if mode == WINDOWS_PERFORMANCE { 3 } else { 2 }
}

fn code_mode(code: u32) -> GUID {
    match code {
        1 => WINDOWS_EFFICIENCY,
        3 => WINDOWS_PERFORMANCE,
        _ => WINDOWS_BALANCED,
    }
}

fn store(saved: Option<Saved>) {
    let text = saved.map(|s| format!("{},{}", mode_code(s.mode), s.rate.unwrap_or(0)));
    settings::set_app_profile_saved(text.as_deref());
}

fn load() -> Option<Saved> {
    let text = settings::app_profile_saved()?;
    let (mode, rate) = text.split_once(',')?;
    Some(Saved { mode: code_mode(mode.parse().ok()?), rate: rate.parse().ok().filter(|&r| r > 0) })
}

fn dispatch(events: mpsc::Receiver<Event>) {
    let mut running: HashSet<u32> = HashSet::new();
    let mut saved = load();
    ACTIVE.store(saved.is_some(), Ordering::Release);
    for event in events {
        match event {
            Event::Started(pid, name) => {
                if listed(&name) {
                    running.insert(pid);
                }
            }
            Event::Stopped(pid) => {
                running.remove(&pid);
            }
            Event::Rescan => running = running_listed(),
        }
        match (running.is_empty(), saved) {
            (false, None) => {
                let s = enter();
                store(Some(s));
                saved = Some(s);
                ACTIVE.store(true, Ordering::Release);
            }
            (true, Some(before)) => {
                leave(before);
                store(None);
                saved = None;
                ACTIVE.store(false, Ordering::Release);
                // Quiet nights may have been waiting for the game to end.
                crate::hw::night_mode::poke();
            }
            _ => {}
        }
    }
}

fn enter() -> Saved {
    let mode = windows_power_mode().unwrap_or(WINDOWS_BALANCED);
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
    crate::log::info("App profile on");
    crate::osd::notify(
        crate::osd::Icon::Performance,
        t("Профиль программы: производительность", "App profile: performance", "应用配置：高性能"));
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

#[cfg(test)]
mod tests {
    use super::name_matches;

    #[test]
    fn truncated_trace_names_still_match() {
        assert!(name_matches("game.exe", "game.exe"));
        assert!(name_matches("cyberpunk2077.exe", "cyberpunk2077.e"));
        assert!(!name_matches("cyberpunk2077.exe", "cyber"));
        assert!(!name_matches("game.exe", "other.exe"));
    }
}

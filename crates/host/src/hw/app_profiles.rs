//! Power → App profiles: while one of the chosen programs runs (a game, a video editor), the
//! Windows mode is Performance and the screen runs at its highest refresh rate; when the last
//! one closes, the mode and rate from before come back (unless changed by hand meanwhile).
//! Process start and stop arrive as WMI events (`Win32_ProcessStartTrace` / `StopTrace`, kernel
//! trace, no polling). One subscription lives while the list is not empty; the running programs
//! are listed once at start, after a list change and after a lost subscription, so nothing is
//! missed. What to restore is also saved (`AppProfileSaved`), so a crash or restart mid-game
//! still puts the mode back.

use super::com::{ComGuard, connect_wmi, get_prop_string, get_prop_u64};
use super::{display_hdr, power};
use crate::hw::power::{Profile, WINDOWS_PERFORMANCE, windows_mode_for_source};
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

/// True while at least one program requests the Fast profile.
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

fn watched_names() -> Vec<String> {
    let mut names = settings::app_profiles();
    names.extend(display_hdr::watched_programs());
    names.sort_unstable();
    names.dedup();
    names
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
    // The threads start with the first chosen program, not at sign-in.
    start();
    send(Event::Rescan);
    wake();
}

/// Starts watching when there is something to do: a chosen program, or a mode to restore
/// after a crash mid-game.
pub fn start_if_needed() {
    if !watched_names().is_empty() || load().is_some() {
        start();
    }
}

fn start() {
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
        let names = watched_names();
        if names.is_empty() {
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
            let filter = name_filter(&names);
            for class in ["Win32_ProcessStartTrace", "Win32_ProcessStopTrace"] {
                let query = format!("SELECT ProcessID, ProcessName FROM {class} WHERE {filter}");
                if let Err(err) = services.ExecNotificationQueryAsync(&wql, &BSTR::from(query.as_str()), WBEM_FLAG_SEND_STATUS, None, &sink) {
                    // The first query may already be running: cancel it before trying again.
                    let _ = services.CancelAsyncCall(&sink);
                    return Err(err);
                }
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
fn running_listed(power_names: &[String], hdr_names: &[String]) -> (HashSet<u32>, HashSet<u32>) {
    let mut power_running = HashSet::new();
    let mut hdr_running = HashSet::new();
    if power_names.is_empty() && hdr_names.is_empty() {
        return (power_running, hdr_running);
    }
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return (power_running, hdr_running);
        };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut entry).is_ok();
        while ok {
            let end = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]).to_ascii_lowercase();
            if power_names.iter().any(|l| l == &name) {
                power_running.insert(entry.th32ProcessID);
            }
            if hdr_names.iter().any(|l| l == &name) {
                hdr_running.insert(entry.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    (power_running, hdr_running)
}

/// What to put back when the last watched program closes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Saved {
    mode: Option<(bool, GUID)>,
    rate: Option<(u32, u32)>,
}

fn store(saved: Option<Saved>) {
    let text = saved.map(|s| {
        let (source, mode) = s.mode.map_or((0, 0), |(ac, mode)| {
            (if ac { 2 } else { 1 }, Profile::from_windows_mode(mode) as u32)
        });
        let (before, applied) = s.rate.unwrap_or_default();
        format!("{mode},{before},{source},{applied}")
    });
    settings::set_app_profile_state(text.as_deref());
    if settings::app_profile_saved().is_some() {
        settings::set_app_profile_saved(None);
    }
}

fn load() -> Option<Saved> {
    let text = settings::app_profile_state().or_else(settings::app_profile_saved)?;
    let mut fields = text.split(',');
    let profile = Profile::from_code(fields.next()?.parse().ok()?);
    let before = fields.next()?.parse::<u32>().ok()?;
    let source = match fields.next() {
        Some("1") => Some(false),
        Some("2") => Some(true),
        _ => None,
    };
    let applied = fields.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
    Some(Saved {
        // Older saves did not record the source or the applied refresh rate. Restoring
        // those blindly could overwrite a change made by the user or on the other source.
        mode: source.zip(profile.map(Profile::windows_mode)),
        rate: (before > 0 && applied > 0).then_some((before, applied)),
    })
}

pub(super) fn prior_mode(ac: bool) -> Option<GUID> {
    let (source, mode) = load()?.mode?;
    (source == ac && windows_mode_for_source(ac) == Some(WINDOWS_PERFORMANCE)).then_some(mode)
}

fn dispatch(events: mpsc::Receiver<Event>) {
    let mut running = HashSet::new();
    let mut running_hdr = HashSet::new();
    let mut hdr_active = None;
    let mut saved = load();
    for event in events {
        let power_names = settings::app_profiles();
        let hdr_names = display_hdr::watched_programs();
        match event {
            Event::Started(pid, name) => {
                if power_names.iter().any(|l| name_matches(l, &name)) {
                    running.insert(pid);
                }
                if hdr_names.iter().any(|l| name_matches(l, &name)) {
                    running_hdr.insert(pid);
                }
            }
            Event::Stopped(pid) => {
                running.remove(&pid);
                running_hdr.remove(&pid);
            }
            Event::Rescan => (running, running_hdr) = running_listed(&power_names, &hdr_names),
        }
        let hdr_now = !running_hdr.is_empty();
        if hdr_active != Some(hdr_now) {
            hdr_active = Some(hdr_now);
            display_hdr::program_state_changed(hdr_now);
        }
        let changed = ACTIVE.swap(!running.is_empty(), Ordering::AcqRel) != !running.is_empty();
        match (running.is_empty(), saved) {
            (false, None) => {
                let s = enter();
                store(Some(s));
                saved = Some(s);
            }
            (true, Some(before)) => {
                leave(before);
                store(None);
                saved = None;
            }
            _ => {}
        }
        if changed {
            if let Some(applied) = power::reconcile(false) {
                power::notify_applied(applied);
            }
            super::night_mode::poke();
        }
    }
}

fn enter() -> Saved {
    let rates = crate::hw::display::rates();
    let mode = power::begin_legacy_app_profile();
    let mut rate = None;
    if let Some((list, current)) = rates
        && let Some(&max) = list.last()
        && max != current
        && crate::hw::display::set_refresh_rate(max)
    {
        rate = Some((current, max));
    }
    crate::log::info("App profile on");
    if mode.is_some() {
        crate::osd::notify(
            crate::osd::Icon::Performance,
            t("Профиль программы: производительность", "App profile: performance", "应用配置：性能"));
    }
    Saved { mode, rate }
}

fn leave(before: Saved) {
    if let Some((source, mode)) = before.mode {
        power::restore_legacy_app_profile(source, mode);
    }
    if let Some((before, applied)) = before.rate
        && crate::hw::display::current_rate() == Some(applied)
    {
        let _ = crate::hw::display::set_refresh_rate(before);
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

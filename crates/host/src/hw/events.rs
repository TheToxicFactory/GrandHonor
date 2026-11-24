use super::com::{ComGuard, connect_wmi, get_prop_u64};
use crate::settings;
use grandhonor_core::protocol::BacklightLevel;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemObjectSink, IWbemObjectSink_Impl, IWbemServices,
    WBEM_FLAG_SEND_STATUS,
};
use windows_core::{BSTR, HRESULT, Ref, implement};

const ROOT_WMI: &str = r"\\.\root\wmi";
const MIC_MUTE_KEY_CODE: u32 = 0x287;
const CAMERA_KEY_CODE: u32 = 0x288;
const SCREENSHOT_KEY_CODE: u32 = 0x28E;
const HOTKEY_DEBOUNCE_MS: i64 = 400;
const POWER_MODE_DEBOUNCE_MS: i64 = 500;

#[derive(Clone, Copy, Debug)]
pub enum OemEvent {
    PowerModeChanged(bool),
    HunterModeActivated,
    KeyboardBacklightChanged,
    MicMutePressed,
    CameraKeyPressed,
    ScreenshotKeyPressed,
    /// Firmware charging notices (codes found in HONOR OSD_Daemon `SetFunction.SetMachineFunc`).
    Charger(ChargerNotice),
    /// 0x283 on / 0x2A3 off: the firmware toggled the touchpad (Fn key).
    TouchpadToggled(bool),
    /// 0x2EA on / 0x2EB off: Fn lock.
    FnLock(bool),
    /// 0x28C enabled / 0x28D disabled: the Windows key.
    WindowsKey(bool),
    /// 0x2A7: the refresh-rate key; the app switches the rate itself.
    RefreshRateKey,
    /// 0x2A5 / 0x2A6: the firmware refused Performance mode (no charger or low battery).
    PerformanceRefused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargerNotice {
    /// 0x290 / 0x291: a Thunderbolt device draws high power while on battery.
    ThunderboltOnBattery,
    /// 0x293 / 0x294: the charger is in a port that cannot charge.
    WrongPort,
    /// 0x296: the charger is not a standard (full-power) one.
    NonStandardCharger,
}

const CHARGER_NOTICE_DEBOUNCE_MS: i64 = 60_000;

#[implement(IWbemObjectSink)]
struct EventSink {
    sender: mpsc::Sender<u32>,
    error_flag: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    wake_notify: Arc<(Mutex<u64>, Condvar)>,
}

impl IWbemObjectSink_Impl for EventSink_Impl {
    fn Indicate(
        &self,
        lobjectcount: i32,
        apobjarray: *const Option<IWbemClassObject>,
    ) -> windows_core::Result<()> {
        if lobjectcount <= 0 || apobjarray.is_null() {
            return Ok(());
        }
        let slice = unsafe { std::slice::from_raw_parts(apobjarray, lobjectcount as usize) };
        for item in slice {
            let Some(obj) = item else { continue };
            if let Some(force) = get_prop_u64(obj, "Force") {
                let code = (force & 0xFFFF) as u32;
                let _ = self.sender.send(code);
            }
        }
        Ok(())
    }

    fn SetStatus(
        &self,
        _lflags: i32,
        hresult: HRESULT,
        _strparam: &BSTR,
        _pobjparam: Ref<'_, IWbemClassObject>,
    ) -> windows_core::Result<()> {
        if hresult.is_err()
            && !self.closing.load(Ordering::Acquire)
            && !self.error_flag.swap(true, Ordering::AcqRel)
        {
            crate::log::error(&format!(
                "OemWMIEvent sink status error: 0x{:08X}",
                hresult.0 as u32
            ));
            let (lock, cv) = &*self.wake_notify;
            if let Ok(_guard) = lock.lock() {
                cv.notify_all();
            }
        }
        Ok(())
    }
}

pub struct OemEventWatcher {
    stop: Arc<AtomicBool>,
    wake_notify: Arc<(Mutex<u64>, Condvar)>,
    watcher_thread: Option<JoinHandle<()>>,
    dispatch_thread: Option<JoinHandle<()>>,
}

impl OemEventWatcher {
    pub fn start<F, G>(
        should_ignore_backlight: G,
        on_event: F,
    ) -> Self
    where
        G: Fn() -> bool + Send + Sync + 'static,
        F: Fn(OemEvent) + Send + Sync + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let wake_notify = Arc::new((Mutex::new(0u64), Condvar::new()));
        let (tx, rx) = mpsc::channel::<u32>();

        let dispatch_stop = stop.clone();
        let dispatch_thread = thread::Builder::new()
            .name("oem-wmi-dispatch".into())
            .spawn(move || {
                let mut last_perf_time = -POWER_MODE_DEBOUNCE_MS;
                let mut last_mic_time = -HOTKEY_DEBOUNCE_MS;
                let mut last_cam_time = -HOTKEY_DEBOUNCE_MS;
                let mut last_snip_time = -HOTKEY_DEBOUNCE_MS;
                let mut last_charger: Option<(ChargerNotice, i64)> = None;
                let mut logged_codes = std::collections::HashSet::new();
                let start = std::time::Instant::now();

                let now_ms = || start.elapsed().as_millis() as i64;
                let accept = |last: &mut i64, now: i64| -> bool {
                    let prev = *last;
                    *last = now;
                    now - prev >= HOTKEY_DEBOUNCE_MS
                };

                while !dispatch_stop.load(Ordering::Acquire) {
                    // WMI may keep a sink (and its sender) alive after the subscription is
                    // cancelled, so never wait without a timeout: that blocked quitting.
                    let code = match rx.recv_timeout(Duration::from_millis(500)) {
                        Ok(code) => code,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if dispatch_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let now = now_ms();
                    match code {
                        MIC_MUTE_KEY_CODE => {
                            if accept(&mut last_mic_time, now) {
                                on_event(OemEvent::MicMutePressed);
                            }
                        }
                        CAMERA_KEY_CODE => {
                            if accept(&mut last_cam_time, now) {
                                on_event(OemEvent::CameraKeyPressed);
                            }
                        }
                        SCREENSHOT_KEY_CODE => {
                            if accept(&mut last_snip_time, now) {
                                on_event(OemEvent::ScreenshotKeyPressed);
                            }
                        }
                        0x2B1 | 0x2B2 | 0x2B3 => {
                            if should_ignore_backlight() {
                                continue;
                            }
                            let level = match code {
                                0x2B1 => BacklightLevel::Off,
                                0x2B2 => BacklightLevel::Low,
                                _ => BacklightLevel::High,
                            };
                            settings::set_keyboard_backlight(Some(level));
                            on_event(OemEvent::KeyboardBacklightChanged);
                        }
                        0x2A8 => {
                            settings::set_hunter_mode_active(true);
                            settings::set_performance_mode_active(false);
                            on_event(OemEvent::HunterModeActivated);
                        }
                        0x283 | 0x2A3 => on_event(OemEvent::TouchpadToggled(code == 0x283)),
                        0x2EA | 0x2EB => on_event(OemEvent::FnLock(code == 0x2EA)),
                        0x28C | 0x28D => on_event(OemEvent::WindowsKey(code == 0x28C)),
                        0x2A7 => {
                            if accept(&mut last_snip_time, now) {
                                on_event(OemEvent::RefreshRateKey);
                            }
                        }
                        0x2A5 | 0x2A6 => on_event(OemEvent::PerformanceRefused),
                        0x2A0 | 0x2A1 => {
                            if now - last_perf_time < POWER_MODE_DEBOUNCE_MS {
                                continue;
                            }
                            last_perf_time = now;
                            let current_perf = code == 0x2A1;
                            settings::set_hunter_mode_active(false);
                            settings::set_performance_mode_active(current_perf);
                            on_event(OemEvent::PowerModeChanged(current_perf));
                        }
                        0x290 | 0x291 | 0x293 | 0x294 | 0x296 => {
                            let notice = match code {
                                0x290 | 0x291 => ChargerNotice::ThunderboltOnBattery,
                                0x293 | 0x294 => ChargerNotice::WrongPort,
                                _ => ChargerNotice::NonStandardCharger,
                            };
                            // One notice per minute per kind: the firmware repeats them.
                            if !matches!(last_charger, Some((n, t)) if n == notice && now - t < CHARGER_NOTICE_DEBOUNCE_MS) {
                                last_charger = Some((notice, now));
                                on_event(OemEvent::Charger(notice));
                            }
                        }
                        other => {
                            // Unknown firmware events are logged once per run so new codes can be mapped.
                            if logged_codes.insert(other) {
                                crate::log::info(&format!("Unhandled OEM event 0x{other:03X}"));
                            }
                        }
                    }
                }
            })
            .ok();

        let watch_stop = stop.clone();
        let watch_notify = wake_notify.clone();
        let watcher_thread = thread::Builder::new()
            .name("oem-wmi-watcher".into())
            .spawn(move || {
                let _com = ComGuard::mta();
                run_watcher_loop(watch_stop, watch_notify, tx);
            })
            .ok();

        Self {
            stop,
            wake_notify,
            watcher_thread,
            dispatch_thread,
        }
    }

    pub fn request_reconnect(&self) {
        let (lock, cv) = &*self.wake_notify;
        if let Ok(mut gen_id) = lock.lock() {
            *gen_id = gen_id.wrapping_add(1);
            cv.notify_all();
        }
    }
}

impl Drop for OemEventWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.request_reconnect();
        if let Some(h) = self.watcher_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.dispatch_thread.take() {
            let _ = h.join();
        }
    }
}

fn run_watcher_loop(
    stop: Arc<AtomicBool>,
    wake_notify: Arc<(Mutex<u64>, Condvar)>,
    sender: mpsc::Sender<u32>,
) {
    let mut observed_gen = 0u64;
    let mut logged_once = false;

    while !stop.load(Ordering::Acquire) {
        let error_flag = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));
        let sink: IWbemObjectSink = EventSink {
            sender: sender.clone(),
            error_flag: error_flag.clone(),
            closing: closing.clone(),
            wake_notify: wake_notify.clone(),
        }
        .into();

        let subscription: Result<IWbemServices, String> = (|| {
            let services = connect_wmi(ROOT_WMI).map_err(|e| e.to_string())?;
            let lang = BSTR::from("WQL");
            let query = BSTR::from("SELECT * FROM OemWMIEvent");
            unsafe {
                services.ExecNotificationQueryAsync(
                    &lang,
                    &query,
                    WBEM_FLAG_SEND_STATUS,
                    None,
                    &sink,
                )
            }
            .map_err(|e| e.to_string())?;
            Ok(services)
        })();

        let retry = match subscription {
            Ok(services) => {
                logged_once = false;
                let (lock, cv) = &*wake_notify;
                let Ok(mut guard) = lock.lock() else { break };
                while !stop.load(Ordering::Acquire)
                    && !error_flag.load(Ordering::Acquire)
                    && *guard == observed_gen
                {
                    let (g, _) = cv.wait_timeout(guard, Duration::from_secs(30)).unwrap();
                    guard = g;
                }
                let requested = *guard != observed_gen;
                observed_gen = *guard;
                drop(guard);
                closing.store(true, Ordering::Release);
                unsafe {
                    let _ = services.CancelAsyncCall(&sink);
                }
                error_flag.load(Ordering::Acquire) && !requested
            }
            Err(err) => {
                if !logged_once {
                    crate::log::error(&format!("Could not start HONOR WMI event monitoring: {err}"));
                    logged_once = true;
                }
                true
            }
        };
        if retry {
            let (lock, cv) = &*wake_notify;
            let Ok(guard) = lock.lock() else { break };
            let Ok((guard, _)) = cv.wait_timeout_while(guard, Duration::from_secs(5), |generation| {
                !stop.load(Ordering::Acquire) && *generation == observed_gen
            }) else { break };
            observed_gen = *guard;
        }
    }
}

pub fn monotonic_ms() -> u64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let origin = ORIGIN.get_or_init(std::time::Instant::now);
    origin.elapsed().as_millis() as u64
}

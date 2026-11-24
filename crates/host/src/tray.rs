use crate::drivers::diagnostics::DriverPanel;
use crate::hw::audio::MicMuteService;
use crate::hw::backlight::BacklightScheduleService;
use crate::hw::battery::BatteryProtectionService;
use crate::hw::camera::toggle_camera;
use crate::hw::events::{OemEvent, OemEventWatcher, monotonic_ms};
use crate::hw::power::can_enable_performance_mode;
use crate::hw::touchpad::{
    TouchpadBrightnessService, invalidate_cache as invalidate_touchpad_cache,
    is_supported as touchpad_supported, is_supported_device_path, reapply_gestures,
    reapply_haptics,
};
use crate::hw::wmi::acpi_reconnect;
use crate::menu_builder::HostContext;
use crate::panel_host::PanelHost;
use crate::scheduler::migrate_installed_startup;
use crate::settings;
use crate::osd::Icon;
use grandhonor_core::locale::t;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use windows::Win32::Devices::HumanInterfaceDevice::HidD_GetHidGuid;
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, HBITMAP,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    UnregisterPowerSettingNotification,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW,
    RegQueryValueExW,
};
use windows::Win32::System::Threading::{INFINITE, SetEvent, WaitForSingleObject};

const GUID_CONSOLE_DISPLAY_STATE: windows_core::GUID =
    windows_core::GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE,
    DBT_DEVTYP_DEVICEINTERFACE, DEV_BROADCAST_DEVICEINTERFACE_W, DEV_BROADCAST_HDR,
    DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW,
    GetMessageW, GetSystemMetrics, HDEVNOTIFY, HICON, ICONINFO, KillTimer, MSG,
    PBT_APMPOWERSTATUSCHANGE, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
    PBT_POWERSETTINGCHANGE, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterDeviceNotificationW, RegisterWindowMessageW, SM_CXSMICON, SetTimer,
    TranslateMessage, UnregisterDeviceNotification, WM_APP, WM_CLOSE, WM_DESTROY, WM_DEVICECHANGE,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_POWERBROADCAST, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER,
    WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};
use windows_core::{HSTRING, PCWSTR};

const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
const WM_APP_OPEN_PANEL: u32 = WM_APP + 2;
const WM_APP_UPDATE_ICON: u32 = WM_APP + 3;
const WM_APP_NOTIFY: u32 = WM_APP + 4;
const WM_APP_QUIT: u32 = WM_APP + 5;
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const TIMER_DEVICE_CHANGE: usize = 1;
/// Retries adding the tray icon while the taskbar is not ready yet (early at sign-in).
const TIMER_TRAY_RETRY: usize = 2;
const TRAY_RETRY_MS: u32 = 2_000;

const RESUME_DEBOUNCE_MS: u64 = 10_000;
const RESUME_SETTLE_MS: u64 = 8_000;
const TRAY_ICON_ID: u32 = 1;

struct AppRuntime {
    hwnd: HWND,
    disposed: AtomicBool,
    backlight_schedule: Arc<BacklightScheduleService>,
    battery_service: Arc<BatteryProtectionService>,
    mic_mute: Arc<MicMuteService>,
    touchpad_service: Arc<TouchpadBrightnessService>,
    oem_watcher: Mutex<Option<OemEventWatcher>>,
    panel: PanelHost,
    ctx: HostContext,
    activation_event_raw: usize,
    last_resume_ms: AtomicU64,
    suppress_backlight_until_ms: AtomicU64,
    wake_generation: AtomicU64,
    taskbar_created_msg: u32,
    power_notify: Mutex<Option<HPOWERNOTIFY>>,
    hid_notify: Mutex<Option<HDEVNOTIFY>>,
    icon_cache: Mutex<HashMap<(i32, bool, bool), HICON>>,
    current_icon: Mutex<Option<HICON>>,
}

unsafe impl Send for AppRuntime {}
unsafe impl Sync for AppRuntime {}

static PENDING_NOTIFICATION: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();

fn notify_queue() -> &'static Mutex<Option<(String, String)>> {
    PENDING_NOTIFICATION.get_or_init(|| Mutex::new(None))
}

pub fn run_tray_loop(activation_event: HANDLE, open_panel_at_start: bool) -> i32 {
    settings::seed_preferred_battery_protection();

    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map(Into::into)
        .unwrap_or_default();
    let class_name = HSTRING::from("GrandHonorHostWindow");
    let taskbar_created = unsafe { RegisterWindowMessageW(&HSTRING::from("TaskbarCreated")) };

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    unsafe {
        RegisterClassExW(&wc);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            &class_name,
            &HSTRING::from("GrandHonor"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .unwrap_or_default();
    if hwnd.is_invalid() {
        return 1;
    }
    // GrandHonor runs elevated; User Interface Privilege Isolation drops messages from the
    // normal-integrity Explorer unless allowed. Without this, "TaskbarCreated" (sent when the
    // taskbar starts after sign-in or restarts) never arrives and the icon is never re-added.
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{ChangeWindowMessageFilterEx, MSGFLT_ALLOW};
        for message in [taskbar_created, WM_TRAY_CALLBACK] {
            if message != 0 {
                let _ = ChangeWindowMessageFilterEx(hwnd, message, MSGFLT_ALLOW, None);
            }
        }
    }

    let backlight_schedule = Arc::new(BacklightScheduleService::start());
    let battery_service = Arc::new(BatteryProtectionService::new());
    let mic_mute = Arc::new(MicMuteService::new());
    let touchpad_service = Arc::new(TouchpadBrightnessService::new());
    let driver_panel = DriverPanel::default();
    let last_sensor_ms = Arc::new(AtomicU64::new(0));

    let hwnd_raw = hwnd.0 as isize;
    let on_update_tray_icon: Arc<dyn Fn() + Send + Sync> = Arc::new(move || unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd_raw as *mut c_void)),
            WM_APP_UPDATE_ICON,
            WPARAM(0),
            LPARAM(0),
        );
    });
    let on_notify: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |title, body| {
        if !settings::notifications_enabled() {
            return;
        }
        if let Ok(mut q) = notify_queue().lock() {
            *q = Some((title.to_string(), body.to_string()));
        }
        unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd_raw as *mut c_void)),
                WM_APP_NOTIFY,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });
    let on_quit: Arc<dyn Fn() + Send + Sync> = Arc::new(move || unsafe {
        let _ = PostMessageW(
            Some(HWND(hwnd_raw as *mut c_void)),
            WM_APP_QUIT,
            WPARAM(0),
            LPARAM(0),
        );
    });

    let ctx = HostContext {
        tray_hwnd: hwnd_raw,
        backlight_schedule: backlight_schedule.clone(),
        driver_panel,
        on_update_tray_icon: on_update_tray_icon.clone(),
        on_notify: on_notify.clone(),
        on_quit,
        last_sensor_ms,
    };
    let panel = PanelHost::new(ctx.clone());

    let runtime = Arc::new(AppRuntime {
        hwnd,
        disposed: AtomicBool::new(false),
        backlight_schedule,
        battery_service,
        mic_mute,
        touchpad_service,
        oem_watcher: Mutex::new(None),
        panel,
        ctx,
        activation_event_raw: activation_event.0 as usize,
        last_resume_ms: AtomicU64::new(0),
        suppress_backlight_until_ms: AtomicU64::new(0),
        wake_generation: AtomicU64::new(0),
        taskbar_created_msg: taskbar_created,
        power_notify: Mutex::new(None),
        hid_notify: Mutex::new(None),
        icon_cache: Mutex::new(HashMap::new()),
        current_icon: Mutex::new(None),
    });

    let ptr_raw = Arc::into_raw(runtime.clone());
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            ptr_raw as isize,
        );
    }

    if !add_tray_icon(&runtime) {
        unsafe {
            SetTimer(Some(hwnd), TIMER_TRAY_RETRY, TRAY_RETRY_MS, None);
        }
    }
    // Explorer creates the icon's settings entry a moment after the icon is added.
    let _ = std::thread::Builder::new().name("tray-promote".into()).stack_size(128 * 1024).spawn(|| {
        let Ok(exe) = std::env::current_exe() else { return };
        for _ in 0..6 {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if settings::promote_tray_icon_once(&exe) {
                break;
            }
        }
    });
    register_notifications(&runtime);
    crate::hotkeys::register(hwnd);
    start_activation_listener(&runtime, activation_event);
    start_hardware_services(runtime.clone());

    // Start-up work (WMI connections, HID discovery, settings restore) finishes within
    // seconds; afterwards hand the pages it touched back to Windows. One-shot, not a timer.
    crate::footprint::enter_efficiency_mode();
    // A fresh copy of the user's settings at every start (see settings_transfer.rs).
    let _ = std::thread::Builder::new().name("settings-backup".into()).spawn(crate::settings_transfer::refresh_backup);
    crate::hw::cpu_limit::restore_original();
    crate::presets::start();
    let _ = std::thread::Builder::new()
        .name("startup-trim".into())
        .stack_size(64 * 1024)
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(20));
            crate::footprint::trim_working_set();
        });

    if open_panel_at_start {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_OPEN_PANEL, WPARAM(1), LPARAM(0));
        }
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    shutdown_runtime(&runtime);
    unsafe {
        let _ = Arc::from_raw(ptr_raw);
    }
    0
}

fn start_activation_listener(runtime: &Arc<AppRuntime>, activation_event: HANDLE) {
    let rt = runtime.clone();
    let ev_raw = activation_event.0 as usize;
    let _ = thread::Builder::new()
        .name("panel-activation".into())
        .spawn(move || {
            let ev = HANDLE(ev_raw as *mut c_void);
            while !rt.disposed.load(Ordering::Acquire) {
                let wait = unsafe { WaitForSingleObject(ev, INFINITE) };
                if wait.0 != 0 || rt.disposed.load(Ordering::Acquire) {
                    break;
                }
                unsafe {
                    let _ = PostMessageW(Some(rt.hwnd), WM_APP_OPEN_PANEL, WPARAM(1), LPARAM(0));
                }
            }
        });
}

fn start_hardware_services(rt: Arc<AppRuntime>) {
    thread::Builder::new()
        .name("hw-startup".into())
        .spawn(move || {
            if rt.disposed.load(Ordering::Acquire) {
                return;
            }
            migrate_installed_startup();
            rt.touchpad_service.start();
            let _ = touchpad_supported();
            reapply_haptics();
            reapply_gestures();
            rt.mic_mute.sync();
            crate::hw::display_filter::apply_saved();
            crate::hw::battery_alerts::on_power_status();
            crate::hw::battery_alerts::record_capacity();
            crate::hw::break_reminder::start();
            crate::hw::night_mode::start();
            crate::hw::app_profiles::start();
            // GrandHonor shows every HONOR notice itself: keep HONOR's own OSD service off so
            // nothing appears twice. Uninstalling GrandHonor turns it back on (main.rs).
            if crate::hw::honor_osd::installed() && !crate::hw::honor_osd::replaced() {
                match crate::hw::honor_osd::set_replaced(true) {
                    Ok(()) => {}
                    Err(err) => crate::log::error(&format!("Could not turn off HONOR OSD: {err}")),
                }
            }
            let rt_privacy = rt.clone();
            crate::hw::privacy_watch::start(move |device| capture_started(&rt_privacy, device));
            // The firmware forgets HONOR Performance on reboot; restore the saved choice.
            restore_performance(&rt);
            // Bring the firmware in line with the stored choices at every start (after an
            // update, a settings import or a crash), exactly as after waking from sleep.
            let rt_bl = rt.clone();
            let bl_gen = rt.backlight_schedule.current_wake_gen();
            let _ = thread::Builder::new().name("start-backlight".into())
                .spawn(move || rt_bl.backlight_schedule.restore_after_resume(bl_gen));
            let rt_bat = rt.clone();
            let bat_gen = rt.battery_service.current_gen();
            let _ = thread::Builder::new().name("start-battery".into())
                .spawn(move || rt_bat.battery_service.restore_after_resume(bat_gen));

            let rt_sup = rt.clone();
            let rt_ev = rt.clone();
            let watcher = OemEventWatcher::start(
                move || monotonic_ms() < rt_sup.suppress_backlight_until_ms.load(Ordering::Acquire),
                move |event| handle_oem_event(&rt_ev, event),
            );
            if rt.disposed.load(Ordering::Acquire) {
                drop(watcher);
                return;
            }
            if let Ok(mut guard) = rt.oem_watcher.lock() {
                *guard = Some(watcher);
            }
        })
        .ok();
}

fn handle_oem_event(rt: &Arc<AppRuntime>, event: OemEvent) {
    if rt.disposed.load(Ordering::Acquire) {
        return;
    }
    match event {
        OemEvent::PowerModeChanged(enabled) => {
            if crate::hw::power::is_software_echo() {
                // GrandHonor switched the HONOR profile itself; the firmware only confirms it.
                (rt.ctx.on_update_tray_icon)();
                return;
            }
            if enabled && let Err(reason) = can_enable_performance_mode() {
                disable_performance_mode(rt);
                crate::log::error(&reason);
            }
            cycle_profile(rt);
        }
        OemEvent::HunterModeActivated => {
            (rt.ctx.on_update_tray_icon)();
            notice(rt, Icon::Performance, t("Режим HUNTER", "HUNTER mode", "HUNTER 模式"), None);
        }
        OemEvent::KeyboardBacklightChanged => {
            rt.backlight_schedule.set_manual_override();
            let text = match settings::keyboard_backlight() {
                Some(grandhonor_core::protocol::BacklightLevel::Off) => t("Подсветка выключена", "Backlight off", "背光已关闭"),
                Some(grandhonor_core::protocol::BacklightLevel::Low) => t("Подсветка: слабая", "Backlight: weak", "背光：弱"),
                _ => t("Подсветка: сильная", "Backlight: strong", "背光：强"),
            };
            let (icon, level) = match settings::keyboard_backlight() {
                Some(grandhonor_core::protocol::BacklightLevel::Off) => (Icon::KeyboardOff, 0),
                Some(grandhonor_core::protocol::BacklightLevel::Low) => (Icon::KeyboardLight, 1),
                _ => (Icon::KeyboardLight, 2),
            };
            notice(rt, icon, text, Some((level, 2)));
        }
        OemEvent::TouchpadToggled(on) => {
            let text = if on { t("Тачпад включён", "Touchpad on", "触控板已开启") } else { t("Тачпад выключен", "Touchpad off", "触控板已关闭") };
            notice(rt, if on { Icon::Touchpad } else { Icon::TouchpadOff }, text, None);
        }
        OemEvent::FnLock(on) => {
            let text = if on { t("Fn Lock включён", "Fn Lock on", "Fn 锁定已开启") } else { t("Fn Lock выключен", "Fn Lock off", "Fn 锁定已关闭") };
            notice(rt, if on { Icon::Lock } else { Icon::Unlock }, text, None);
        }
        OemEvent::WindowsKey(enabled) => {
            let text = if enabled { t("Клавиша Windows включена", "Windows key on", "Windows 键已启用") } else { t("Клавиша Windows отключена", "Windows key off", "Windows 键已禁用") };
            notice(rt, if enabled { Icon::WindowsKey } else { Icon::WindowsKeyOff }, text, None);
        }
        OemEvent::RefreshRateKey => {
            let rt2 = rt.clone();
            thread::spawn(move || match crate::hw::display::toggle_refresh_rate() {
                Some(hz) => notice(&rt2, Icon::Display, &format!("{hz} {}", t("Гц", "Hz", "赫兹")), None),
                None => crate::log::info("Refresh-rate key: the display offers a single rate"),
            });
        }
        OemEvent::PerformanceRefused => {
            // Fn+P on battery: the firmware refuses its own profile, the Windows modes still cycle.
            cycle_profile(rt);
        }
        OemEvent::MicMutePressed => {
            if let Some(muted) = rt.mic_mute.toggle() {
                let title = if muted {
                    t("Микрофон выключен", "Microphone is off", "麦克风已关闭")
                } else {
                    t("Микрофон включён", "Microphone is on", "麦克风已开启")
                };
                let body = if muted {
                    t("Приложения не слышат звук.", "Apps receive no sound.", "应用程序听不到声音。")
                } else {
                    t(
                        "Звук снова доступен приложениям.",
                        "Sound is available to apps again.",
                        "应用程序可以再次获取声音。",
                    )
                };
                if settings::osd_enabled() {
                    crate::osd::show(if muted { Icon::MicOff } else { Icon::Mic }, title, None);
                } else {
                    (rt.ctx.on_notify)(title, body);
                }
            }
        }
        OemEvent::CameraKeyPressed => {
            if let Some(off) = toggle_camera() {
                let body = if off {
                    t(
                        "Запрошено отключение. Проверьте видеопоток.",
                        "Camera off requested. Check the video feed.",
                        "已请求关闭摄像头，请检查视频画面。",
                    )
                } else {
                    t(
                        "Запрошено включение.",
                        "Camera on requested.",
                        "已请求开启摄像头。",
                    )
                };
                if settings::osd_enabled() {
                    let text = if off { t("Камера выключена", "Camera off", "摄像头已关闭") } else { t("Камера включена", "Camera on", "摄像头已开启") };
                    crate::osd::show(if off { Icon::CameraOff } else { Icon::Camera }, text, None);
                } else {
                    (rt.ctx.on_notify)(
                        t("Переключатель камеры HONOR", "HONOR camera switch", "HONOR 摄像头开关"),
                        body,
                    );
                }
            }
        }
        OemEvent::ScreenshotKeyPressed => {
            // As the user: the protocol handler may come from HKCU (see unelevated.rs).
            crate::unelevated::open("ms-screenclip:");
        }
        OemEvent::Charger(notice) => {
            use crate::hw::events::ChargerNotice;
            crate::log::info(&format!("Charger notice from firmware: {notice:?}"));
            let (title, body) = match notice {
                ChargerNotice::ThunderboltOnBattery => (
                    t("Thunderbolt расходует батарею", "Thunderbolt is using the battery", "雷电接口正在消耗电池"),
                    t(
                        "Подключённое устройство питается от ноутбука. Подключите зарядку.",
                        "A connected device is powered by the laptop. Plug in the charger.",
                        "已连接的设备由笔记本供电，请接通电源。",
                    ),
                ),
                ChargerNotice::WrongPort => (
                    t("Зарядка в этом порту не работает", "This port does not charge", "此接口无法充电"),
                    t(
                        "Подключите зарядное устройство к левому порту.",
                        "Plug the charger into the port on the left.",
                        "请将充电器插入左侧接口。",
                    ),
                ),
                ChargerNotice::NonStandardCharger => (
                    t("Нестандартное зарядное устройство", "Non-standard charger", "非标准充电器"),
                    t(
                        "Зарядка и производительность могут быть ниже. Лучше использовать штатное зарядное устройство.",
                        "Charging and performance may be lower. The original charger is recommended.",
                        "充电和性能可能受影响，建议使用原装充电器。",
                    ),
                ),
            };
            let icon = if notice == ChargerNotice::WrongPort { Icon::Warning } else { Icon::Charger };
            crate::osd::show(icon, title, None);
            (rt.ctx.on_notify)(title, body);
        }
    }
}

/// Right-click on the tray icon: a small native menu with the switches people reach for most.
fn show_quick_menu(rt: &Arc<AppRuntime>, hwnd: HWND) {
    use crate::hw::power::{WINDOWS_BALANCED, WINDOWS_EFFICIENCY, WINDOWS_PERFORMANCE, windows_power_mode};
    use crate::settings::DisplayFilter;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_CHECKED, MF_SEPARATOR, MF_STRING,
        MF_UNCHECKED, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        TrackPopupMenu,
    };
    allow_dark_menus();
    let mode = windows_power_mode();
    let eye = settings::display_filter() != DisplayFilter::Off;
    let muted = rt.mic_mute.is_muted() == Some(true);
    let modes = [
        (1usize, WINDOWS_EFFICIENCY, t("Экономия", "Efficiency", "能效")),
        (2, WINDOWS_BALANCED, t("Баланс", "Balanced", "平衡")),
        (3, WINDOWS_PERFORMANCE, t("Производительность", "Performance", "性能")),
    ];
    let chosen = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        for (id, guid, label) in modes {
            let _ = AppendMenuW(menu, MF_STRING | check(mode == Some(guid)), id, &HSTRING::from(label));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING | check(eye), 10, &HSTRING::from(t("Защита зрения", "Eye comfort", "护眼")));
        let _ = AppendMenuW(menu, MF_STRING | check(muted), 11, &HSTRING::from(t("Микрофон выключен", "Microphone off", "麦克风已关闭")));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 20, &HSTRING::from(t("Открыть GrandHonor", "Open GrandHonor", "打开 GrandHonor")));
        let _ = AppendMenuW(menu, MF_STRING, 21, &HSTRING::from(t("Выход", "Quit", "退出")));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        cmd.0 as usize
    };
    match chosen {
        1..=3 => {
            let (_, guid, label) = modes[chosen - 1];
            match crate::hw::power::set_windows_power_mode(guid) {
                Ok(()) => {
                    (rt.ctx.on_update_tray_icon)();
                    let icon = match chosen { 1 => Icon::Normal, 3 => Icon::Performance, _ => Icon::Balanced };
                    notice(rt, icon, label, None);
                }
                Err(err) => crate::log::error(&format!("Quick menu power mode: {err}")),
            }
        }
        10 => {
            settings::set_display_filter(if eye { DisplayFilter::Off } else { DisplayFilter::EyeComfort });
            crate::hw::display_filter::apply_saved();
        }
        11 => handle_oem_event(rt, OemEvent::MicMutePressed),
        20 => rt.panel.toggle_or_open(true),
        21 => unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_QUIT, WPARAM(0), LPARAM(0));
        },
        _ => {}
    }
}

/// Lets Win32 menus follow the Windows dark theme (uxtheme ordinal 135, SetPreferredAppMode).
fn allow_dark_menus() {
    use std::sync::Once;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        if let Ok(module) = LoadLibraryW(windows_core::w!("uxtheme.dll"))
            && let Some(f) = GetProcAddress(module, windows_core::PCSTR(135 as *const u8))
        {
            let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(1); // AllowDark: follow the system theme
        }
    });
}

/// A remapped key (hotkeys.rs): runs its chosen action.
fn run_key_action(rt: &Arc<AppRuntime>, key: &'static str) {
    use crate::hotkeys::Action;
    match crate::hotkeys::action(key) {
        Action::Default => {}
        Action::Panel => rt.panel.toggle_or_open(true),
        Action::Screenshot => handle_oem_event(rt, OemEvent::ScreenshotKeyPressed),
        Action::Microphone => handle_oem_event(rt, OemEvent::MicMutePressed),
        Action::PowerMode => cycle_profile(rt),
        Action::EyeComfort => {
            use crate::settings::DisplayFilter;
            let on = settings::display_filter() == DisplayFilter::Off;
            settings::set_display_filter(if on { DisplayFilter::EyeComfort } else { DisplayFilter::Off });
            crate::hw::display_filter::apply_saved();
            let text = if on { t("Защита зрения включена", "Eye comfort on", "护眼已开启") } else { t("Защита зрения выключена", "Eye comfort off", "护眼已关闭") };
            notice(rt, Icon::Display, text, None);
        }
        Action::App => {
            // The path comes from a user-writable setting: start it as the user, never elevated.
            if let Some(path) = settings::key_app(key).filter(|p| crate::unelevated::is_launchable_program(p)) {
                crate::unelevated::open(&path);
            }
        }
    }
}

/// Fn+P: Economy → Balanced → Performance → Economy (see `hw::power::apply_profile`).
fn cycle_profile(rt: &Arc<AppRuntime>) {
    use crate::hw::power::{Profile, apply_profile, current_profile};
    let rt = rt.clone();
    thread::spawn(move || {
        let next = match current_profile() {
            Profile::Economy => Profile::Balanced,
            Profile::Balanced => Profile::Performance,
            Profile::Performance => Profile::Economy,
        };
        let honor_on = apply_profile(next);
        (rt.ctx.on_update_tray_icon)();
        let (icon, text) = match next {
            Profile::Economy => (Icon::Normal, t("Экономия", "Economy", "节能")),
            Profile::Balanced => (Icon::Balanced, t("Баланс", "Balanced", "平衡")),
            Profile::Performance if honor_on => (Icon::Performance, t("Производительный", "Performance", "高性能")),
            Profile::Performance => (Icon::Performance, t("Производительный · без зарядки", "Performance · on battery", "高性能 · 电池供电")),
        };
        notice(&rt, icon, text, None);
    });
}

/// An app started using the microphone or camera. If GrandHonor has it switched off, say how
/// to turn it on (HONOR OSD's "Press F7 to turn on the microphone"), at most once per 20 s.
fn capture_started(rt: &Arc<AppRuntime>, device: crate::hw::privacy_watch::Device) {
    use crate::hw::privacy_watch::Device;
    static LAST: Mutex<[Option<std::time::Instant>; 2]> = Mutex::new([None, None]);
    let (off, icon, text, slot) = match device {
        Device::Microphone => (
            rt.mic_mute.is_muted() == Some(true),
            Icon::MicOff,
            t("Микрофон выключен. Нажмите F7, чтобы включить", "Microphone is off. Press F7 to turn it on", "麦克风已关闭，按 F7 开启"),
            0,
        ),
        Device::Camera => (
            crate::hw::camera::get_camera_off() == Some(true),
            Icon::CameraOff,
            t("Камера выключена. Нажмите F8, чтобы включить", "Camera is off. Press F8 to turn it on", "摄像头已关闭，按 F8 开启"),
            1,
        ),
    };
    if !off || rt.disposed.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut last) = LAST.lock() {
        if last[slot].is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(20)) {
            return;
        }
        last[slot] = Some(std::time::Instant::now());
    }
    notice(rt, icon, text, None);
}

/// A short on-screen notice; when the on-screen display is off, a tray notification instead.
fn notice(rt: &Arc<AppRuntime>, icon: Icon, text: &str, level: Option<(u8, u8)>) {
    if settings::osd_enabled() {
        crate::osd::show(icon, text, level);
    } else {
        (rt.ctx.on_notify)("GrandHonor", text);
    }
}

fn disable_performance_mode(rt: &Arc<AppRuntime>) {
    if !settings::performance_mode_active() {
        return;
    }
    // Temporary (sleep, battery): the saved choice stays, so it comes back on its own.
    match crate::hw::power::suspend_performance() {
        Ok(()) => (rt.ctx.on_update_tray_icon)(),
        Err(err) => crate::log::error_with("Could not leave performance mode", &err),
    }
}

fn restore_performance(rt: &Arc<AppRuntime>) {
    if crate::hw::power::restore_performance_preference() {
        (rt.ctx.on_update_tray_icon)();
    }
}

fn trigger_wake_recovery(rt: &Arc<AppRuntime>) {
    if rt.disposed.load(Ordering::Acquire) {
        return;
    }
    let now = monotonic_ms();
    let prev = rt.last_resume_ms.load(Ordering::Acquire);
    if prev != 0 && now.saturating_sub(prev) < RESUME_DEBOUNCE_MS {
        return;
    }
    rt.last_resume_ms.store(now, Ordering::Release);
    rt.suppress_backlight_until_ms
        .store(now + RESUME_SETTLE_MS, Ordering::Release);

    let wake_gen = rt.wake_generation.fetch_add(1, Ordering::AcqRel) + 1;
    rt.backlight_schedule.cancel_wake();
    rt.battery_service.cancel_pending();
    let bl_gen = rt.backlight_schedule.current_wake_gen();
    let bat_gen = rt.battery_service.current_gen();

    let rt_wake = rt.clone();
    thread::Builder::new()
        .name("wake-recovery".into())
        .spawn(move || {
            for _ in 0..10 {
                if rt_wake.disposed.load(Ordering::Acquire)
                    || rt_wake.wake_generation.load(Ordering::Acquire) != wake_gen
                {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            if rt_wake.disposed.load(Ordering::Acquire)
                || rt_wake.wake_generation.load(Ordering::Acquire) != wake_gen
            {
                return;
            }

            acpi_reconnect();
            invalidate_touchpad_cache();
            rt_wake.touchpad_service.restart();
            reapply_haptics();
            reapply_gestures();
            rt_wake.mic_mute.sync();
            crate::hw::display_filter::apply_saved();
            crate::hw::battery_alerts::on_resume();
            crate::hw::battery_alerts::record_capacity();
            crate::hw::break_reminder::settings_changed();
            crate::hw::night_mode::poke();
            if let Ok(guard) = rt_wake.oem_watcher.lock()
                && let Some(watcher) = guard.as_ref()
            {
                watcher.request_reconnect();
            }
            // Sleep turned HONOR Performance off; bring back the saved choice if allowed.
            restore_performance(&rt_wake);

            let rt_bl = rt_wake.clone();
            let t_bl = thread::spawn(move || rt_bl.backlight_schedule.restore_after_resume(bl_gen));
            let rt_bat = rt_wake.clone();
            let t_bat = thread::spawn(move || rt_bat.battery_service.restore_after_resume(bat_gen));
            let _ = t_bl.join();
            let _ = t_bat.join();
        })
        .ok();
}

fn cancel_wake_recovery(rt: &Arc<AppRuntime>) {
    rt.wake_generation.fetch_add(1, Ordering::AcqRel);
    rt.backlight_schedule.cancel_wake();
    rt.battery_service.cancel_pending();
    rt.last_resume_ms.store(0, Ordering::Release);
}

fn register_notifications(rt: &Arc<AppRuntime>) {
    unsafe {
        if let Ok(h) = RegisterPowerSettingNotification(
            HANDLE(rt.hwnd.0),
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        ) && let Ok(mut guard) = rt.power_notify.lock()
        {
            *guard = Some(h);
        }

        let hid_guid = HidD_GetHidGuid();
        let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
            dbcc_size: std::mem::size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
            dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
            dbcc_reserved: 0,
            dbcc_classguid: hid_guid,
            dbcc_name: [0],
        };
        if let Ok(h) = RegisterDeviceNotificationW(
            HANDLE(rt.hwnd.0),
            &filter as *const _ as *const c_void,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        ) && let Ok(mut guard) = rt.hid_notify.lock()
        {
            *guard = Some(h);
        }
    }
}

fn shutdown_runtime(rt: &Arc<AppRuntime>) {
    rt.disposed.store(true, Ordering::Release);
    // Quitting must never hang (setup and updates wait for this process to end): whatever a
    // service thread is stuck on, the process ends 5 s after the window closed.
    let _ = thread::Builder::new().name("exit-watchdog".into()).stack_size(64 * 1024).spawn(|| {
        thread::sleep(Duration::from_secs(5));
        crate::log::error("Quit took longer than 5 s; ending the process");
        std::process::exit(0);
    });
    if rt.activation_event_raw != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(rt.activation_event_raw as *mut c_void));
        }
    }
    rt.panel.shutdown();
    cancel_wake_recovery(rt);
    if let Ok(mut guard) = rt.oem_watcher.lock() {
        *guard = None;
    }
    if let Ok(mut guard) = rt.power_notify.lock()
        && let Some(h) = guard.take()
    {
        unsafe {
            let _ = UnregisterPowerSettingNotification(h);
        }
    }
    if let Ok(mut guard) = rt.hid_notify.lock()
        && let Some(h) = guard.take()
    {
        unsafe {
            let _ = UnregisterDeviceNotification(h);
        }
    }
    delete_tray_icon(rt);
    if let Ok(mut cache) = rt.icon_cache.lock() {
        for (_, hicon) in cache.drain() {
            unsafe {
                let _ = DestroyIcon(hicon);
            }
        }
    }
    unsafe {
        let _ = DestroyWindow(rt.hwnd);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
        )
    } as *const AppRuntime;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let rt = unsafe { &*(ptr) };
    // Recover Arc clone only for helper methods needing Arc<AppRuntime>
    if msg == rt.taskbar_created_msg && rt.taskbar_created_msg != 0 {
        if !add_tray_icon(rt) {
            unsafe {
                SetTimer(Some(hwnd), TIMER_TRAY_RETRY, TRAY_RETRY_MS, None);
            }
        }
        return LRESULT(0);
    }

    match msg {
        WM_TRAY_CALLBACK => {
            let event = (lparam.0 & 0xFFFF) as u32;
            match event {
                WM_LBUTTONUP => {
                    rt.panel.toggle_or_open(false);
                }
                WM_RBUTTONUP => {
                    let rt_arc = unsafe {
                        Arc::increment_strong_count(ptr);
                        Arc::from_raw(ptr)
                    };
                    show_quick_menu(&rt_arc, hwnd);
                }
                WM_MOUSEMOVE => {
                    update_tray_tooltip(rt);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_OPEN_PANEL => {
            rt.panel.toggle_or_open(wparam.0 != 0);
            LRESULT(0)
        }
        WM_APP_UPDATE_ICON => {
            update_tray_icon(rt);
            LRESULT(0)
        }
        WM_APP_NOTIFY => {
            if let Ok(mut q) = notify_queue().lock()
                && let Some((title, body)) = q.take()
            {
                show_tray_balloon(rt, &title, &body);
            }
            LRESULT(0)
        }
        crate::hotkeys::WM_APP_KEYS => {
            crate::hotkeys::register(hwnd);
            LRESULT(0)
        }
        crate::hotkeys::WM_APP_KEY_PRESSED => {
            let rt_arc = unsafe {
                Arc::increment_strong_count(ptr);
                Arc::from_raw(ptr)
            };
            if let Some(key) = crate::hotkeys::KEYS.get(wparam.0) {
                run_key_action(&rt_arc, key);
            }
            LRESULT(0)
        }
        WM_APP_QUIT => {
            rt.disposed.store(true, Ordering::Release);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED => {
            if let Ok(mut cache) = rt.icon_cache.lock() {
                for (_, hicon) in cache.drain() {
                    unsafe {
                        let _ = DestroyIcon(hicon);
                    }
                }
            }
            if let Ok(mut cur) = rt.current_icon.lock() {
                *cur = None;
            }
            update_tray_icon(rt);
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            let rt_arc = unsafe {
                Arc::increment_strong_count(ptr);
                Arc::from_raw(ptr)
            };
            match wparam.0 as u32 {
                PBT_APMSUSPEND => {
                    crate::hw::battery_alerts::on_suspend();
                    cancel_wake_recovery(&rt_arc);
                    let rt_dis = rt_arc.clone();
                    thread::spawn(move || disable_performance_mode(&rt_dis));
                }
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                    trigger_wake_recovery(&rt_arc);
                }
                PBT_APMPOWERSTATUSCHANGE => {
                    crate::presets::on_power_status_change();
                    thread::spawn(crate::hw::battery_alerts::on_power_status);
                    let rt_pw = rt_arc.clone();
                    if can_enable_performance_mode().is_err() {
                        thread::spawn(move || disable_performance_mode(&rt_pw));
                    } else {
                        thread::spawn(move || restore_performance(&rt_pw));
                    }
                }
                PBT_POWERSETTINGCHANGE if lparam.0 != 0 => {
                    let setting = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
                    if setting.PowerSetting == GUID_CONSOLE_DISPLAY_STATE
                        && setting.DataLength >= 4
                        && setting.Data[0] == 1
                    {
                        trigger_wake_recovery(&rt_arc);
                    }
                }
                _ => {}
            }
            LRESULT(1)
        }
        WM_DEVICECHANGE if lparam.0 != 0 => {
            let ev_type = wparam.0 as u32;
            if ev_type == DBT_DEVICEARRIVAL || ev_type == DBT_DEVICEREMOVECOMPLETE {
                let hdr = unsafe { &*(lparam.0 as *const DEV_BROADCAST_HDR) };
                if hdr.dbch_devicetype == DBT_DEVTYP_DEVICEINTERFACE {
                    let name_ptr =
                        (lparam.0 as usize + std::mem::offset_of!(DEV_BROADCAST_DEVICEINTERFACE_W, dbcc_name))
                            as *const u16;
                    let mut len = 0usize;
                    unsafe {
                        let header_size = std::mem::offset_of!(DEV_BROADCAST_DEVICEINTERFACE_W, dbcc_name);
                        let max_chars = (hdr.dbch_size as usize).saturating_sub(header_size) / 2;
                        while len < max_chars.min(512) && *name_ptr.add(len) != 0 {
                            len += 1;
                        }
                        let path = String::from_utf16_lossy(std::slice::from_raw_parts(name_ptr, len));
                        if is_supported_device_path(&path) {
                            let _ = KillTimer(Some(hwnd), TIMER_DEVICE_CHANGE);
                            let _ = SetTimer(Some(hwnd), TIMER_DEVICE_CHANGE, 1_000, None);
                        }
                    }
                }
            }
            LRESULT(1)
        }
        WM_TIMER if wparam.0 == TIMER_TRAY_RETRY => {
            let rt_arc = unsafe {
                Arc::increment_strong_count(ptr);
                Arc::from_raw(ptr)
            };
            if add_tray_icon(&rt_arc) {
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_TRAY_RETRY);
                }
                crate::log::info("Tray icon added after the taskbar became ready");
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_DEVICE_CHANGE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_DEVICE_CHANGE);
            }
            let rt_arc = unsafe {
                Arc::increment_strong_count(ptr);
                Arc::from_raw(ptr)
            };
            thread::spawn(move || {
                if rt_arc.disposed.load(Ordering::Acquire) {
                    return;
                }
                invalidate_touchpad_cache();
                rt_arc.touchpad_service.restart();
                reapply_haptics();
                reapply_gestures();
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            rt.disposed.store(true, Ordering::Release);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn update_tray_tooltip(rt: &AppRuntime) {
    let (tip_text, _) = crate::hw::sensors::build_telemetry(true);
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: rt.hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_TIP,
        ..Default::default()
    };
    write_wide(&mut nid.szTip, &tip_text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }

    let ctx = rt.ctx.clone();
    thread::spawn(move || {
        crate::menu_builder::refresh_sensors_throttled(&ctx, 5_000);
    });
}

/// Adds (or, if Explorer still has it, refreshes) the tray icon. False when the taskbar is not
/// there yet; the caller then retries.
fn add_tray_icon(rt: &AppRuntime) -> bool {
    let perf = settings::performance_mode_active() || settings::hunter_mode_active();
    let hicon = get_or_render_icon(rt, perf);
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: rt.hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY_CALLBACK,
        hIcon: hicon,
        ..Default::default()
    };
    write_wide(&mut nid.szTip, "GrandHonor");
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &nid).as_bool() || Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() };
    if let Ok(mut cur) = rt.current_icon.lock() {
        *cur = Some(hicon);
    }
    ok
}

fn update_tray_icon(rt: &AppRuntime) {
    let perf = settings::performance_mode_active() || settings::hunter_mode_active();
    let hicon = get_or_render_icon(rt, perf);
    if let Ok(mut cur) = rt.current_icon.lock() {
        if *cur == Some(hicon) {
            return;
        }
        *cur = Some(hicon);
    }
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: rt.hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_ICON,
        hIcon: hicon,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn delete_tray_icon(rt: &AppRuntime) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: rt.hwnd,
        uID: TRAY_ICON_ID,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn show_tray_balloon(rt: &AppRuntime, title: &str, body: &str) {
    if !settings::notifications_enabled() {
        return;
    }
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: rt.hwnd,
        uID: TRAY_ICON_ID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    write_wide(&mut nid.szInfoTitle, title);
    write_wide(&mut nid.szInfo, body);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn write_wide(buf: &mut [u16], text: &str) {
    buf.fill(0);
    let max = buf.len().saturating_sub(1);
    for (i, ch) in text.encode_utf16().take(max).enumerate() {
        buf[i] = ch;
    }
}

fn is_dark_theme() -> bool {
    let subkey = HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let mut key = windows::Win32::System::Registry::HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, Some(0), KEY_READ, &mut key) }.0 != 0 {
        return true;
    }
    let read_val = |name: &str| -> Option<u32> {
        let val_name = HSTRING::from(name);
        let mut ty = REG_VALUE_TYPE::default();
        let mut val = 0u32;
        let mut len = 4u32;
        let st = unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(val_name.as_ptr()),
                None,
                Some(&mut ty),
                Some(&mut val as *mut u32 as *mut u8),
                Some(&mut len),
            )
        };
        if st.0 == 0 && ty == REG_DWORD {
            Some(val)
        } else {
            None
        }
    };
    let light = read_val("SystemUsesLightTheme").or_else(|| read_val("AppsUseLightTheme"));
    unsafe {
        let _ = RegCloseKey(key);
    }
    light == Some(0)
}

fn read_system_accent_rgb() -> (u8, u8, u8) {
    let subkey = HSTRING::from(r"Software\Microsoft\Windows\DWM");
    let mut key = windows::Win32::System::Registry::HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, Some(0), KEY_READ, &mut key) }.0 == 0 {
        let val_name = HSTRING::from("AccentColor");
        let mut ty = REG_VALUE_TYPE::default();
        let mut val = 0u32;
        let mut len = 4u32;
        let st = unsafe {
            RegQueryValueExW(
                key,
                PCWSTR(val_name.as_ptr()),
                None,
                Some(&mut ty),
                Some(&mut val as *mut u32 as *mut u8),
                Some(&mut len),
            )
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        if st.0 == 0 && ty == REG_DWORD {
            // DWM AccentColor is stored as ABGR
            let r = (val & 0xFF) as u8;
            let g = ((val >> 8) & 0xFF) as u8;
            let b = ((val >> 16) & 0xFF) as u8;
            return (r, g, b);
        }
    }
    (0x52, 0x94, 0xE2)
}

fn get_or_render_icon(rt: &AppRuntime, perf: bool) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let dark = is_dark_theme();
    if let Ok(mut cache) = rt.icon_cache.lock() {
        if let Some(&icon) = cache.get(&(size, dark, perf)) {
            return icon;
        }
        let icon = render_monochrome_icon(size, dark, perf);
        cache.insert((size, dark, perf), icon);
        return icon;
    }
    render_monochrome_icon(size, dark, perf)
}

/// The GrandHonor mark for the tray: a graphite squircle (superellipse, n = 5) shaded from a
/// lighter top to a deep bottom, a lit top rim and a strict white H with pixel-snapped stems.
/// Same geometry as `Assets/mark.ps1` (the application icon). Performance mode draws the rim in the accent colour.
fn render_monochrome_icon(size: i32, dark: bool, perf: bool) -> HICON {
    let (acc_r, acc_g, acc_b) = read_system_accent_rgb();
    let s = size as f32;
    let half = s * 0.46;
    let rim_w = (s * 0.06).max(1.0);
    // Pixel-snapped H: stems and bar land on whole pixels at every tray size.
    let t = (s * 0.105).round().max(2.0);
    let left = (s * 0.305).round();
    let right = (s * 0.695).round() - t;
    let top = (s * 0.285).round();
    let bottom = (s * 0.715).round();
    let bar_top = ((top + bottom - t) / 2.0).round();
    let inside = |x: f32, y: f32, a: f32| {
        let (u, v) = (((x - s * 0.5) / a).abs(), ((y - s * 0.5) / a).abs());
        u.powi(5) + v.powi(5) <= 1.0
    };

    let mut pixels = vec![0u32; (size * size) as usize];
    let sub = 4;
    for py in 0..size {
        for px in 0..size {
            let (mut cr, mut cg, mut cb, mut ca) = (0f32, 0f32, 0f32, 0f32);
            for sy in 0..sub {
                for sx in 0..sub {
                    let x = px as f32 + (sx as f32 + 0.5) / sub as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / sub as f32;
                    if !inside(x, y, half) {
                        continue;
                    }
                    let k = (y / s).clamp(0.0, 1.0);
                    // Graphite gradient: #3A3A3F at the top to #121214 at the bottom.
                    let mut col = [58.0 + (18.0 - 58.0) * k, 58.0 + (18.0 - 58.0) * k, 63.0 + (20.0 - 63.0) * k];
                    let on_h = (y >= top && y < bottom && ((x >= left && x < left + t) || (x >= right && x < right + t)))
                        || (y >= bar_top && y < bar_top + t && x >= left && x < right + t);
                    if on_h {
                        col = [245.0, 245.0, 247.0];
                    } else if !inside(x, y, half - rim_w) {
                        if perf {
                            col = [acc_r as f32, acc_g as f32, acc_b as f32];
                        } else {
                            // Lit edge: bright at the top, fading out by the middle.
                            let glow = (1.0 - k * 1.8).clamp(0.0, 1.0) * if dark { 0.42 } else { 0.30 };
                            for c in &mut col {
                                *c += (255.0 - *c) * glow;
                            }
                        }
                    }
                    cr += col[0];
                    cg += col[1];
                    cb += col[2];
                    ca += 255.0;
                }
            }
            let n = (sub * sub) as f32;
            let a = (ca / n).round().clamp(0.0, 255.0) as u32;
            // Premultiplied BGRA: each channel is the sum over covered samples / all samples.
            let r = (cr / n).round().clamp(0.0, 255.0) as u32;
            let g = (cg / n).round().clamp(0.0, 255.0) as u32;
            let b = (cb / n).round().clamp(0.0, 255.0) as u32;
            pixels[(py * size + px) as usize] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }

    create_icon_from_bgra(size, &pixels)
}

fn create_icon_from_bgra(size: i32, bgra: &[u32]) -> HICON {
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let hdc = CreateCompatibleDC(None);
        let mut bits: *mut c_void = ptr::null_mut();
        let color_bmp: HBITMAP = CreateDIBSection(
            Some(hdc),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .unwrap_or_default();
        if !bits.is_null() {
            ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u32, bgra.len());
        }
        let mask_bmp = windows::Win32::Graphics::Gdi::CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bmp,
            hbmColor: color_bmp,
        };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        if !mask_bmp.is_invalid() {
            let _ = DeleteObject(mask_bmp.into());
        }
        if !color_bmp.is_invalid() {
            let _ = DeleteObject(color_bmp.into());
        }
        let _ = DeleteDC(hdc);
        icon
    }
}

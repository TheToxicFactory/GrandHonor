use crate::osd::Icon;
use crate::drivers::diagnostics::DriverPanel;
use crate::hw::backlight::{
    BacklightScheduleService, set_backlight_level, set_backlight_timeout,
};
use crate::hw::battery::apply_battery_mode_and_save;
use crate::hw::power::{
    WINDOWS_BALANCED, WINDOWS_EFFICIENCY, WINDOWS_PERFORMANCE, apply_power_unlock_and_save,
    can_enable_performance_mode, set_windows_power_mode, windows_power_mode,
};
use crate::hw::sensors::{build_telemetry, read_and_store};
use crate::hw::touchpad::{
    apply_gesture, apply_haptics, is_gesture_enabled, is_supported as touchpad_supported,
};
use crate::scheduler::{is_installation_path, is_startup_enabled, set_startup_enabled};
use crate::settings;
use grandhonor_core::locale::{AppLanguage, t};
use grandhonor_core::protocol::{
    BacklightLevel, BatteryMode, ControlItem, PanelState, TouchpadEdgeGesture, TouchpadHaptics,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
};
use windows_core::HSTRING;

pub type ActionFn = Box<dyn Fn() + Send + Sync>;

pub struct MenuBuilder {
    items: Vec<ControlItem>,
    commands: Arc<Mutex<HashMap<u32, ActionFn>>>,
    next_id: Arc<Mutex<u32>>,
}

impl MenuBuilder {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            commands: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
        }
    }

    pub fn add_submenu_ex(
        &mut self,
        text: &str,
        help: Option<&str>,
        badge: Option<&str>,
        kind: Option<&str>,
    ) -> SubMenu<'_> {
        let idx = self.items.len();
        self.items.push(ControlItem {
            text: text.to_string(),
            help: help.map(str::to_string),
            badge: badge.map(str::to_string),
            command: None,
            selected: false,
            children: Some(Vec::new()),
            kind: kind.map(str::to_string),
        });
        SubMenu {
            parent_item: &mut self.items[idx],
            commands: self.commands.clone(),
            next_id: self.next_id.clone(),
        }
    }

    pub fn add_submenu(&mut self, text: &str, help: Option<&str>, kind: Option<&str>) -> SubMenu<'_> {
        self.add_submenu_ex(text, help, None, kind)
    }

    pub fn build_into(
        self,
        status: String,
        telemetry: grandhonor_core::protocol::TelemetrySummary,
    ) -> (PanelState, HashMap<u32, ActionFn>) {
        // "HONOR MagicBook · ZQC-P": the marketing family plus the model code, not the
        // internal codename (DeviceTypeEx) that means nothing to people.
        let bios_key = r"HARDWARE\DESCRIPTION\System\BIOS";
        let clean = |s: String| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        let family = settings::read_hklm_string(bios_key, "SystemFamily").and_then(clean);
        let product = settings::read_hklm_string(bios_key, "SystemProductName").and_then(clean);
        let machine_name = match (family, product) {
            (Some(f), Some(p)) if !f.contains(&p) => format!("{f} · {p}"),
            (Some(f), _) => f,
            (None, Some(p)) => p,
            (None, None) => "HONOR MagicBook".to_string(),
        };
        let bios = crate::hw::firmware::bios_version()
            .map(|v| format!("{machine_name} · BIOS {v}"))
            .unwrap_or(machine_name);

        let state = PanelState {
            status,
            title: "GrandHonor".to_string(),
            subtitle: bios,
            refresh: t("Обновить", "Refresh", "刷新").to_string(),
            back: t("Назад", "Back", "返回").to_string(),
            telemetry,
            items: self.items,
            language: Some(grandhonor_core::locale::language().code().to_string()),
            notice: None,
            notice_is_error: false,
            job_running: crate::updater::checking() || crate::updater::installing(),
        };
        let mut state = state;
        if let Some((text, error)) = PANEL_NOTICE.lock().ok().and_then(|mut s| s.take()) {
            state.notice = Some(text);
            state.notice_is_error = error;
        }
        let cmds = self
            .commands
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default();
        (state, cmds)
    }
}

pub struct SubMenu<'a> {
    parent_item: &'a mut ControlItem,
    commands: Arc<Mutex<HashMap<u32, ActionFn>>>,
    next_id: Arc<Mutex<u32>>,
}

impl<'a> SubMenu<'a> {
    pub fn add_submenu_ex(
        &mut self,
        text: &str,
        help: Option<&str>,
        badge: Option<&str>,
        kind: Option<&str>,
    ) -> SubMenu<'_> {
        let children = self.parent_item.children.get_or_insert_with(Vec::new);
        let idx = children.len();
        children.push(ControlItem {
            text: text.to_string(),
            help: help.map(str::to_string),
            badge: badge.map(str::to_string),
            command: None,
            selected: false,
            children: Some(Vec::new()),
            kind: kind.map(str::to_string),
        });
        SubMenu {
            parent_item: &mut children[idx],
            commands: self.commands.clone(),
            next_id: self.next_id.clone(),
        }
    }

    pub fn add_item(
        &mut self,
        text: &str,
        action: Option<ActionFn>,
        enabled: bool,
        selected: bool,
        help: Option<&str>,
    ) {
        self.add_command(text, action, help, None, enabled, selected);
    }

    pub fn add_command_ex(
        &mut self,
        text: &str,
        action: Option<ActionFn>,
        help: Option<&str>,
        badge: Option<&str>,
        kind: Option<&str>,
        enabled: bool,
        selected: bool,
    ) {
        let command = if enabled && let Some(cb) = action {
            let id = self
                .next_id
                .lock()
                .map(|mut n| {
                    let cur = *n;
                    *n += 1;
                    cur
                })
                .unwrap_or(1);
            if let Ok(mut map) = self.commands.lock() {
                map.insert(id, cb);
            }
            Some(id)
        } else {
            None
        };
        let children = self.parent_item.children.get_or_insert_with(Vec::new);
        children.push(ControlItem {
            text: text.to_string(),
            help: help.map(str::to_string),
            badge: badge.map(str::to_string),
            command,
            selected,
            children: None,
            kind: kind.map(str::to_string),
        });
    }

    pub fn add_command(
        &mut self,
        text: &str,
        action: Option<ActionFn>,
        help: Option<&str>,
        kind: Option<&str>,
        enabled: bool,
        selected: bool,
    ) {
        self.add_command_ex(text, action, help, None, kind, enabled, selected);
    }

    pub fn add_container(&mut self, text: &str, kind: &str) -> SubMenu<'_> {
        self.add_submenu_ex(text, None, None, Some(kind))
    }

    pub fn add_separator(&mut self) {
        let children = self.parent_item.children.get_or_insert_with(Vec::new);
        children.push(ControlItem {
            text: String::new(),
            help: None,
            badge: None,
            command: None,
            selected: false,
            children: None,
            kind: None,
        });
    }
}

#[derive(Clone)]
pub struct HostContext {
    pub tray_hwnd: isize,
    pub backlight_schedule: Arc<BacklightScheduleService>,
    pub driver_panel: DriverPanel,
    pub on_update_tray_icon: Arc<dyn Fn() + Send + Sync>,
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub on_notify: Arc<dyn Fn(&str, &str) + Send + Sync>,
    pub on_quit: Arc<dyn Fn() + Send + Sync>,
    pub last_sensor_ms: Arc<AtomicU64>,
}

pub fn refresh_sensors_throttled(ctx: &HostContext, min_interval_ms: u64) {
    let now = crate::hw::events::monotonic_ms();
    let prev = ctx.last_sensor_ms.load(Ordering::Acquire);
    if now.saturating_sub(prev) < min_interval_ms && prev != 0 {
        return;
    }
    if ctx
        .last_sensor_ms
        .compare_exchange(prev, now, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        if read_and_store("panel").is_ok() {
            (ctx.on_update_tray_icon)();
        }
    }
}

pub fn build_panel_snapshot(ctx: &HostContext) -> (PanelState, HashMap<u32, ActionFn>) {
    refresh_sensors_throttled(ctx, 5_000);
    let mut root = MenuBuilder::new();

    // 1. Power & Battery
    let mut power = root.add_submenu(
        t("Питание", "Power", "电源"),
        Some(t("Заряд и производительность", "Charging and performance", "充电与性能")),
        Some("battery"),
    );
    if let Some(current) = windows_power_mode() {
        power.add_item(
            t("Режим питания", "Power mode", "电源模式"),
            None, false, false,
            Some(t(
                "Режим Windows, отдельно для зарядки и батареи. Fn+P переключает по кругу",
                "The Windows mode, kept separately on the charger and on battery. Fn+P cycles it",
                "Windows 电源模式，接通电源与使用电池分别记忆。Fn+P 循环切换",
            )),
        );
        let mut modes = power.add_container("", "segment");
        for (mode, label) in [
            (WINDOWS_EFFICIENCY, t("Экономия", "Efficiency", "能效")),
            (WINDOWS_BALANCED, t("Баланс", "Balanced", "平衡")),
            (WINDOWS_PERFORMANCE, t("Производительность", "Performance", "性能")),
        ] {
            modes.add_command_ex(
                label,
                Some(Box::new(move || {
                    match set_windows_power_mode(mode) {
                        Ok(()) => {
                            let icon = if mode == WINDOWS_EFFICIENCY { Icon::Normal } else if mode == WINDOWS_PERFORMANCE { Icon::Performance } else { Icon::Balanced };
                            crate::osd::show(icon, label, None);
                        }
                        Err(err) => show_error_box(&err),
                    }
                })),
                None, None, Some("pill"), true, current == mode,
            );
        }
    }
    build_cpu_profile_section(&mut power);
    power.add_separator();
    build_performance_section(&mut power, ctx.on_update_tray_icon.clone());
    power.add_separator();
    build_battery_section(&mut power);
    power.add_separator();
    build_presets_section(&mut power);
    build_night_section(&mut power);
    build_app_profiles_section(&mut power);

    // 2. Display: refresh rate and HONOR's Display & Eye Comfort modes.
    let mut screen = root.add_submenu(
        t("Экран", "Display", "显示"),
        Some(t("Частота и защита зрения", "Refresh rate and eye comfort", "刷新率与护眼")),
        Some("display"),
    );
    if let Some((rates, current)) = crate::hw::display::rates() {
        screen.add_item(t("Частота экрана", "Refresh rate", "屏幕刷新率"), None, false, false, None);
        let mut seg = screen.add_container("", "segment");
        for hz in rates {
            seg.add_command_ex(
                &format!("{hz} {}", t("Гц", "Hz", "赫兹")),
                Some(Box::new(move || {
                    if crate::hw::display::set_refresh_rate(hz) {
                        crate::osd::show(Icon::Display, &format!("{hz} {}", t("Гц", "Hz", "赫兹")), None);
                    } else {
                        show_error_box(t("Не удалось сменить частоту экрана.", "Could not change the refresh rate.", "无法更改刷新率。"));
                    }
                })),
                None, None, Some("pill"), true, current == hz,
            );
        }
    }
    build_display_filter_section(&mut screen);

    // 3. Keyboard
    let mut kb = root.add_submenu(
        t("Клавиатура", "Keyboard", "键盘"),
        Some(t(
            "Яркость, таймаут и расписание подсветки",
            "Backlight, idle timeout and schedule",
            "背光、超时与计划",
        )),
        Some("keyboard"),
    );
    build_keyboard_section(&mut kb, ctx.backlight_schedule.clone());
    build_keys_section(&mut kb);

    // 4. Touchpad (only when compatible Goodix/HONOR touchpad is present)
    if touchpad_supported() {
        let mut tp = root.add_submenu(
            t("Тачпад", "Touchpad", "触控板"),
            Some(t(
                "Жесты и тактильный отклик",
                "Gestures and haptic feedback",
                "手势与触觉反馈",
            )),
            Some("touchpad"),
        );
        build_touchpad_section(&mut tp);
    }

    // 5. Drivers and BIOS
    let upd_count = ctx.driver_panel.pending_update_count();
    let upd_badge = (upd_count > 0).then(|| upd_count.to_string());
    let mut drv = root.add_submenu_ex(
        t("Драйверы", "Drivers", "驱动"),
        Some(t(
            "Обновления и резервная копия драйверов",
            "Driver updates and backup",
            "驱动更新与备份",
        )),
        upd_badge.as_deref(),
        Some("drivers"),
    );
    ctx.driver_panel.build(&mut drv);
    build_device_check(&mut drv);

    // 5. Settings
    let mut cfg = root.add_submenu(
        t("Система", "System", "系统"),
        Some(t(
            "Уведомления и запуск GrandHonor",
            "Notifications and startup",
            "通知与启动",
        )),
        Some("settings"),
    );
    cfg.add_item(
        t("Уведомления и запуск", "Notifications and startup", "通知与启动"),
        None,
        false,
        false,
        None,
    );
    let notif_enabled = settings::notifications_enabled();
    cfg.add_command_ex(
        t("Уведомления Windows", "Windows notifications", "Windows 通知"),
        Some(Box::new(move || {
            settings::set_notifications_enabled(!settings::notifications_enabled());
        })),
        Some(t(
            "Вместо экранных подсказок, когда они выключены, и для важных событий зарядки",
            "Instead of on-screen notices when they are off, and for important charging events",
            "屏幕提示关闭时代替提示，并用于重要充电事件",
        )),
        None,
        Some("switch"),
        true,
        notif_enabled,
    );
    let osd_on = settings::osd_enabled();
    cfg.add_command_ex(
        t("Экранные подсказки", "On-screen notices", "屏幕提示"),
        Some(Box::new(move || settings::set_osd_enabled(!osd_on))),
        Some(t(
            "Короткая подсказка над панелью задач при нажатии клавиш Fn",
            "A short notice above the taskbar when you press Fn keys",
            "按下 Fn 键时在任务栏上方显示简短提示",
        )),
        None, Some("switch"), true, osd_on,
    );
    let startup_on = is_startup_enabled();
    cfg.add_command_ex(
        t("Запускать с Windows", "Start with Windows", "开机自启动"),
        Some(Box::new(|| {
            let next = !is_startup_enabled();
            if let Err(err) = set_startup_enabled(next) {
                show_error_box(&format!(
                    "{}{err}",
                    t(
                        "Не удалось изменить автозапуск: ",
                        "Failed to change startup setting: ",
                        "无法修改开机自启动设置："
                    )
                ));
            }
        })),
        Some(if is_installation_path() {
            t(
                "Клавиши Fn, жесты и лимиты заряда работают, пока GrandHonor запущен",
                "Fn keys, gestures and charge limits work while GrandHonor runs",
                "GrandHonor 运行时 Fn 键、手势和充电上限才会生效",
            )
        } else {
            t(
                "Доступно после установки через установщик",
                "Available after installing with the setup program",
                "通过安装程序安装后可用",
            )
        }),
        None,
        Some("switch"),
        is_installation_path(),
        startup_on,
    );
    // Developer aid only: release builds have no test-notification button.
    #[cfg(debug_assertions)]
    let notify_cb = ctx.on_notify.clone();
    #[cfg(debug_assertions)]
    cfg.add_command_ex(
        t("Проверить уведомление", "Send test notification", "发送测试通知"),
        Some(Box::new(move || {
            notify_cb(
                "GrandHonor",
                t("Уведомления работают", "Notifications are working", "通知可用"),
            );
        })),
        Some(t(
            "Показать тестовое системное сообщение в трее",
            "Trigger a sample system tray notification",
            "触发示例系统托盘通知",
        )),
        Some(t("Тест", "Test", "测试")),
        Some("card"),
        notif_enabled,
        false,
    );
    cfg.add_separator();
    cfg.add_item(t("Язык", "Language", "语言"), None, false, false, None);
    let chosen = settings::language_preference();
    let mut languages = cfg.add_container("", "segment");
    languages.add_command_ex(
        t("Авто", "Auto", "自动"),
        Some(Box::new(|| settings::set_language_preference(None))),
        None, None, Some("pill"), true, chosen.is_none(),
    );
    for lang in AppLanguage::ALL {
        languages.add_command_ex(
            lang.native_name(),
            Some(Box::new(move || settings::set_language_preference(Some(lang)))),
            None, None, Some("pill"), true, chosen == Some(lang),
        );
    }
    build_update_section(&mut cfg, ctx.on_quit.clone());
    build_settings_transfer_section(&mut cfg, ctx.on_quit.clone());
    cfg.add_separator();
    let quit_cb = ctx.on_quit.clone();
    cfg.add_command_ex(
        t("Выход из GrandHonor", "Quit GrandHonor", "退出 GrandHonor"),
        Some(Box::new(move || quit_cb())),
        Some(t(
            "Полностью закрыть GrandHonor. Клавиши Fn и жесты перестанут работать",
            "Close GrandHonor completely. Fn keys and gestures stop working",
            "完全退出 GrandHonor。Fn 键和手势将停止工作",
        )),
        None,
        Some("danger"),
        true,
        false,
    );

    let (status, telemetry) = build_telemetry(true);
    root.build_into(status, telemetry)
}

/// Export / import of all settings (settings_transfer.rs). Import restarts GrandHonor.
fn build_settings_transfer_section(menu: &mut SubMenu<'_>, on_quit: Arc<dyn Fn() + Send + Sync>) {
    let mut row = menu.add_container("", "row-2");
    row.add_command_ex(
        t("Экспорт настроек", "Export settings", "导出设置"),
        Some(Box::new(|| {
            if let Err(err) = crate::settings_transfer::export_dialog() {
                show_error_box(&err);
            }
        })),
        Some(t("Сохранить в файл", "Save to a file", "保存到文件")),
        None, Some("card"), true, false,
    );
    row.add_command_ex(
        t("Импорт настроек", "Import settings", "导入设置"),
        Some(Box::new(move || match crate::settings_transfer::import_dialog() {
            Ok(true) => match crate::updater::spawn_relaunch() {
                Ok(()) => on_quit(),
                Err(err) => show_error_box(&err),
            },
            Ok(false) => {}
            Err(err) => show_error_box(&err),
        })),
        Some(t("Загрузить и перезапустить", "Load and restart", "加载并重启")),
        None, Some("card"), true, false,
    );
}

/// GrandHonor updates from GitHub and rollback to the stored previous version (updater.rs).
fn build_update_section(menu: &mut SubMenu<'_>, on_quit: Arc<dyn Fn() + Send + Sync>) {
    use crate::updater::{self, State};
    menu.add_separator();
    menu.add_item(t("Обновления GrandHonor", "GrandHonor updates", "GrandHonor 更新"), None, false, false, None);
    let installed = is_installation_path();
    let version = env!("CARGO_PKG_VERSION");
    match updater::state() {
        Some(State::Available(release)) => {
            let title = format!("{} {}", t("Установить версию", "Install version", "安装版本"), release.version);
            let quit = on_quit.clone();
            menu.add_command_ex(
                &title,
                Some(Box::new(move || {
                    let quit = quit.clone();
                    updater::start_update_in_background(release.clone(), move |result| match result {
                        Ok(()) => quit(),
                        Err(err) => post_panel_error(&err),
                    })
                })),
                Some(if updater::installing() {
                    t("Загрузка и проверка…", "Downloading and checking…", "正在下载并校验…")
                } else if installed {
                    t(
                        "Скачать, проверить и установить. Если что-то пойдёт не так, вернётся текущая версия",
                        "Download, check and install. If anything goes wrong, this version comes back",
                        "下载、校验并安装；如有问题会恢复当前版本",
                    )
                } else {
                    t(
                        "Портативная версия: скачайте установщик со страницы выпусков на GitHub",
                        "Portable copy: download the setup from the GitHub releases page",
                        "便携版：请从 GitHub 发布页面下载安装程序",
                    )
                }),
                None, Some("cta"), installed && !updater::installing(), false,
            );
        }
        other => {
            let checking = updater::checking();
            let help = match other {
                _ if checking => t("Проверка…", "Checking…", "正在检查…").to_string(),
                Some(State::UpToDate) => format!("{} {version}", t("Установлена последняя версия", "Latest version installed", "已是最新版本")),
                Some(State::Failed(err)) => format!(
                    "{}: {err}",
                    t("Не удалось проверить", "Check failed", "检查失败")
                ),
                _ => format!("{} {version}", t("Сейчас установлена", "Installed:", "当前版本")),
            };
            menu.add_command_ex(
                t("Проверить обновление GrandHonor", "Check for GrandHonor updates", "检查 GrandHonor 更新"),
                Some(Box::new(updater::check)),
                Some(&help),
                None, Some("card"), !checking, false,
            );
        }
    }
    if let Some(previous) = updater::rollback_version().filter(|v| v != version) {
        let title = format!("{} {previous}", t("Вернуть версию", "Roll back to", "回退到版本"));
        menu.add_command_ex(
            &title,
            Some(Box::new(move || match updater::start_rollback() {
                Ok(()) => on_quit(),
                Err(err) => show_error_box(&err),
            })),
            Some(if installed {
                t(
                    "Если новая версия работает плохо: вернуть сохранённую прошлую",
                    "If the new version misbehaves: restore the stored previous one",
                    "如果新版本有问题：恢复已保存的旧版本",
                )
            } else {
                t("Доступно в установленной версии", "Available in the installed version", "仅安装版可用")
            }),
            None, Some("card"), installed, false,
        );
    }
}

/// Power → App profiles (hw/app_profiles.rs): Performance and the top refresh rate while a
/// chosen program runs.
fn build_app_profiles_section(menu: &mut SubMenu<'_>) {
    let apps = settings::app_profiles();
    let summary = if apps.is_empty() {
        t("Не выбраны", "None chosen", "未选择").to_string()
    } else {
        apps.iter().map(|a| a.trim_end_matches(".exe")).collect::<Vec<_>>().join(", ")
    };
    let mut page = menu.add_submenu_ex(t("Профили программ", "App profiles", "应用配置"), Some(&summary), None, Some("preset"));
    page.add_item(
        t("Программы", "Programs", "程序"),
        None, false, false,
        Some(t(
            "Пока программа открыта: режим «Производительность» и наибольшая частота экрана. После закрытия всё вернётся",
            "While a program is open: Performance mode and the highest refresh rate. Closing it puts both back",
            "程序运行时使用高性能模式与最高刷新率，关闭后恢复",
        )),
    );
    for app in apps.clone() {
        let rest: Vec<String> = apps.iter().filter(|a| **a != app).cloned().collect();
        let key = format!("app-profile/{app}");
        let armed = is_armed(&key);
        page.add_command_ex(
            app.trim_end_matches(".exe"),
            Some(Box::new(move || {
                if confirmed(&key) {
                    settings::set_app_profiles(&rest);
                    crate::hw::app_profiles::settings_changed();
                }
            })),
            Some(if armed { t("Нажмите ещё раз, чтобы убрать из списка", "Click again to remove it", "再次点击以移除") } else { &app }),
            Some(t("Убрать", "Remove", "移除")),
            Some(if armed { "danger" } else { "card" }), true, false,
        );
    }
    let current = apps.clone();
    page.add_command_ex(
        t("Добавить программу…", "Add a program…", "添加程序…"),
        Some(Box::new(move || {
            match crate::drivers::transfer::open_file_dialog(
                t("Программа для профиля", "Program for the profile", "配置使用的程序"),
                t("Программы", "Programs", "程序"),
                "*.exe",
            ) {
                Ok(Some(path)) => {
                    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()) else { return };
                    let mut list = current.clone();
                    if !list.contains(&name) {
                        list.push(name);
                    }
                    settings::set_app_profiles(&list);
                    crate::hw::app_profiles::settings_changed();
                }
                Ok(None) => {}
                Err(err) => show_error_box(&err),
            }
        })),
        None, None, Some("card"), true, false,
    );
}

/// Power → Quiet nights (hw/night_mode.rs): Efficiency between two hours.
fn build_night_section(menu: &mut SubMenu<'_>) {
    let enabled = settings::night_mode_enabled();
    let (from, to) = settings::night_mode_hours();
    menu.add_command_ex(
        t("Тихие ночи", "Quiet nights", "安静夜间"),
        Some(Box::new(move || {
            settings::set_night_mode_enabled(!enabled);
            crate::hw::night_mode::poke();
        })),
        Some(t(
            "Ночью режим «Экономия»: тише вентиляторы. Утром вернётся прежний режим",
            "Efficiency mode at night: quieter fans. The previous mode returns in the morning",
            "夜间使用能效模式，风扇更安静；早晨恢复原模式",
        )),
        None, Some("switch"), true, enabled,
    );
    if !enabled {
        return;
    }
    let mut row = menu.add_container("", "row-2");
    for (title, current, is_from) in [
        (t("С", "From", "从"), from, true),
        (t("До", "Until", "至"), to, false),
    ] {
        let badge = format!("{current:02}:00");
        let mut sub = row.add_submenu_ex(title, None, Some(&badge), Some("time"));
        let mut grid = sub.add_container("", "grid-hours");
        for h in 0..24u32 {
            grid.add_command_ex(
                &format!("{h:02}:00"),
                Some(Box::new(move || {
                    let (f, u) = settings::night_mode_hours();
                    if is_from { settings::set_night_mode_hours(h, u) } else { settings::set_night_mode_hours(f, h) }
                    crate::hw::night_mode::poke();
                })),
                None, None, Some("pill"), true, h == current,
            );
        }
    }
}

/// "When the charger changes": per power source presets (see presets.rs).
/// Power → Processor profile: Quiet / Balanced / Performance for the charger and for battery.
fn build_cpu_profile_section(menu: &mut SubMenu<'_>) {
    use crate::hw::cpu_profile::{Profile, apply, chosen};
    menu.add_item(
        t("Профиль процессора", "Processor profile", "处理器配置"),
        None, false, false,
        Some(t(
            "Тихий — прохладнее и дольше от батареи, Мощный — быстрее отклик и выше частоты",
            "Quiet runs cooler and longer on battery, Fast responds quicker with higher clocks",
            "安静更凉爽、续航更长；强劲响应更快、频率更高",
        )),
    );
    for (ac, title) in [
        (true, t("От сети", "Plugged in", "接通电源")),
        (false, t("От батареи", "On battery", "使用电池")),
    ] {
        menu.add_item(title, None, false, false, None);
        let current = chosen(ac);
        let mut seg = menu.add_container("", "segment");
        let key = if ac { "Ac" } else { "Dc" };
        let options: [(Option<Profile>, &'static str); 4] = [
            (None, t("Windows", "Windows", "Windows")),
            (Some(Profile::Quiet), t("Тихий", "Quiet", "安静")),
            (Some(Profile::Balanced), t("Баланс", "Balanced", "平衡")),
            (Some(Profile::Performance), t("Мощный", "Fast", "强劲")),
        ];
        for (profile, label) in options {
            seg.add_command_ex(
                label,
                Some(Box::new(move || {
                    let previous = settings::cpu_profile(key);
                    settings::set_cpu_profile(key, profile.map(|p| p as u32));
                    if !apply() {
                        // The panel must show what is in effect, not a choice Windows refused.
                        settings::set_cpu_profile(key, previous);
                        let _ = apply();
                        show_error_box(t(
                            "Windows не дала изменить схему питания",
                            "Windows refused to change the power scheme",
                            "Windows 拒绝更改电源计划",
                        ));
                        return;
                    }
                    let icon = match profile {
                        Some(Profile::Performance) => Icon::Performance,
                        Some(Profile::Quiet) => Icon::Normal,
                        _ => Icon::Balanced,
                    };
                    let source = if ac { t("От сети", "Plugged in", "接通电源") } else { t("От батареи", "On battery", "使用电池") };
                    crate::osd::show(icon, &format!("{source}: {label}"), None);
                })),
                None, None, Some("pill"), true, current == profile,
            );
        }
    }
}

fn build_presets_section(menu: &mut SubMenu<'_>) {
    use crate::presets::{BACKLIGHT, BRIGHTNESS, MODE, REFRESH};
    menu.add_item(
        t("При смене питания", "When the charger changes", "电源切换时"),
        None, false, false, None,
    );
    let enabled = settings::preset_enabled();
    menu.add_command_ex(
        t("Пресеты питания", "Power presets", "电源预设"),
        Some(Box::new(move || settings::set_preset_enabled(!enabled))),
        Some(t(
            "Сами меняют режим, подсветку и яркость при подключении и отключении зарядки",
            "Switch mode, backlight and brightness when you plug in or unplug",
            "插拔电源时自动切换模式、背光和亮度",
        )),
        None, Some("switch"), true, enabled,
    );
    if !enabled {
        return;
    }
    let keep = t("Не менять", "Keep", "不变");
    let owned = |list: &[(u32, &str)]| list.iter().map(|(id, l)| (*id, l.to_string())).collect::<Vec<_>>();
    let modes = owned(&[(1, t("Экономия", "Efficiency", "能效")), (2, t("Баланс", "Balanced", "平衡")), (3, t("Максимум", "Best performance", "最佳性能"))]);
    let lights = owned(&[(1, t("Выкл.", "Off", "关闭")), (2, t("Слабая", "Weak", "弱")), (3, t("Сильная", "Strong", "强"))]);
    let screens = owned(&[(31, "30%"), (51, "50%"), (71, "70%"), (101, "100%")]);
    let hz_label = t("Гц", "Hz", "赫兹");
    let rates: Vec<(u32, String)> = crate::hw::display::rates()
        .map(|(list, _)| list.into_iter().map(|hz| (hz, format!("{hz} {hz_label}"))).collect())
        .unwrap_or_default();
    for (on_ac, title) in [
        (true, t("От сети", "Plugged in", "接通电源")),
        (false, t("От батареи", "On battery", "使用电池")),
    ] {
        let key = crate::presets::source_key(on_ac);
        let label_of = |list: &[(u32, String)], name: &str| {
            settings::preset_value(key, name)
                .and_then(|v| list.iter().find(|(id, _)| *id == v).map(|(_, l)| l.clone()))
                .unwrap_or_else(|| keep.to_string())
        };
        let mut parts = vec![label_of(&modes, MODE), label_of(&lights, BACKLIGHT), label_of(&screens, BRIGHTNESS)];
        if !rates.is_empty() {
            parts.push(label_of(&rates, REFRESH));
        }
        let summary = parts.join(" · ");
        let mut page = menu.add_submenu_ex(title, Some(&summary), None, Some("preset"));
        let mut groups = vec![
            (t("Режим питания Windows", "Windows power mode", "Windows 电源模式"), MODE, &modes),
            (t("Подсветка клавиатуры", "Keyboard backlight", "键盘背光"), BACKLIGHT, &lights),
            (t("Яркость экрана", "Display brightness", "屏幕亮度"), BRIGHTNESS, &screens),
        ];
        if !rates.is_empty() {
            groups.push((t("Частота экрана", "Refresh rate", "刷新率"), REFRESH, &rates));
        }
        for (heading, name, options) in groups {
            page.add_item(heading, None, false, false, None);
            let current = settings::preset_value(key, name);
            let mut seg = page.add_container("", "segment");
            seg.add_command_ex(
                keep,
                Some(Box::new(move || settings::set_preset_value(key, name, None))),
                None, None, Some("pill"), true, current.is_none(),
            );
            for (id, label) in options.iter().cloned() {
                seg.add_command_ex(
                    &label,
                    Some(Box::new(move || settings::set_preset_value(key, name, Some(id)))),
                    None, None, Some("pill"), true, current == Some(id),
                );
            }
        }
        page.add_separator();
        page.add_command_ex(
            t("Применить сейчас", "Apply now", "立即应用"),
            Some(Box::new(move || crate::presets::apply(key))),
            Some(t("Проверить пресет, не меняя зарядку", "Try the preset without unplugging", "无需插拔即可试用")),
            None, Some("card"), true, false,
        );
    }
}

fn build_battery_section(menu: &mut SubMenu<'_>) {
    let current = settings::battery_protection().or_else(settings::preferred_battery_protection);
    let modes = [
        (
            BatteryMode::Disabled,
            t("Без лимита", "Unrestricted", "无限制"),
            t("до 100%", "up to 100%", "上限 100%"),
            t("Полный заряд батареи", "Full battery charge", "充满电池"),
        ),
        (
            BatteryMode::Home,
            t("Дом", "Home", "居家"),
            "40–70%",
            t("Долгая работа от сети", "Long hours on the charger", "长时间插电使用"),
        ),
        (
            BatteryMode::Office,
            t("Офис", "Office", "办公"),
            "70–90%",
            t("Баланс автономности", "Balanced battery life", "均衡续航"),
        ),
        (
            BatteryMode::Travel,
            t("Поездки", "Travel", "出行"),
            "95–100%",
            t("Быстрая подзарядка", "Quick top-up", "快速补电"),
        ),
    ];

    // Health from the battery driver: full-charge vs design capacity and cycles.
    let health = crate::hw::battery_health::read().map(|h| {
        let wh = |mwh: u32| format!("{:.1}", f64::from(mwh) / 1000.0);
        let cycles = h
            .cycles
            .map(|c| format!(" · {c} {}", t("циклов", "cycles", "次循环")))
            .unwrap_or_default();
        let trend = crate::hw::battery_alerts::wear_trend(h.design_mwh)
            .map(|(months, delta)| format!(" · {} {months} {}: {delta:+}%", t("за", "in", "近"), t("мес.", "mo", "个月")))
            .unwrap_or_default();
        format!(
            "{}% · {} {} {} {}{cycles}{trend}",
            h.percent(),
            wh(h.full_mwh),
            t("из", "of", "/"),
            wh(h.design_mwh),
            t("Вт·ч", "Wh", "Wh"),
        )
    });
    menu.add_item(
        t("Защита батареи", "Battery protection", "电池保护"),
        None,
        false,
        false,
        health.as_deref().map(|h| format!("{} {h}", t("Здоровье:", "Health:", "健康度："))).as_deref(),
    );
    let mut grid = menu.add_container("", "group-grid");
    for (mode, label, badge, tip) in modes {
        grid.add_command_ex(
            label,
            Some(Box::new(move || match apply_battery_mode_and_save(mode) {
                Ok(()) => crate::osd::show(Icon::Battery, &format!("{label} · {badge}"), None),
                Err(err) => {
                    crate::log::error_with("Could not start battery configuration", &err);
                    show_error_box(&err.to_string());
                }
            })),
            Some(tip),
            Some(badge),
            Some("tile"),
            true,
            current == Some(mode),
        );
    }
    let smart = settings::smart_charge_enabled();
    menu.add_command_ex(
        t("Умная зарядка", "Smart charging", "智能充电"),
        Some(Box::new(move || {
            settings::set_smart_charge_enabled(!smart);
            if smart {
                crate::hw::smart_charge::release();
            } else {
                crate::presets::start();
            }
        })),
        Some(if settings::smart_charge_holding() {
            t(
                "Сейчас держим 70–90%: ноутбук долго на зарядке",
                "Holding 70–90% now: the laptop has been on the charger for a long time",
                "正在保持 70–90%：已长时间接通电源",
            )
        } else {
            t(
                "После 3 ч на зарядке держать 70–90%, пока вы не отключите зарядку",
                "After 3 h on the charger, hold 70–90% until you unplug",
                "接通电源 3 小时后保持 70–90%，拔下电源后恢复",
            )
        }),
        None,
        Some("switch"),
        true,
        smart,
    );
    let once = settings::full_charge_once();
    let limited = current.is_some_and(|m| m != BatteryMode::Disabled);
    if once || limited {
        menu.add_command_ex(
            t("Зарядить до 100% один раз", "Charge to 100% once", "单次充满 100%"),
            Some(Box::new(move || {
                if once {
                    std::thread::spawn(|| crate::hw::battery_alerts::end_full_charge(false));
                } else {
                    match crate::hw::battery_alerts::start_full_charge() {
                        Ok(()) => crate::osd::show(Icon::Battery, t("Заряжаем до 100%", "Charging to 100%", "正在充至 100%"), None),
                        Err(err) => show_info_box(&err),
                    }
                }
            })),
            Some(if once {
                t(
                    "Лимит снят до полного заряда или отключения зарядки",
                    "The limit is lifted until the battery is full or you unplug",
                    "充满或拔下电源前不限制",
                )
            } else {
                t("Перед поездкой: потом лимит вернётся сам", "Before a trip: the limit comes back by itself", "出行前使用，之后自动恢复上限")
            }),
            None, Some("switch"), true, once,
        );
    }
    let notices = settings::charge_notices_enabled();
    menu.add_command_ex(
        t("Уведомления о заряде", "Charge notices", "电量提示"),
        Some(Box::new(move || settings::set_charge_notices_enabled(!notices))),
        Some(t(
            "Когда заряд дошёл до лимита, осталось 20% и сколько ушло во сне",
            "When charging reaches the limit, at 20% left and the charge used asleep",
            "充到上限、剩余 20% 以及睡眠耗电时提示",
        )),
        None, Some("switch"), true, notices,
    );
}

fn build_performance_section(menu: &mut SubMenu<'_>, on_mode_changed: Arc<dyn Fn() + Send + Sync>) {
    menu.add_item(
        t("Режим HONOR", "HONOR mode", "HONOR 模式"),
        None,
        false,
        false,
        None,
    );
    if settings::hunter_mode_active() {
        menu.add_command_ex(
            t("Режим HUNTER активен", "HUNTER mode active", "HUNTER 模式已启用"),
            None,
            Some(t(
                "Игровой режим ноутбука. Пока он включён, профиль HONOR не меняется.",
                "The laptop's gaming mode. While it is on, the HONOR profile does not change.",
                "笔记本的游戏模式。开启期间 HONOR 模式不会改变。",
            )),
            Some("HUNTER"),
            Some("banner"),
            false,
            true,
        );
        return;
    }

    let active = settings::performance_mode_active();
    menu.add_command_ex(
        t("Производительный режим", "Performance mode", "高性能模式"),
        Some(Box::new(move || {
            if settings::hunter_mode_active() {
                return;
            }
            let target = !settings::performance_mode_active();
            if target && let Err(reason) = can_enable_performance_mode() {
                show_info_box(&reason);
                return;
            }
            match apply_power_unlock_and_save(target) {
                Ok(()) => {
                    on_mode_changed();
                    let text = if target {
                        t("Производительный режим HONOR", "HONOR Performance on", "HONOR 高性能已开启")
                    } else {
                        t("Обычный режим HONOR", "HONOR normal mode", "HONOR 标准模式")
                    };
                    crate::osd::show(if target { Icon::Performance } else { Icon::Balanced }, text, None);
                }
                Err(err) => {
                    crate::log::error_with("Could not change performance mode", &err);
                    show_error_box(&err.to_string());
                }
            }
        })),
        Some(t(
            "Только от сети и при заряде от 20%",
            "Only on the charger with at least 20% battery",
            "需接通电源且电量不低于 20%",
        )),
        None,
        Some("switch"),
        true,
        active,
    );
}

/// Drivers → Device check: devices with a problem and one-click repairs (`hw/devices.rs`).
fn build_device_check(menu: &mut SubMenu<'_>) {
    use crate::hw::devices::{self, Radio};
    let problems = devices::problems();
    let summary = if problems.is_empty() {
        t("Все устройства работают", "All devices are working", "所有设备工作正常").to_string()
    } else {
        format!("{}: {}", t("Устройств с ошибками", "Devices with a problem", "异常设备"), problems.len())
    };
    let badge = (!problems.is_empty()).then(|| problems.len().to_string());
    menu.add_separator();
    let mut page = menu.add_submenu_ex(
        t("Проверка устройств", "Device check", "设备检查"),
        Some(&summary),
        badge.as_deref(),
        Some("collection"),
    );
    if problems.is_empty() {
        page.add_command_ex(
            t("Ошибок не найдено", "No problems found", "未发现问题"),
            None,
            Some(t(
                "Windows не сообщает о сбоях ни одного устройства",
                "Windows reports no device with a problem",
                "Windows 未报告任何设备故障",
            )),
            None,
            Some("banner"),
            false,
            false,
        );
    } else {
        page.add_item(
            t("Устройства с ошибками", "Devices with a problem", "异常设备"),
            None, false, false,
            Some(t("Откройте устройство: перезапуск или откат драйвера", "Open a device to restart it or roll back its driver", "打开设备以重新启动或回退驱动")),
        );
        for device in problems {
            let help = format!("{} {}: {}", t("Код", "Code", "代码"), device.problem, devices::problem_text(device.problem));
            let mut dev = page.add_submenu_ex(&device.name, Some(&help), None, Some("driver"));
            let (id, name) = (device.instance_id.clone(), device.name.clone());
            dev.add_command_ex(
                t("Перезапустить устройство", "Restart the device", "重启设备"),
                Some(Box::new(move || {
                    let result = devices::restart_device(&id);
                    devices::invalidate();
                    report_repair(&name, result);
                })),
                Some(t("Как отключить и включить в диспетчере устройств", "Like disable and enable in Device Manager", "相当于在设备管理器中禁用再启用")),
                None, Some("card"), true, false,
            );
            let (id, name) = (device.instance_id.clone(), device.name.clone());
            let key = format!("rollback/{id}");
            let armed = is_armed(&key);
            dev.add_command_ex(
                t("Откатить драйвер", "Roll back driver", "回退驱动程序"),
                Some(Box::new(move || {
                    if !confirmed(&key) {
                        return;
                    }
                    let result = devices::rollback_driver(&id);
                    devices::invalidate();
                    report_repair(&name, result);
                })),
                Some(if armed {
                    t("Нажмите ещё раз, чтобы откатить", "Click again to roll back", "再次点击以回退")
                } else {
                    t("Вернуть драйвер, стоявший до последнего обновления", "Go back to the driver used before the last update", "恢复上次更新前的驱动程序")
                }),
                None, Some("danger"), true, false,
            );
        }
    }
    if let Some(ssd) = crate::hw::storage::read() {
        page.add_separator();
        let state = if ssd.warning || ssd.media_errors > 0 || ssd.used_percent >= 90 {
            t("Требует внимания", "Needs attention", "需要注意")
        } else {
            t("В порядке", "Good", "良好")
        };
        page.add_item(
            &format!("SSD · {}", ssd.model),
            None, false, false,
            Some(&format!(
                "{state} · {} {}% · {} °C · {:.1} {} · {} {}",
                t("износ", "wear", "磨损"), ssd.used_percent, ssd.temperature_c, ssd.written_tb,
                t("ТБ записано", "TB written", "TB 已写入"), ssd.power_on_hours, t("ч работы", "h on", "小时通电"),
            )),
        );
    }
    page.add_separator();
    page.add_item(t("Починка", "Repair", "修复"), None, false, false, None);
    for (radio, label, help) in [
        (
            Radio::Bluetooth,
            t("Перезапустить Bluetooth", "Restart Bluetooth", "重启蓝牙"),
            t("Если мышь, наушники или телефон не подключаются", "When a mouse, headphones or a phone will not connect", "鼠标、耳机或手机无法连接时"),
        ),
        (
            Radio::WiFi,
            t("Перезапустить Wi-Fi", "Restart Wi-Fi", "重启 Wi-Fi"),
            t("Если сеть не находится или постоянно отключается", "When networks are not found or keep dropping", "找不到网络或频繁断开时"),
        ),
    ] {
        let key = format!("radio/{radio:?}");
        let armed = is_armed(&key);
        page.add_command_ex(
            label,
            Some(Box::new(move || {
                // The connection drops for a few seconds: ask once more.
                if !confirmed(&key) {
                    return;
                }
                let result = devices::restart_radio(radio);
                devices::invalidate();
                report_repair(label, result);
            })),
            Some(if armed { t("Нажмите ещё раз: связь пропадёт на несколько секунд", "Click again: the connection drops for a few seconds", "再次点击：连接将中断几秒") } else { help }),
            None,
            None,
            true,
            false,
        );
    }
    page.add_separator();
    let mut row = page.add_container("", "row-2");
    row.add_command_ex(
        t("Отчёт о системе", "System report", "系统报告"),
        Some(Box::new(|| match crate::report::copy_to_clipboard(&crate::report::build()) {
            Ok(()) => crate::osd::show(Icon::Display, t("Отчёт скопирован", "Report copied", "报告已复制"), None),
            Err(err) => show_error_box(&err),
        })),
        Some(t("Скопировать для поддержки или GitHub", "Copy for support or GitHub", "复制以便提交支持或 GitHub")),
        None, Some("card"), true, false,
    );
    row.add_command_ex(
        t("Журнал событий", "Event log", "事件日志"),
        Some(Box::new(|| crate::unelevated::open(&crate::log::log_path().display().to_string()))),
        Some(t("Что GrandHonor делал и какие были ошибки", "What GrandHonor did and any errors", "GrandHonor 的操作与错误")),
        None, Some("card"), true, false,
    );
    page.add_command_ex(
        t("Найти новое оборудование", "Scan for hardware changes", "扫描硬件改动"),
        Some(Box::new(|| {
            let ok = devices::rescan();
            devices::invalidate();
            if ok {
                crate::osd::show(Icon::Display, t("Оборудование обновлено", "Hardware rescanned", "已重新扫描硬件"), None);
            } else {
                show_error_box(t("Не удалось обновить список оборудования.", "Could not scan for hardware changes.", "无法扫描硬件改动。"));
            }
        })),
        Some(t(
            "Как в диспетчере устройств: Windows заново ищет устройства и драйверы",
            "As in Device Manager: Windows looks for devices and drivers again",
            "与设备管理器相同：Windows 重新查找设备与驱动",
        )),
        None,
        None,
        true,
        false,
    );
}

fn report_repair(name: &str, result: Result<bool, String>) {
    match result {
        Ok(false) => crate::osd::show(Icon::Display, &format!("{name}: {}", t("перезапущено", "restarted", "已重启")), None),
        Ok(true) => show_info_box(&format!(
            "{name}: {}",
            t(
                "перезапуск завершится после перезагрузки Windows.",
                "the restart completes after Windows restarts.",
                "重启 Windows 后完成。",
            )
        )),
        Err(err) => show_error_box(&format!("{name}: {err}")),
    }
}

fn build_display_filter_section(menu: &mut SubMenu<'_>) {
    use settings::{ColorProfile, DisplayFilter};
    menu.add_separator();
    let profile = settings::color_profile();
    let profile_help = match profile {
        ColorProfile::Native => t(
            "Полный цветовой охват панели: ярко и насыщенно",
            "The panel's full gamut: vivid and saturated",
            "面板原生色域：鲜艳饱和",
        ),
        ColorProfile::Srgb => t(
            "Точные цвета для интернета, фото и офиса",
            "Accurate colours for the web, photos and office work",
            "适合网页、照片与办公的准确色彩",
        ),
        ColorProfile::DisplayP3 => t(
            "Широкий охват, как у экранов Apple (Display P3)",
            "Wide gamut, as on Apple displays (Display P3)",
            "与 Apple 显示屏一致的广色域（Display P3）",
        ),
    };
    menu.add_item(t("Цветовой профиль", "Colour profile", "色彩配置"), None, false, false, Some(profile_help));
    let mut profiles = menu.add_container("", "segment");
    for (choice, label) in [
        (ColorProfile::Native, t("Яркий", "Vivid", "鲜艳")),
        (ColorProfile::Srgb, "sRGB"),
        (ColorProfile::DisplayP3, "P3"),
    ] {
        profiles.add_command_ex(
            label,
            Some(Box::new(move || {
                settings::set_color_profile(choice);
                crate::hw::display_filter::apply_saved();
            })),
            None, None, Some("pill"), true, profile == choice,
        );
    }
    menu.add_separator();
    let current = settings::display_filter();
    let help = match current {
        DisplayFilter::Off => t(
            "Цвета экрана без изменений",
            "Screen colours unchanged",
            "屏幕颜色不做调整",
        ),
        DisplayFilter::EyeComfort => t(
            "Меньше синего света для вечера и долгой работы",
            "Less blue light for evenings and long sessions",
            "减少蓝光，适合夜间与长时间使用",
        ),
        DisplayFilter::Ebook => t(
            "Тёплый монохром для чтения; оттенок меняется со временем суток",
            "Warm monochrome for reading; the tint follows the time of day",
            "暖色单色阅读模式，色调随时间变化",
        ),
        DisplayFilter::Comfort => t(
            "Чуть спокойнее насыщенность и баланс белого",
            "Slightly calmer saturation and white balance",
            "略微降低饱和度，柔和白平衡",
        ),
    };
    menu.add_item(t("Защита зрения", "Eye comfort", "护眼"), None, false, false, Some(help));
    let mut seg = menu.add_container("", "segment");
    for (choice, label) in [
        (DisplayFilter::Off, t("Выкл.", "Off", "关闭")),
        (DisplayFilter::EyeComfort, t("Тёплый", "Warm", "护眼")),
        (DisplayFilter::Ebook, t("Книга", "E-book", "电子书")),
        (DisplayFilter::Comfort, t("Мягкий", "Soft", "舒适")),
    ] {
        seg.add_command_ex(
            label,
            Some(Box::new(move || {
                settings::set_display_filter(choice);
                crate::hw::display_filter::apply_saved();
            })),
            None, None, Some("pill"), true, current == choice,
        );
    }
    if current == DisplayFilter::EyeComfort {
        let warmth = settings::eye_warmth();
        let mut slider = menu.add_container("", "slider");
        for step in 1..=settings::WARMTH_MAX {
            let label = match step {
                1 => t("Слабее", "Subtle", "弱"),
                settings::WARMTH_MAX => t("Теплее", "Warmer", "暖"),
                _ => "",
            };
            slider.add_command_ex(
                label,
                Some(Box::new(move || {
                    settings::set_eye_warmth(step);
                    crate::hw::display_filter::apply_saved();
                })),
                None, None, None, true, warmth == step,
            );
        }
    }
    menu.add_separator();
    let every = settings::break_reminder_minutes();
    menu.add_item(
        t("Отдых для глаз", "Eye breaks", "护眼休息"),
        None, false, false,
        Some(t(
            "Напомнить посмотреть вдаль на 20 секунд; пауза в работе сбрасывает отсчёт",
            "A reminder to look into the distance for 20 seconds; a break in work resets it",
            "提醒您远眺 20 秒；离开电脑会重新计时",
        )),
    );
    let mut breaks = menu.add_container("", "segment");
    for minutes in crate::hw::break_reminder::CHOICES {
        let label = if minutes == 0 {
            t("Выкл.", "Off", "关闭").to_string()
        } else {
            format!("{minutes} {}", t("мин", "min", "分钟"))
        };
        breaks.add_command_ex(
            &label,
            Some(Box::new(move || {
                settings::set_break_reminder_minutes(minutes);
                crate::hw::break_reminder::settings_changed();
            })),
            None, None, Some("pill"), true, every == minutes,
        );
    }
}

fn build_keyboard_section(menu: &mut SubMenu<'_>, schedule: Arc<BacklightScheduleService>) {
    menu.add_item(t("Яркость подсветки", "Backlight level", "背光亮度"), None, false, false, None);
    let current_level = settings::keyboard_backlight();
    let levels = [
        (BacklightLevel::Off, t("Выкл.", "Off", "关闭")),
        (BacklightLevel::Low, t("Слабая", "Weak", "弱")),
        (BacklightLevel::High, t("Сильная", "Strong", "强")),
    ];
    let mut seg = menu.add_container("", "segment");
    for (level, label) in levels {
        let sched = schedule.clone();
        seg.add_command_ex(
            label,
            Some(Box::new(move || {
                match set_backlight_level(level) {
                    Ok(()) => {
                        settings::set_keyboard_backlight(Some(level));
                        sched.set_manual_override();
                        let (icon, filled) = match level {
                            BacklightLevel::Off => (Icon::KeyboardOff, 0),
                            BacklightLevel::Low => (Icon::KeyboardLight, 1),
                            BacklightLevel::High => (Icon::KeyboardLight, 2),
                        };
                        crate::osd::show(icon, &format!("{} {label}", t("Подсветка:", "Backlight:", "背光：")), Some((filled, 2)));
                    }
                    Err(err) => {
                        crate::log::error_with("Could not change the keyboard backlight", &err);
                        show_error_box(&err.to_string());
                    }
                }
            })),
            None,
            None,
            Some("pill"),
            true,
            current_level == Some(level),
        );
    }

    menu.add_separator();
    menu.add_item(t("Таймаут отключения", "Idle timeout", "闲置超时"), None, false, false, None);
    let current_timeout = settings::keyboard_backlight_timeout();
    // The same seven choices as HONOR PC Manager (settingMenu_keyboard.xml: 5, 10, 15, 30, 60,
    // 300 s and never), all sent with the firmware's idle-timeout command.
    let timeouts: [(u16, &str); 7] = [
        (0, t("Никогда", "Never", "从不")),
        (5, t("5 с", "5 s", "5 秒")),
        (10, t("10 с", "10 s", "10 秒")),
        (15, t("15 с", "15 s", "15 秒")),
        (30, t("30 с", "30 s", "30 秒")),
        (60, t("1 мин", "1 min", "1 分钟")),
        (300, t("5 мин", "5 min", "5 分钟")),
    ];
    let mut timeout_seg = menu.add_container("", "segment");
    for (secs, label) in timeouts {
        timeout_seg.add_command_ex(
            label,
            Some(Box::new(move || {
                match set_backlight_timeout(secs) {
                    Ok(()) => {
                        settings::set_keyboard_backlight_timeout(secs);
                        crate::osd::show(Icon::KeyboardLight, &format!("{} {label}", t("Отключение подсветки:", "Backlight off after:", "背光关闭：")), None);
                    }
                    Err(err) => {
                        crate::log::error_with("Could not change the keyboard backlight timeout", &err);
                        show_error_box(&err.to_string());
                    }
                }
            })),
            None,
            None,
            Some("pill"),
            true,
            current_timeout == secs,
        );
    }

    menu.add_separator();
    let sched_enabled = settings::backlight_schedule_enabled();
    let sched_toggle = schedule.clone();
    menu.add_command_ex(
        t("Подсветка по расписанию", "Backlight schedule", "背光计划"),
        Some(Box::new(move || {
            let on = !settings::backlight_schedule_enabled();
            settings::set_backlight_schedule_enabled(on);
            sched_toggle.settings_changed();
            let text = if on { t("Расписание подсветки включено", "Backlight schedule on", "背光计划已开启") } else { t("Расписание подсветки выключено", "Backlight schedule off", "背光计划已关闭") };
            crate::osd::show(if on { Icon::KeyboardLight } else { Icon::KeyboardOff }, text, None);
        })),
        Some(t(
            "Включает и выключает подсветку в заданные часы",
            "Turns the backlight on and off at set hours",
            "在设定时段开关键盘背光",
        )),
        None,
        Some("switch"),
        true,
        sched_enabled,
    );
    if !sched_enabled {
        return;
    }

    let on_hour = settings::backlight_on_hour();
    let off_hour = settings::backlight_off_hour();
    let mut times_row = menu.add_container("", "row-2");

    let on_badge = format!("{on_hour:02}:00");
    let mut on_sub = times_row.add_submenu_ex(
        t("Включать в", "Turn on", "开启"),
        None,
        Some(&on_badge),
        Some("time"),
    );
    let mut on_grid = on_sub.add_container("", "grid-hours");
    for h in 0..24u32 {
        let sched_h = schedule.clone();
        on_grid.add_command_ex(
            &format!("{h:02}:00"),
            Some(Box::new(move || {
                settings::set_backlight_on_hour(h);
                sched_h.settings_changed();
            })),
            None,
            None,
            Some("pill"),
            true,
            h == on_hour,
        );
    }

    let off_badge = format!("{off_hour:02}:00");
    let mut off_sub = times_row.add_submenu_ex(
        t("Выключать в", "Turn off", "关闭"),
        None,
        Some(&off_badge),
        Some("time"),
    );
    let mut off_grid = off_sub.add_container("", "grid-hours");
    for h in 0..24u32 {
        let sched_h = schedule.clone();
        off_grid.add_command_ex(
            &format!("{h:02}:00"),
            Some(Box::new(move || {
                settings::set_backlight_off_hour(h);
                sched_h.settings_changed();
            })),
            None,
            None,
            Some("pill"),
            true,
            h == off_hour,
        );
    }

    menu.add_item(
        t("Яркость по расписанию", "Scheduled level", "计划亮度"),
        None,
        false,
        false,
        None,
    );
    let sched_level = settings::backlight_schedule_level();
    let mut sched_lvl_seg = menu.add_container("", "segment");
    for (level, label) in [
        (BacklightLevel::Low, t("Слабая", "Weak", "弱")),
        (BacklightLevel::High, t("Сильная", "Strong", "强")),
    ] {
        let sched_l = schedule.clone();
        sched_lvl_seg.add_command_ex(
            label,
            Some(Box::new(move || {
                settings::set_backlight_schedule_level(level);
                sched_l.settings_changed();
            })),
            None,
            None,
            Some("pill"),
            true,
            sched_level == level,
        );
    }
}

/// Keyboard → Keys: actions for the Copilot and YOYO keys (hotkeys.rs).
fn build_keys_section(menu: &mut SubMenu<'_>) {
    use crate::hotkeys::{self, Action};
    menu.add_separator();
    menu.add_item(
        t("Клавиши", "Keys", "按键"),
        None, false, false,
        Some(t("Что делают клавиши Copilot и YOYO", "What the Copilot and YOYO keys do", "Copilot 键与 YOYO 键的功能")),
    );
    for (key, title) in [
        (hotkeys::COPILOT, t("Клавиша Copilot", "Copilot key", "Copilot 键")),
        (hotkeys::YOYO, t("Клавиша YOYO (F11)", "YOYO key (F11)", "YOYO 键 (F11)")),
    ] {
        let current = hotkeys::action(key);
        let app = hotkeys::app_name(key);
        let app_path = settings::key_app(key);
        let badge = match (current, &app) {
            (Action::App, Some(name)) => name.clone(),
            _ => current.label().to_string(),
        };
        let mut page = menu.add_submenu_ex(title, None, Some(&badge), Some("keyboard"));
        for choice in Action::ALL {
            let help = match choice {
                Action::Default if key == hotkeys::COPILOT => Some(t("Открывает Copilot или то, что выбрано в Параметрах Windows", "Opens Copilot or what Windows Settings say", "打开 Copilot 或 Windows 设置中所选项")),
                Action::Default => Some(t("Обычная клавиша F11 (полный экран в браузере)", "A normal F11 key (full screen in browsers)", "普通 F11 键（浏览器全屏）")),
                Action::App => app_path.as_deref().or(Some(t("Выбрать программу…", "Choose a program…", "选择程序…"))),
                _ => None,
            };
            page.add_command_ex(
                choice.label(),
                Some(Box::new(move || {
                    if choice == Action::App {
                        match crate::drivers::transfer::open_file_dialog(
                            t("Программа для клавиши", "Program for the key", "按键启动的程序"),
                            t("Программы", "Programs", "程序"),
                            "*.exe;*.lnk;*.bat;*.cmd",
                        ) {
                            Ok(Some(path)) if crate::unelevated::is_launchable_program(&path.to_string_lossy()) => {
                                settings::set_key_app(key, Some(&path.to_string_lossy()))
                            }
                            Ok(Some(_)) => {
                                show_error_box(t("Выберите программу: файл .exe, .lnk, .bat или .cmd.", "Choose a program: an .exe, .lnk, .bat or .cmd file.", "请选择程序：.exe、.lnk、.bat 或 .cmd 文件。"));
                                return;
                            }
                            Ok(None) => return,
                            Err(err) => {
                                show_error_box(&err);
                                return;
                            }
                        }
                    }
                    settings::set_key_action(key, (choice != Action::Default).then_some(choice.code()));
                    hotkeys::settings_changed();
                })),
                help, None, None, true, current == choice,
            );
        }
    }
}

fn build_touchpad_section(menu: &mut SubMenu<'_>) {
    menu.add_item(
        t("Тактильный отклик", "Haptic feedback", "触觉反馈强度"),
        None,
        false,
        false,
        None,
    );
    let current = settings::touchpad_haptics();
    let mut seg = menu.add_container("", "segment");
    for (lvl, label) in [
        (TouchpadHaptics::Low, t("Низкий", "Low", "低")),
        (TouchpadHaptics::Medium, t("Средний", "Medium", "中")),
        (TouchpadHaptics::High, t("Высокий", "High", "高")),
    ] {
        seg.add_command_ex(
            label,
            Some(Box::new(move || match apply_haptics(lvl) {
                Ok(()) => {
                    let filled = match lvl { TouchpadHaptics::Low => 1, TouchpadHaptics::Medium => 2, TouchpadHaptics::High => 3 };
                    crate::osd::show(Icon::Touchpad, &format!("{} {label}", t("Отклик тачпада:", "Touchpad haptics:", "触控板反馈：")), Some((filled, 3)));
                }
                Err(err) => {
                    crate::log::error(&format!("Failed to set touchpad haptics level: {err}"));
                    show_error_box(&err);
                }
            })),
            None,
            None,
            Some("pill"),
            true,
            current == Some(lvl),
        );
    }

    menu.add_separator();
    menu.add_item(
        t("Жесты по краям", "Edge gestures", "触控板边缘手势"),
        None,
        false,
        false,
        None,
    );
    for (gesture, label, tip) in [
        (
            TouchpadEdgeGesture::Brightness,
            t("Яркость (левый край)", "Brightness (left edge)", "亮度（左边缘）"),
            t(
                "Проведите вверх или вниз у левого края",
                "Slide up or down along the left edge",
                "沿左边缘上下滑动",
            ),
        ),
        (
            TouchpadEdgeGesture::Volume,
            t("Громкость (правый край)", "Volume (right edge)", "音量（右边缘）"),
            t(
                "Проведите вверх или вниз у правого края",
                "Slide up or down along the right edge",
                "沿右边缘上下滑动",
            ),
        ),
    ] {
        let enabled = is_gesture_enabled(gesture);
        menu.add_command_ex(
            label,
            Some(Box::new(move || match apply_gesture(gesture, !enabled) {
                Ok(()) => {
                    let state = if enabled { t("выкл.", "off", "关") } else { t("вкл.", "on", "开") };
                    crate::osd::show(if enabled { Icon::TouchpadOff } else { Icon::Touchpad }, &format!("{label}: {state}"), None);
                }
                Err(err) => {
                    crate::log::error(&format!("Failed to set touchpad edge gesture: {err}"));
                    show_error_box(&err);
                }
            })),
            Some(tip),
            None,
            Some("switch"),
            true,
            enabled,
        );
    }
}

/// The action waiting for a second click (key, when it was armed).
static ARMED: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);
const CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// True when `key` was clicked once within the last 5 s (the control then asks again).
fn is_armed(key: &str) -> bool {
    ARMED.lock().ok().is_some_and(|a| a.as_ref().is_some_and(|(k, at)| k == key && at.elapsed() < CONFIRM_WINDOW))
}

/// Two-step confirmation for actions that are hard to undo: the first click arms the control
/// (its label asks to click again), the second within 5 s runs it.
fn confirmed(key: &str) -> bool {
    let Ok(mut armed) = ARMED.lock() else { return true };
    if armed.as_ref().is_some_and(|(k, at)| k == key && at.elapsed() < CONFIRM_WINDOW) {
        *armed = None;
        true
    } else {
        *armed = Some((key.to_string(), std::time::Instant::now()));
        false
    }
}

thread_local! {
    /// True on the panel's IPC thread while one of its actions runs.
    static IN_PANEL_ACTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The message the last panel action left (text, is error), shown once as a banner.
static PANEL_NOTICE: Mutex<Option<(String, bool)>> = Mutex::new(None);

/// Runs a panel action so that its error and info messages go into the panel (a message box
/// would take the focus and close the panel, or hide behind it).
pub fn run_panel_action(action: &ActionFn) {
    IN_PANEL_ACTION.with(|f| f.set(true));
    action();
    IN_PANEL_ACTION.with(|f| f.set(false));
}

fn report(msg: &str, error: bool) {
    if IN_PANEL_ACTION.with(|f| f.get()) {
        if let Ok(mut slot) = PANEL_NOTICE.lock() {
            *slot = Some((msg.to_string(), error));
        }
        return;
    }
    let text = HSTRING::from(msg);
    let title = HSTRING::from("GrandHonor");
    let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(None, &text, &title, MB_OK | icon | MB_TOPMOST | MB_SETFOREGROUND);
    }
}

/// An error from work a panel action started on a thread: a banner the next snapshot shows,
/// never a message box (it would take the focus from a game or close the panel).
pub fn post_panel_error(msg: &str) {
    if let Ok(mut slot) = PANEL_NOTICE.lock() {
        *slot = Some((msg.to_string(), true));
    }
}

pub fn show_error_box(msg: &str) {
    report(msg, true);
}

fn show_info_box(msg: &str) {
    report(msg, false);
}

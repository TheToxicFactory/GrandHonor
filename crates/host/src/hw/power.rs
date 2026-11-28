use super::wmi::{WmiError, honor_wmi_call};
pub use super::cpu_profile::Profile;
use crate::settings;
use grandhonor_core::locale::t;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_core::{GUID, s, w};

const POWER_UNLOCK_SET_COMMAND: u64 = 0x0000_0F04;
const PERFORMANCE_MODE_GET_COMMAND: u64 = 0x0000_0E04;
pub const WINDOWS_EFFICIENCY: GUID = GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a);
pub const WINDOWS_BALANCED: GUID = GUID::zeroed();
pub const WINDOWS_PERFORMANCE: GUID = GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238);

type GetPowerMode = unsafe extern "system" fn(*mut GUID) -> u32;
type SetPowerMode = unsafe extern "system" fn(*const GUID) -> u32;

struct PowerModeApi {
    ac: (GetPowerMode, SetPowerMode),
    dc: (GetPowerMode, SetPowerMode),
}

fn windows_mode_api(ac: bool) -> Option<(GetPowerMode, SetPowerMode)> {
    static API: OnceLock<Option<PowerModeApi>> = OnceLock::new();
    // Resolve these entry points on demand for Windows builds that lack them.
    // The system DLL remains loaded for the lifetime of the cached function pointers.
    let api = API
        .get_or_init(|| unsafe {
            let module =
                LoadLibraryExW(w!("powrprof.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            let get_ac = GetProcAddress(module, s!("PowerGetUserConfiguredACPowerMode"))?;
            let set_ac = GetProcAddress(module, s!("PowerSetUserConfiguredACPowerMode"))?;
            let get_dc = GetProcAddress(module, s!("PowerGetUserConfiguredDCPowerMode"))?;
            let set_dc = GetProcAddress(module, s!("PowerSetUserConfiguredDCPowerMode"))?;
            Some(PowerModeApi {
                ac: (
                    std::mem::transmute::<_, GetPowerMode>(get_ac),
                    std::mem::transmute::<_, SetPowerMode>(set_ac),
                ),
                dc: (
                    std::mem::transmute::<_, GetPowerMode>(get_dc),
                    std::mem::transmute::<_, SetPowerMode>(set_dc),
                ),
            })
        })
        .as_ref()?;
    Some(if ac { api.ac } else { api.dc })
}

pub fn current_source() -> Option<bool> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    match status.ACLineStatus {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

pub fn windows_mode_for_source(ac: bool) -> Option<GUID> {
    let (get, _) = windows_mode_api(ac)?;
    let mut mode = GUID::zeroed();
    (unsafe { get(&mut mode) } == 0).then_some(mode)
}

pub fn windows_power_mode() -> Option<GUID> {
    windows_mode_for_source(current_source()?)
}

fn unavailable() -> String {
    t("Режим питания Windows недоступен.", "Windows power mode is unavailable.", "Windows 电源模式不可用。").into()
}

pub fn set_windows_mode_for_source(ac: bool, mode: GUID) -> Result<(), String> {
    let (_, set) = windows_mode_api(ac).ok_or_else(unavailable)?;
    if windows_mode_for_source(ac) == Some(mode) {
        return Ok(());
    }
    let result = unsafe { set(&mode) };
    if result != 0 {
        crate::log::error(&format!("Could not set Windows power mode: {result}"));
    }
    if result != 0 || windows_mode_for_source(ac) != Some(mode) {
        return Err(t(
            "Windows не подтвердила новый режим питания.",
            "Windows did not confirm the new power mode.",
            "Windows 未确认新的电源模式。",
        ).into());
    }
    Ok(())
}

pub fn set_windows_power_mode(mode: GUID) -> Result<(), String> {
    set_windows_mode_for_source(current_source().ok_or_else(unavailable)?, mode)
}

pub fn can_enable_performance_mode() -> Result<(), String> {
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return Err(t(
            "Не удалось определить состояние питания.",
            "Could not read system power status.",
            "无法读取电源状态。",
        )
        .to_string());
    }
    if status.ACLineStatus != 1 {
        return Err(t(
            "«Производительный HONOR» доступен только от сети.",
            "HONOR Performance needs the charger.",
            "HONOR 高性能仅在接通电源时可用。",
        )
        .to_string());
    }
    if status.BatteryLifePercent > 100 {
        return Err(t(
            "Не удалось проверить заряд для режима HONOR.",
            "Could not check the battery charge for HONOR Performance.",
            "无法检查 HONOR 高性能所需的电池电量。",
        ).into());
    }
    if status.BatteryLifePercent < 20 {
        return Err(t(
            "Для «Производительного HONOR» заряд должен быть не ниже 20%.",
            "HONOR Performance needs at least 20% charge.",
            "HONOR 高性能要求电量不低于 20%。",
        )
        .to_string());
    }
    Ok(())
}

/// When GrandHonor itself switched the HONOR profile: the firmware then reports the change
/// as an OEM event that must not be mistaken for a Fn+P press. The time is kept in the
/// settings store on the system-wide tick clock, so a change made by another GrandHonor
/// process (a CLI helper or a hardware check) is recognised by the tray host too.
fn system_tick() -> u32 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() as u32 }
}

/// True for ~1.5 s after any GrandHonor process changed the HONOR profile.
pub fn is_software_echo() -> bool {
    settings::profile_write_tick().is_some_and(|at| system_tick().wrapping_sub(at) < 1_500)
}

impl Profile {
    pub fn from_windows_mode(mode: GUID) -> Self {
        if mode == WINDOWS_EFFICIENCY {
            Self::Quiet
        } else if mode == WINDOWS_PERFORMANCE {
            Self::Performance
        } else {
            Self::Balanced
        }
    }

    pub fn windows_mode(self) -> GUID {
        match self {
            Self::Quiet => WINDOWS_EFFICIENCY,
            Self::Balanced => WINDOWS_BALANCED,
            Self::Performance => WINDOWS_PERFORMANCE,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Quiet => t("Тихий", "Quiet", "安静"),
            Self::Balanced => t("Баланс", "Balanced", "平衡"),
            Self::Performance => t("Быстрый", "Fast", "快速"),
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Quiet => Self::Balanced,
            Self::Balanced => Self::Performance,
            Self::Performance => Self::Quiet,
        }
    }

    pub fn icon(self) -> crate::osd::Icon {
        match self {
            Self::Quiet => crate::osd::Icon::Normal,
            Self::Balanced => crate::osd::Icon::Balanced,
            Self::Performance => crate::osd::Icon::Performance,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Bases {
    ac: Profile,
    dc: Profile,
}

impl Bases {
    fn read() -> Option<Self> {
        let value = settings::linked_power_profiles()?;
        let (ac, dc) = value.split_once(',')?;
        Some(Self {
            ac: Profile::from_code(ac.parse().ok()?)?,
            dc: Profile::from_code(dc.parse().ok()?)?,
        })
    }

    fn infer() -> Self {
        let baseline = |ac| {
            let key = crate::presets::source_key(ac);
            let current = windows_mode_for_source(ac);
            crate::hw::app_profiles::prior_mode(ac)
                .or_else(|| {
                    (current == Some(WINDOWS_EFFICIENCY))
                        .then(|| settings::night_mode_saved(key).and_then(Profile::from_code).map(Profile::windows_mode))
                        .flatten()
                })
                .or(current)
                .map(Profile::from_windows_mode)
                .unwrap_or(Profile::Balanced)
        };
        Self { ac: baseline(true), dc: baseline(false) }
    }

    fn for_source(self, ac: bool) -> Profile {
        if ac { self.ac } else { self.dc }
    }

    fn set(&mut self, ac: bool, profile: Profile) -> Result<(), String> {
        *if ac { &mut self.ac } else { &mut self.dc } = profile;
        if settings::set_linked_power_profiles(&format!("{},{}", self.ac as u32, self.dc as u32)) {
            Ok(())
        } else {
            Err(t(
                "Не удалось сохранить профиль питания.",
                "Could not save the power profile.",
                "无法保存电源配置。",
            ).into())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Night,
    Program,
}

fn scenario(night: bool, program: bool) -> Option<Scenario> {
    if night {
        Some(Scenario::Night)
    } else if program {
        Some(Scenario::Program)
    } else {
        None
    }
}

fn current_scenario() -> Option<Scenario> {
    scenario(crate::hw::night_mode::requested_now(), crate::hw::app_profiles::active())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileReason {
    Base,
    Night,
    Program,
    Manual,
}

impl ProfileReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Base => t("Основной профиль", "Base profile", "基础配置"),
            Self::Night => t("Тихие ночи", "Quiet nights", "安静夜间"),
            Self::Program => t("Профиль программы", "App profile", "应用配置"),
            Self::Manual => t("Ручной выбор", "Manual selection", "手动选择"),
        }
    }
}

#[derive(Clone, Copy)]
struct Manual {
    ac: bool,
    profile: Profile,
    scenario: Scenario,
}

fn resolve(base: Profile, ac: bool, active: Option<Scenario>, hold: Option<Manual>) -> (Profile, ProfileReason) {
    if let Some(manual) = hold
        && manual.ac == ac
        && Some(manual.scenario) == active
    {
        return (manual.profile, ProfileReason::Manual);
    }
    match active {
        Some(Scenario::Night) => (Profile::Quiet, ProfileReason::Night),
        Some(Scenario::Program) => (Profile::Performance, ProfileReason::Program),
        None => (base, ProfileReason::Base),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HonorState {
    Standard,
    Performance,
    Hunter,
}

#[derive(Clone, Copy)]
enum HonorResult {
    Actual(HonorState),
    Battery,
    Charge,
    Windows,
    Unavailable,
}

impl HonorResult {
    fn label(self) -> &'static str {
        match self {
            Self::Actual(HonorState::Standard) => t("HONOR: обычный", "HONOR: Standard", "HONOR：标准"),
            Self::Actual(HonorState::Performance) => t("HONOR: производительный", "HONOR: Performance", "HONOR：高性能"),
            Self::Actual(HonorState::Hunter) => t("HONOR: активен HUNTER", "HONOR: HUNTER active", "HONOR：HUNTER 已开启"),
            Self::Battery => t("HONOR: требуется зарядка", "HONOR: charger required", "HONOR：需要接通电源"),
            Self::Charge => t("HONOR: нужен заряд от 20%", "HONOR: needs at least 20% charge", "HONOR：电量需至少 20%"),
            Self::Windows => t("HONOR: Windows не подтвердила режим", "HONOR: Windows did not confirm the mode", "HONOR：Windows 未确认模式"),
            Self::Unavailable => t("HONOR: режим недоступен", "HONOR: mode unavailable", "HONOR：模式不可用"),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Applied {
    pub profile: Profile,
    pub reason: ProfileReason,
    ac: bool,
    scheme: Option<GUID>,
    windows: Option<GUID>,
    cpu: bool,
    honor: HonorResult,
}

impl Applied {
    pub fn detail(self) -> String {
        let windows = self.windows.map_or("—", |m| {
            if m == WINDOWS_EFFICIENCY {
                t("Экономия", "Efficiency", "能效")
            } else if m == WINDOWS_PERFORMANCE {
                t("Производительность", "Performance", "性能")
            } else {
                t("Баланс", "Balanced", "平衡")
            }
        });
        let cpu = if self.cpu {
            t("CPU: применён", "CPU: applied", "CPU：已应用")
        } else {
            t("CPU: не применён", "CPU: not applied", "CPU：未应用")
        };
        format!("Windows: {windows} · {cpu} · {}", self.honor.label())
    }

    pub fn warning(self) -> Option<&'static str> {
        if self.windows != Some(self.profile.windows_mode()) {
            Some(t("Windows не подтвердила профиль", "Windows did not confirm the profile", "Windows 未确认配置"))
        } else if !self.cpu {
            Some(t("Не удалось применить параметры процессора", "Could not apply the processor settings", "无法应用处理器设置"))
        } else if matches!(self.honor, HonorResult::Actual(HonorState::Performance))
            || self.profile != Profile::Performance && matches!(self.honor, HonorResult::Actual(_))
        {
            None
        } else {
            Some(self.honor.label())
        }
    }
}

#[derive(Default)]
struct Controller {
    manual: Option<Manual>,
}

static CONTROL: Mutex<Controller> = Mutex::new(Controller { manual: None });
static APPLIED: Mutex<Option<Applied>> = Mutex::new(None);
static SUSPENDED: AtomicBool = AtomicBool::new(false);
static ON_CHANGED: OnceLock<Arc<dyn Fn() + Send + Sync>> = OnceLock::new();

pub fn on_changed(callback: Arc<dyn Fn() + Send + Sync>) {
    let _ = ON_CHANGED.set(callback);
}

/// Serialize other changes that must re-activate the active Windows power scheme.
pub fn with_power_scheme_write<R>(operation: impl FnOnce() -> R) -> R {
    let _controller = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    operation()
}

pub fn notify_applied(applied: Applied) {
    let label = applied.warning().unwrap_or_else(|| applied.profile.label());
    let icon = if applied.warning().is_some() { crate::osd::Icon::Warning } else { applied.profile.icon() };
    crate::osd::notify(icon, label);
}

pub fn linked() -> bool {
    Bases::read().is_some()
}

pub(super) fn if_legacy<R>(action: impl FnOnce() -> R) -> Option<R> {
    let _controller = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    (!linked() && !SUSPENDED.load(Ordering::Acquire)).then(action)
}

pub fn restore_legacy_cpu_at_start() {
    let _ = if_legacy(|| {
        if super::cpu_profile::chosen(true).is_some() || super::cpu_profile::chosen(false).is_some() {
            let _ = super::cpu_profile::apply();
        }
    });
}

pub(super) fn begin_legacy_app_profile() -> Option<(bool, GUID)> {
    if_legacy(|| {
        let source = current_source()?;
        let mode = windows_mode_for_source(source)?;
        if mode == WINDOWS_PERFORMANCE {
            return None;
        }
        match set_windows_mode_for_source(source, WINDOWS_PERFORMANCE) {
            Ok(()) => Some((source, mode)),
            Err(err) => {
                crate::log::error(&format!("App profile: {err}"));
                None
            }
        }
    }).flatten()
}

pub(super) fn restore_legacy_app_profile(source: bool, mode: GUID) {
    let _ = if_legacy(|| {
        if windows_mode_for_source(source) == Some(WINDOWS_PERFORMANCE)
            && let Err(err) = set_windows_mode_for_source(source, mode)
        {
            crate::log::error(&format!("App profile restore: {err}"));
        }
    });
}

pub fn saved_or_current(ac: bool) -> Option<Profile> {
    Bases::read().map(|bases| bases.for_source(ac))
        .or_else(|| windows_mode_for_source(ac).map(Profile::from_windows_mode))
}

pub fn applied() -> Option<Applied> {
    if !linked() {
        return None;
    }
    let ac = current_source()?;
    let mut last = APPLIED.lock().ok()?.filter(|last| last.ac == ac)?;
    last.windows = windows_mode_for_source(ac);
    last.cpu &= last.scheme == super::cpu_profile::active_scheme_id();
    Some(last)
}

pub fn current_profile() -> Profile {
    applied().map(|last| last.profile)
        .or_else(|| windows_power_mode().map(Profile::from_windows_mode))
        .unwrap_or(Profile::Balanced)
}

fn read_honor() -> Option<HonorState> {
    let bytes = honor_wmi_call(PERFORMANCE_MODE_GET_COMMAND).ok()?;
    let active = match bytes.get(1).copied()? {
        0 => HonorState::Standard,
        1 => HonorState::Performance,
        3 => HonorState::Hunter,
        _ => return None,
    };
    settings::set_performance_mode_active(active == HonorState::Performance);
    settings::set_hunter_mode_active(active == HonorState::Hunter);
    Some(active)
}

fn apply_honor(ac: bool, profile: Profile, windows_ok: bool) -> HonorResult {
    let blocked = if profile != Profile::Performance {
        None
    } else if !windows_ok {
        Some(HonorResult::Windows)
    } else if !ac {
        Some(HonorResult::Battery)
    } else if can_enable_performance_mode().is_err() {
        Some(HonorResult::Charge)
    } else {
        None
    };
    if current_source() != Some(ac) || SUSPENDED.load(Ordering::Acquire) {
        return HonorResult::Unavailable;
    }
    let Some(current) = read_honor() else { return blocked.unwrap_or(HonorResult::Unavailable) };
    if current == HonorState::Hunter {
        return HonorResult::Actual(current);
    }
    let enable = profile == Profile::Performance && blocked.is_none();
    let expected = if enable { HonorState::Performance } else { HonorState::Standard };
    if current != expected {
        if current_source() != Some(ac) || SUSPENDED.load(Ordering::Acquire) {
            return HonorResult::Unavailable;
        }
        if set_power_unlock(enable).is_err() || read_honor() != Some(expected) {
            return HonorResult::Unavailable;
        }
    }
    blocked.unwrap_or(HonorResult::Actual(expected))
}

fn run_profile(controller: &mut Controller, bases: Bases, ac: bool) -> Applied {
    let active = current_scenario();
    controller.manual = controller.manual.filter(|m| m.ac == ac && Some(m.scenario) == active);
    let (profile, reason) = resolve(bases.for_source(ac), ac, active, controller.manual);
    let scheme = super::cpu_profile::active_scheme_id();
    let cpu = super::cpu_profile::apply_for_source(ac, profile);
    let windows = set_windows_mode_for_source(ac, profile.windows_mode());
    let honor = apply_honor(ac, profile, windows.is_ok());
    let result = Applied { profile, reason, ac, scheme, windows: windows_mode_for_source(ac), cpu, honor };
    if let Ok(mut last) = APPLIED.lock() {
        *last = Some(result);
    }
    if let Some(on_changed) = ON_CHANGED.get() {
        on_changed();
    }
    result
}

/// A selection made through the panel, tray or Fn+P. The first selection links the layers.
pub fn choose(profile: Profile) -> Result<Applied, String> {
    achieve(|_| profile)
}

pub fn cycle_profile() -> Result<Applied, String> {
    achieve(Profile::next)
}

fn achieve(select: impl FnOnce(Profile) -> Profile) -> Result<Applied, String> {
    let mut controller = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if SUSPENDED.load(Ordering::Acquire) {
        return Err(unavailable());
    }
    let ac = current_source().ok_or_else(unavailable)?;
    let profile = select(current_profile());
    let mut bases = Bases::read().unwrap_or_else(Bases::infer);
    bases.set(ac, profile)?;
    controller.manual = current_scenario().map(|scenario| Manual { ac, profile, scenario });
    Ok(run_profile(&mut controller, bases, ac))
}

/// Sets just the named source's baseline. Automatic processing never overwrites it.
pub fn apply_preset_profile(ac: bool, profile: Profile) -> Result<Option<Applied>, String> {
    let mut controller = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut bases) = Bases::read() {
        bases.set(ac, profile)?;
        if current_source() == Some(ac) {
            controller.manual = current_scenario().map(|scenario| Manual { ac, profile, scenario });
            return Ok(Some(run_profile(&mut controller, bases, ac)));
        }
        Ok(None)
    } else {
        set_windows_mode_for_source(ac, profile.windows_mode())?;
        Ok(None)
    }
}

pub fn apply_automatic_legacy_preset(ac: bool, profile: Profile) {
    let _ = if_legacy(|| {
        if let Err(err) = set_windows_mode_for_source(ac, profile.windows_mode()) {
            crate::log::error(&format!("Preset power mode: {err}"));
        }
    });
}

pub fn reconcile(force: bool) -> Option<Applied> {
    let mut controller = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let (bases, ac) = (Bases::read()?, current_source()?);
    if SUSPENDED.load(Ordering::Acquire) {
        return None;
    }
    let active = current_scenario();
    controller.manual = controller.manual.filter(|m| m.ac == ac && Some(m.scenario) == active);
    let (profile, reason) = resolve(bases.for_source(ac), ac, active, controller.manual);
    if !force && applied().is_some_and(|last| {
        last.profile == profile && last.reason == reason
            && last.scheme == super::cpu_profile::active_scheme_id()
    }) {
        return None;
    }
    Some(run_profile(&mut controller, bases, ac))
}

pub fn mark_suspended() {
    SUSPENDED.store(true, Ordering::Release);
}

pub fn mark_awake() {
    SUSPENDED.store(false, Ordering::Release);
}

pub fn set_power_unlock(enabled: bool) -> Result<(), WmiError> {
    if enabled {
        can_enable_performance_mode().map_err(WmiError::Unavailable)?;
        if settings::hunter_mode_active() {
            return Err(WmiError::Unavailable(
                t("Включён режим HUNTER.", "HUNTER mode is on.", "HUNTER 模式已开启。").to_string(),
            ));
        }
    }
    let command = POWER_UNLOCK_SET_COMMAND | ((if enabled { 1u64 } else { 0u64 }) << 16);
    settings::set_profile_write_tick(system_tick());
    honor_wmi_call(command)?;
    Ok(())
}

/// Turns HONOR Performance off for now (sleep, battery) without changing the user's choice.
pub fn suspend_performance() -> Result<(), WmiError> {
    let _controller = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if settings::hunter_mode_active() || !settings::performance_mode_active() {
        return Ok(());
    }
    set_power_unlock(false)?;
    if read_honor() != Some(HonorState::Standard) {
        return Err(WmiError::Unavailable(t(
            "Ноутбук не подтвердил обычный режим HONOR.",
            "The laptop did not confirm HONOR Standard.",
            "笔记本未确认 HONOR 标准模式。",
        ).into()));
    }
    Ok(())
}

/// Brings HONOR Performance back when the user chose it and the charger, 20 % and no HUNTER
/// allow it: at start-up (the firmware forgets it on reboot), after sleep and on plug-in.
/// Returns true when it was turned on.
pub fn restore_performance_preference() -> bool {
    // HONOR Performance belongs with the Windows Performance mode (that is what Fn+P sets):
    // after a restart, sleep or plug-in under Efficiency or Balanced it stays off. This holds
    // across restarts and for every way the Windows mode was changed.
    let _controller = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    if linked()
        || current_profile() != Profile::Performance
        || !settings::power_unlock_preference()
        || settings::performance_mode_active()
        || settings::hunter_mode_active()
        || can_enable_performance_mode().is_err()
    {
        return false;
    }
    match set_power_unlock(true) {
        Ok(()) => {
            settings::set_performance_mode_active(true);
            crate::log::info("HONOR Performance restored from the saved choice");
            true
        }
        Err(err) => {
            crate::log::error_with("Could not restore HONOR Performance", &err);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_outranks_apps_and_manual_holds_only_for_its_source_and_scenario() {
        let night = scenario(true, true);
        assert_eq!(night, Some(Scenario::Night));
        let manual = Some(Manual { ac: true, profile: Profile::Balanced, scenario: Scenario::Night });
        assert_eq!(resolve(Profile::Quiet, true, night, manual), (Profile::Balanced, ProfileReason::Manual));
        assert_eq!(resolve(Profile::Balanced, false, night, manual), (Profile::Quiet, ProfileReason::Night));
        assert_eq!(resolve(Profile::Balanced, true, scenario(false, true), manual), (Profile::Performance, ProfileReason::Program));
        assert_eq!(resolve(Profile::Balanced, true, None, manual), (Profile::Balanced, ProfileReason::Base));
    }
}

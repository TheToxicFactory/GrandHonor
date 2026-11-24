use grandhonor_core::protocol::{
    BacklightLevel, BatteryMode, TouchpadEdgeGesture, TouchpadHaptics,
};
use grandhonor_core::locale::AppLanguage;
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_core::{HSTRING, PCWSTR};

const SETTINGS_SUBKEY: &str = r"Software\GrandHonor";
const DEFAULT_BACKLIGHT_TIMEOUT: u16 = 60;

static GATE: OnceLock<Mutex<()>> = OnceLock::new();

fn gate() -> &'static Mutex<()> {
    GATE.get_or_init(|| Mutex::new(()))
}

#[derive(Clone, Debug)]
pub struct TooltipState {
    pub sensor_snapshot: Option<String>,
    pub performance_mode_active: bool,
    pub hunter_mode_active: bool,
    pub keyboard_backlight: Option<BacklightLevel>,
    pub battery_protection: Option<BatteryMode>,
}

struct OwnedRegKey(HKEY);

impl Drop for OwnedRegKey {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }
}

fn open_user_key(write: bool) -> Option<OwnedRegKey> {
    let subkey = HSTRING::from(SETTINGS_SUBKEY);
    let mut handle = HKEY::default();
    let status = unsafe {
        if write {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &subkey,
                Some(0),
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                None,
                &mut handle,
                None,
            )
        } else {
            RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, Some(0), KEY_READ, &mut handle)
        }
    };
    if status == ERROR_SUCCESS {
        Some(OwnedRegKey(handle))
    } else {
        None
    }
}

pub fn read_hklm_string(subkey: &str, name: &str) -> Option<String> {
    let sub = HSTRING::from(subkey);
    let mut handle = HKEY::default();
    let status = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &sub, Some(0), KEY_READ, &mut handle) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let key = OwnedRegKey(handle);
    read_reg_string(key.0, name)
}

pub fn read_hklm_dword(subkey: &str, name: &str) -> Option<u32> {
    let sub = HSTRING::from(subkey);
    let mut handle = HKEY::default();
    let status = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, &sub, Some(0), KEY_READ, &mut handle) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let key = OwnedRegKey(handle);
    read_reg_dword(key.0, name)
}

/// One stored setting, as kept in `HKCU\Software\GrandHonor`.
#[derive(Clone, Debug, PartialEq)]
pub enum StoredValue {
    Dword(u32),
    Text(String),
}

/// Runtime state, not user choices: never exported or imported.
const RUNTIME_VALUES: [&str; 11] = [
    "ProfileWriteTick",
    "AppProfileSaved",
    "NightModeSavedAc",
    "FullChargeOnce",
    "BatteryWear",
    "NightModeSavedDc",
    "PerformanceModeActive",
    "HunterModeActive",
    "CpuCapOriginalAc",
    "CpuCapOriginalDc",
    "SensorSnapshot",
];

pub fn is_user_setting(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        && !name.starts_with('.')
        && !RUNTIME_VALUES.contains(&name)
}

/// Every user setting currently stored (runtime state excluded), sorted by name.
pub fn all_user_settings() -> Vec<(String, StoredValue)> {
    store::all().into_iter().filter(|(n, _)| is_user_setting(n)).collect()
}

/// Writes imported settings; names that are not user settings are ignored.
pub fn store_user_settings(values: &[(String, StoredValue)]) -> usize {
    let mut written = 0;
    for (name, value) in values.iter().filter(|(n, _)| is_user_setting(n)) {
        if matches!(value, StoredValue::Text(s) if s.len() > 256) {
            continue;
        }
        store::set(name, Some(value.clone()));
        written += 1;
    }
    written
}

/// Windows 11 puts every new tray icon into the hidden overflow (^), so GrandHonor looked
/// "closed" after its panel closed. Once — only while the user has never decided for this
/// icon (`IsPromoted` absent in `HKCU\Control Panel\NotifyIconSettings\<id>`) — show it on
/// the taskbar. Returns true when the entry was found (promoted now or decided by the user).
pub fn promote_tray_icon_once(exe: &std::path::Path) -> bool {
    use windows::Win32::System::Registry::RegEnumKeyExW;
    if read_u32("TrayIconPromoted") == Some(1) {
        return true;
    }
    let root = HSTRING::from(r"Control Panel\NotifyIconSettings");
    let mut handle = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &root, Some(0), KEY_READ, &mut handle) } != ERROR_SUCCESS {
        return false;
    }
    let root_key = OwnedRegKey(handle);
    // Explorer stores Program Files paths with the FOLDERID_ProgramFilesX64 GUID.
    let program_files = std::env::var("ProgramW6432").or_else(|_| std::env::var("ProgramFiles")).unwrap_or_default();
    let expand = |p: &str| p.replacen("{6D809377-6AF0-444B-8957-A3773F02200E}", &program_files, 1);
    let ours = exe.to_string_lossy().to_lowercase();
    let mut found = false;
    for index in 0..512u32 {
        let mut buf = [0u16; 128];
        let mut len = buf.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(root_key.0, index, Some(windows_core::PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None)
        };
        if status != ERROR_SUCCESS {
            break;
        }
        let sub = HSTRING::from(format!(r"Control Panel\NotifyIconSettings\{}", String::from_utf16_lossy(&buf[..len as usize])));
        let mut icon = HKEY::default();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &sub, Some(0), KEY_READ | KEY_WRITE, &mut icon) } != ERROR_SUCCESS {
            continue;
        }
        let icon = OwnedRegKey(icon);
        let Some(path) = read_reg_string(icon.0, "ExecutablePath") else { continue };
        if expand(&path).to_lowercase() != ours {
            continue;
        }
        found = true;
        if read_reg_dword(icon.0, "IsPromoted").is_none() {
            let _ = write_reg_dword(icon.0, "IsPromoted", 1);
            crate::log::info("Tray icon shown on the taskbar (first run)");
        }
    }
    if found {
        write_u32("TrayIconPromoted", Some(1));
    }
    found
}

/// Look of the panel and the on-screen notices (System → Style).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    /// Opaque OLED black panel and HUD (default).
    Graphite,
    /// Translucent panel over Windows acrylic and a frosted HUD.
    Glass,
}

impl Appearance {
    pub fn code(self) -> &'static str {
        match self {
            Self::Graphite => "graphite",
            Self::Glass => "glass",
        }
    }
}

pub fn appearance() -> Appearance {
    match read_str("Appearance").as_deref() {
        Some("glass") => Appearance::Glass,
        _ => Appearance::Graphite,
    }
}

pub fn set_appearance(value: Appearance) {
    write_str("Appearance", Some(value.code()));
}

/// Smart charging (see `hw/smart_charge.rs`), default off.
pub fn smart_charge_enabled() -> bool {
    read_u32("SmartCharge") == Some(1)
}

pub fn set_smart_charge_enabled(enabled: bool) {
    write_u32("SmartCharge", Some(u32::from(enabled)));
}

/// True while smart charging holds the battery at 70–90 % on a long charger session.
pub fn smart_charge_holding() -> bool {
    read_u32("SmartChargeHolding") == Some(1)
}

pub fn set_smart_charge_holding(holding: bool) {
    write_u32("SmartChargeHolding", holding.then_some(1));
}

/// Display → Eye comfort mode (see `hw/display_filter.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayFilter {
    Off,
    EyeComfort,
    Ebook,
    Comfort,
}

impl DisplayFilter {
    pub fn code(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::EyeComfort => "eye",
            Self::Ebook => "ebook",
            Self::Comfort => "comfort",
        }
    }
}

pub fn display_filter() -> DisplayFilter {
    match read_str("DisplayFilter").as_deref() {
        Some("eye") => DisplayFilter::EyeComfort,
        Some("ebook") => DisplayFilter::Ebook,
        Some("comfort") => DisplayFilter::Comfort,
        _ => DisplayFilter::Off,
    }
}

pub fn set_display_filter(value: DisplayFilter) {
    write_str("DisplayFilter", Some(value.code()));
}

/// Display → Colour profile (see `hw/display_filter.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorProfile {
    Native,
    Srgb,
    DisplayP3,
}

pub fn color_profile() -> ColorProfile {
    match read_str("ColorProfile").as_deref() {
        Some("srgb") => ColorProfile::Srgb,
        Some("p3") => ColorProfile::DisplayP3,
        _ => ColorProfile::Native,
    }
}

pub fn set_color_profile(value: ColorProfile) {
    write_str("ColorProfile", Some(match value {
        ColorProfile::Native => "native",
        ColorProfile::Srgb => "srgb",
        ColorProfile::DisplayP3 => "p3",
    }));
}

/// How warm Eye comfort is, 1 (barely) to 10 (warmest). Earlier versions stored three levels.
pub const WARMTH_MAX: u32 = 10;

pub fn eye_warmth() -> u32 {
    if let Some(v) = read_u32("EyeComfortWarmth") {
        return v.clamp(1, WARMTH_MAX);
    }
    match read_str("EyeComfortLevel").as_deref() {
        Some("light") => 3,
        Some("strong") => 8,
        _ => 5,
    }
}

pub fn set_eye_warmth(value: u32) {
    write_u32("EyeComfortWarmth", Some(value.clamp(1, WARMTH_MAX)));
}

/// Display → Eye break reminder interval in minutes; 0 = off (default).
pub fn break_reminder_minutes() -> u32 {
    read_u32("BreakReminder").unwrap_or(0)
}

pub fn set_break_reminder_minutes(minutes: u32) {
    write_u32("BreakReminder", Some(minutes));
}

/// Battery notices: charge limit reached, low battery, charge lost asleep (default on).
pub fn charge_notices_enabled() -> bool {
    read_u32("ChargeNotices") != Some(0)
}

pub fn set_charge_notices_enabled(enabled: bool) {
    write_u32("ChargeNotices", Some(u32::from(enabled)));
}

/// True while "Charge to 100 % once" has lifted the limit (`hw/battery_alerts.rs`).
pub fn full_charge_once() -> bool {
    read_u32("FullChargeOnce") == Some(1)
}

pub fn set_full_charge_once(active: bool) {
    write_u32("FullChargeOnce", active.then_some(1));
}

/// Monthly full-charge capacity, "yyyymm=mWh,..." (`hw/battery_alerts.rs`).
pub fn battery_wear() -> Option<String> {
    read_str("BatteryWear")
}

pub fn set_battery_wear(value: Option<&str>) {
    write_str("BatteryWear", value);
}

/// Power → Quiet nights: Efficiency mode between two hours (`hw/night_mode.rs`).
pub fn night_mode_enabled() -> bool {
    read_u32("NightMode") == Some(1)
}

pub fn set_night_mode_enabled(enabled: bool) {
    write_u32("NightMode", Some(u32::from(enabled)));
}

pub fn night_mode_hours() -> (u32, u32) {
    (read_u32("NightModeFrom").unwrap_or(23).min(23), read_u32("NightModeTo").unwrap_or(7).min(23))
}

pub fn set_night_mode_hours(from: u32, to: u32) {
    write_u32("NightModeFrom", Some(from.min(23)));
    write_u32("NightModeTo", Some(to.min(23)));
}

/// The Windows power mode to put back when a quiet night ends, per power source ("Ac"/"Dc"):
/// Windows keeps one mode for each (runtime state).
pub fn night_mode_saved(source: &str) -> Option<u32> {
    read_u32(&format!("NightModeSaved{source}"))
}

pub fn set_night_mode_saved(source: &str, mode: Option<u32>) {
    write_u32(&format!("NightModeSaved{source}"), mode);
}

/// What a remappable key does (`hotkeys.rs`): "Copilot" or "Yoyo" → action code.
pub fn key_action(key: &str) -> Option<String> {
    read_str(&format!("KeyAction{key}"))
}

pub fn set_key_action(key: &str, action: Option<&str>) {
    write_str(&format!("KeyAction{key}"), action);
}

/// The program a key starts when its action is "app".
pub fn key_app(key: &str) -> Option<String> {
    read_str(&format!("KeyApp{key}"))
}

pub fn set_key_app(key: &str, path: Option<&str>) {
    write_str(&format!("KeyApp{key}"), path);
}

/// What an app profile will restore ("mode,rate"); runtime state, survives a crash.
pub fn app_profile_saved() -> Option<String> {
    read_str("AppProfileSaved")
}

pub fn set_app_profile_saved(value: Option<&str>) {
    write_str("AppProfileSaved", value);
}

/// Per-app profiles: program file names, "game.exe|other.exe" (`hw/app_profiles.rs`).
pub fn app_profiles() -> Vec<String> {
    read_str("AppProfiles")
        .map(|s| s.split('|').map(str::trim).filter(|n| !n.is_empty()).map(str::to_ascii_lowercase).collect())
        .unwrap_or_default()
}

pub fn set_app_profiles(names: &[String]) {
    let text = names.join("|");
    write_str("AppProfiles", (!text.is_empty()).then_some(text.as_str()));
}

/// On-screen notices for Fn keys and firmware events (default on).
pub fn osd_enabled() -> bool {
    read_u32("OsdEnabled") != Some(0)
}

pub fn set_osd_enabled(enabled: bool) {
    write_u32("OsdEnabled", Some(u32::from(enabled)));
}

/// Like `write_hklm_dword`, but creates the key first when it is missing.
pub fn create_hklm_dword(subkey: &str, name: &str, value: u32) -> Result<(), WIN32_ERROR> {
    let sub = HSTRING::from(subkey);
    let mut handle = HKEY::default();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &sub,
            Some(0),
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            None,
            &mut handle,
            None,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(status);
    }
    let key = OwnedRegKey(handle);
    write_reg_dword(key.0, name, value)
}

fn read_reg_string(key: HKEY, name: &str) -> Option<String> {
    let value_name = HSTRING::from(name);
    let mut ty = REG_VALUE_TYPE::default();
    let mut bytes = 0u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut bytes),
        )
    };
    if status != ERROR_SUCCESS || ty != REG_SZ || bytes < 2 {
        return None;
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut bytes),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn read_reg_dword(key: HKEY, name: &str) -> Option<u32> {
    let value_name = HSTRING::from(name);
    let mut ty = REG_VALUE_TYPE::default();
    let mut val = 0u32;
    let mut bytes = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut ty),
            Some(&mut val as *mut u32 as *mut u8),
            Some(&mut bytes),
        )
    };
    if status == ERROR_SUCCESS && ty == REG_DWORD {
        Some(val)
    } else {
        None
    }
}

fn write_reg_string(key: HKEY, name: &str, value: &str) -> Result<(), WIN32_ERROR> {
    let value_name = HSTRING::from(name);
    let mut wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let bytes = unsafe {
        std::slice::from_raw_parts(wide.as_mut_ptr() as *const u8, wide.len() * 2)
    };
    let status = unsafe { RegSetValueExW(key, &value_name, Some(0), REG_SZ, Some(bytes)) };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(status)
    }
}

fn write_reg_dword(key: HKEY, name: &str, value: u32) -> Result<(), WIN32_ERROR> {
    let value_name = HSTRING::from(name);
    let bytes = value.to_le_bytes();
    let status = unsafe { RegSetValueExW(key, &value_name, Some(0), REG_DWORD, Some(&bytes)) };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(status)
    }
}

fn delete_reg_value(key: HKEY, name: &str) {
    let value_name = HSTRING::from(name);
    let status = unsafe { RegDeleteValueW(key, &value_name) };
    if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
        crate::log::error(&format!("RegDeleteValueW({name}) failed: {:?}", status));
    }
}

fn read_str(name: &str) -> Option<String> {
    match store::get(name)? {
        StoredValue::Text(s) => Some(s),
        StoredValue::Dword(_) => None,
    }
}

// Writes are skipped when the stored value already matches, so repeated sensor samples
// and state refreshes cause no disk writes.
fn write_str(name: &str, value: Option<&str>) {
    store::set(name, value.map(|v| StoredValue::Text(v.to_string())));
}

fn read_u32(name: &str) -> Option<u32> {
    match store::get(name)? {
        StoredValue::Dword(v) => Some(v),
        StoredValue::Text(_) => None,
    }
}

fn write_u32(name: &str, value: Option<u32>) {
    store::set(name, value.map(StoredValue::Dword));
}

/// Mirrors one value into `HKCU\Software\GrandHonor`, so earlier GrandHonor versions (which
/// read the registry) see the same settings after a rollback.
fn mirror_to_registry(name: &str, value: Option<&StoredValue>) {
    let Ok(_g) = gate().lock() else { return };
    let Some(key) = open_user_key(true) else { return };
    match value {
        Some(StoredValue::Dword(v)) => {
            if read_reg_dword(key.0, name) != Some(*v) {
                let _ = write_reg_dword(key.0, name, *v);
            }
        }
        Some(StoredValue::Text(s)) => {
            if read_reg_string(key.0, name).as_deref() != Some(s.as_str()) {
                let _ = write_reg_string(key.0, name, s);
            }
        }
        None => delete_reg_value(key.0, name),
    }
}

/// Every value in the registry key (user choices and runtime state), for the one-time move
/// into the settings file.
fn registry_values() -> Vec<(String, StoredValue)> {
    use windows::Win32::System::Registry::RegEnumValueW;
    let Ok(_g) = gate().lock() else { return Vec::new() };
    let Some(key) = open_user_key(false) else { return Vec::new() };
    let mut out = Vec::new();
    for index in 0..1024u32 {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let status = unsafe {
            RegEnumValueW(key.0, index, Some(windows_core::PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None)
        };
        if status != ERROR_SUCCESS {
            break;
        }
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        if name == "SensorSnapshot" {
            continue;
        }
        if let Some(v) = read_reg_dword(key.0, &name) {
            out.push((name, StoredValue::Dword(v)));
        } else if let Some(s) = read_reg_string(key.0, &name) {
            out.push((name, StoredValue::Text(s)));
        }
    }
    out
}

/// The settings file: `%APPDATA%\GrandHonor\settings.json`, the one place every GrandHonor
/// version reads and writes. Same JSON format as an export. Writes are atomic (temp file +
/// rename) under a cross-process mutex; readers reload only when the file changed.
pub mod store {
    use super::StoredValue;
    use serde_json::{Map, Value, json};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::SystemTime;

    pub const FORMAT: &str = "GrandHonor settings";

    struct Cache {
        stamp: Option<(SystemTime, u64)>,
        values: BTreeMap<String, StoredValue>,
    }

    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

    pub fn path() -> PathBuf {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("GrandHonor")
            .join("settings.json")
    }

    fn stamp() -> Option<(SystemTime, u64)> {
        let meta = std::fs::metadata(path()).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    /// Serializes writers across GrandHonor processes (tray host, CLI helpers).
    struct ProcessLock(isize);
    impl Drop for ProcessLock {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe { ReleaseMutex(self.0) };
            }
        }
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateMutexW(attrs: *const std::ffi::c_void, owner: i32, name: *const u16) -> isize;
        fn WaitForSingleObject(handle: isize, ms: u32) -> u32;
        fn ReleaseMutex(handle: isize) -> i32;
    }
    fn process_lock() -> ProcessLock {
        static HANDLE: OnceLock<isize> = OnceLock::new();
        let h = *HANDLE.get_or_init(|| {
            let name: Vec<u16> = "Local\\GrandHonor.Settings\0".encode_utf16().collect();
            unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) }
        });
        if h != 0 && unsafe { WaitForSingleObject(h, 3_000) } <= 0x80 {
            ProcessLock(h)
        } else {
            ProcessLock(0)
        }
    }

    pub fn parse(text: &str) -> Option<BTreeMap<String, StoredValue>> {
        let root: Value = serde_json::from_str(text).ok()?;
        if root["format"].as_str() != Some(FORMAT) {
            return None;
        }
        let map = root["settings"].as_object()?;
        Some(
            map.iter()
                .filter_map(|(k, v)| match v {
                    Value::Number(n) => n.as_u64().filter(|&n| n <= u32::MAX as u64).map(|n| (k.clone(), StoredValue::Dword(n as u32))),
                    Value::String(s) => Some((k.clone(), StoredValue::Text(s.clone()))),
                    _ => None,
                })
                .collect(),
        )
    }

    pub fn render(values: &BTreeMap<String, StoredValue>) -> String {
        let mut map = Map::new();
        for (k, v) in values {
            map.insert(k.clone(), match v {
                StoredValue::Dword(n) => json!(n),
                StoredValue::Text(s) => json!(s),
            });
        }
        serde_json::to_string_pretty(&json!({ "format": FORMAT, "version": env!("CARGO_PKG_VERSION"), "settings": map }))
            .unwrap_or_default()
    }

    fn write_file(values: &BTreeMap<String, StoredValue>) -> bool {
        // The host is elevated and this folder is the user's: never write through a link.
        crate::safe_fs::write_atomic(&path(), render(values).as_bytes()).is_ok()
    }

    /// Loads the file; migrates from the registry when it does not exist yet, and recovers
    /// from the registry copy when it is unreadable (the broken file is kept aside).
    fn load() -> BTreeMap<String, StoredValue> {
        let file = path();
        match std::fs::read_to_string(&file) {
            Ok(text) => match parse(&text) {
                Some(values) => return values,
                None => {
                    let _ = crate::safe_fs::rename(&file, &file.with_extension("json.broken"));
                    crate::log::error("settings.json was unreadable; restored from the registry copy");
                }
            },
            Err(_) => {}
        }
        let values: BTreeMap<_, _> = super::registry_values().into_iter().collect();
        let _lock = process_lock();
        if !file.exists() {
            let _ = write_file(&values);
        }
        values
    }

    fn with_cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
        let mut slot = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let now = stamp();
        let stale = match slot.as_ref() {
            Some(cache) => cache.stamp != now || now.is_none(),
            None => true,
        };
        if stale {
            let values = load();
            *slot = Some(Cache { stamp: stamp(), values });
        }
        f(slot.as_mut().expect("cache filled"))
    }

    pub fn get(name: &str) -> Option<StoredValue> {
        with_cache(|c| c.values.get(name).cloned())
    }

    pub fn all() -> Vec<(String, StoredValue)> {
        with_cache(|c| c.values.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    /// Sets (Some) or removes (None) a value. No write when nothing changes.
    pub fn set(name: &str, value: Option<StoredValue>) {
        let changed = {
            let _lock = process_lock();
            with_cache(|c| {
                if c.values.get(name) == value.as_ref() {
                    return false;
                }
                match &value {
                    Some(v) => {
                        c.values.insert(name.to_string(), v.clone());
                    }
                    None => {
                        c.values.remove(name);
                    }
                }
                let ok = write_file(&c.values);
                c.stamp = stamp();
                if !ok {
                    crate::log::error(&format!("Could not save setting {name}"));
                }
                true
            })
        };
        if changed {
            super::mirror_to_registry(name, value.as_ref());
        }
    }
}

pub fn notifications_enabled() -> bool {
    read_u32("NotificationsEnabled") != Some(0)
}

pub fn set_notifications_enabled(enabled: bool) {
    write_u32("NotificationsEnabled", Some(if enabled { 1 } else { 0 }));
}

/// Deletes the CPU power limits of earlier versions (the feature was removed).
pub fn clear_cpu_limits() {
    for mode in ["Efficiency", "Balanced", "Performance"] {
        write_u32(&format!("CpuLimit{mode}"), None);
    }
}

/// The scheme's own "maximum processor state" before GrandHonor capped it, per power source
/// ("Ac"/"Dc"), kept so it can be restored after a limit is removed, on quit or after a crash.
pub fn cpu_cap_original(source: &str) -> Option<u32> {
    read_u32(&format!("CpuCapOriginal{source}"))
}

pub fn set_cpu_cap_original(source: &str, value: Option<u32>) {
    write_u32(&format!("CpuCapOriginal{source}"), value);
}

/// Per power source presets ("Ac"/"Dc"): applied when the charger is plugged in or removed.
pub fn preset_enabled() -> bool {
    read_u32("PresetsEnabled") == Some(1)
}

pub fn set_preset_enabled(enabled: bool) {
    write_u32("PresetsEnabled", Some(u32::from(enabled)));
}

/// A preset value: `None` = leave unchanged.
pub fn preset_value(source: &str, name: &str) -> Option<u32> {
    read_u32(&format!("Preset{source}{name}"))
}

pub fn set_preset_value(source: &str, name: &str, value: Option<u32>) {
    write_u32(&format!("Preset{source}{name}"), value);
}

/// Interface language chosen in System. `None` follows the Windows language.
pub fn language_preference() -> Option<AppLanguage> {
    read_str("Language").and_then(|code| AppLanguage::from_code(&code))
}

pub fn set_language_preference(language: Option<AppLanguage>) {
    write_str("Language", language.map(AppLanguage::code));
    grandhonor_core::locale::set_language_override(language);
}

pub fn keyboard_backlight() -> Option<BacklightLevel> {
    read_str("KeyboardBacklightLevel").and_then(|s| BacklightLevel::parse(&s))
}

pub fn set_keyboard_backlight(level: Option<BacklightLevel>) {
    write_str("KeyboardBacklightLevel", level.map(BacklightLevel::as_str));
}

pub fn keyboard_backlight_timeout() -> u16 {
    read_u32("KeyboardBacklightTimeout")
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(DEFAULT_BACKLIGHT_TIMEOUT)
}

pub fn set_keyboard_backlight_timeout(seconds: u16) {
    write_u32("KeyboardBacklightTimeout", Some(seconds as u32));
}

pub fn performance_mode_active() -> bool {
    read_u32("PerformanceModeActive").is_some_and(|v| v != 0)
}

pub fn set_performance_mode_active(active: bool) {
    write_u32("PerformanceModeActive", Some(if active { 1 } else { 0 }));
}

pub fn hunter_mode_active() -> bool {
    read_u32("HunterModeActive").is_some_and(|v| v != 0)
}

pub fn set_hunter_mode_active(active: bool) {
    write_u32("HunterModeActive", Some(if active { 1 } else { 0 }));
}

pub fn battery_protection() -> Option<BatteryMode> {
    read_str("BatteryProtectionMode").and_then(|s| BatteryMode::parse(&s))
}

pub fn set_battery_protection(mode: Option<BatteryMode>) {
    write_str("BatteryProtectionMode", mode.map(BatteryMode::as_str));
}

pub fn preferred_battery_protection() -> Option<BatteryMode> {
    read_str("PreferredBatteryProtectionMode").and_then(|s| BatteryMode::parse(&s))
}

pub fn set_preferred_battery_protection(mode: Option<BatteryMode>) {
    write_str(
        "PreferredBatteryProtectionMode",
        mode.map(BatteryMode::as_str),
    );
}

pub fn seed_preferred_battery_protection() {
    if read_str("PreferredBatteryProtectionMode").is_none()
        && let Some(current) = read_str("BatteryProtectionMode")
    {
        write_str("PreferredBatteryProtectionMode", Some(&current));
    }
}

/// The user's choice for HONOR Performance (panel switch / Fn+P), kept across sleep, battery
/// periods and reboots; the firmware state itself is `performance_mode_active`.
pub fn power_unlock_preference() -> bool {
    read_u32("PowerUnlockEnabled") == Some(1)
}

pub fn set_power_unlock(enabled: Option<bool>) {
    write_u32("PowerUnlockEnabled", enabled.map(|v| if v { 1 } else { 0 }));
}

pub fn backlight_schedule_enabled() -> bool {
    read_u32("BacklightScheduleEnabled").is_some_and(|v| v != 0)
}

pub fn set_backlight_schedule_enabled(enabled: bool) {
    write_u32(
        "BacklightScheduleEnabled",
        Some(if enabled { 1 } else { 0 }),
    );
}

pub fn backlight_on_hour() -> u32 {
    read_u32("BacklightOnHour").unwrap_or(18).min(23)
}

pub fn set_backlight_on_hour(hour: u32) {
    write_u32("BacklightOnHour", Some(hour.min(23)));
}

pub fn backlight_off_hour() -> u32 {
    read_u32("BacklightOffHour").unwrap_or(6).min(23)
}

pub fn set_backlight_off_hour(hour: u32) {
    write_u32("BacklightOffHour", Some(hour.min(23)));
}

pub fn backlight_schedule_level() -> BacklightLevel {
    match read_str("BacklightScheduleLevel").and_then(|s| BacklightLevel::parse(&s)) {
        Some(BacklightLevel::High) => BacklightLevel::High,
        _ => BacklightLevel::Low,
    }
}

pub fn set_backlight_schedule_level(level: BacklightLevel) {
    let normalized = match level {
        BacklightLevel::Off => BacklightLevel::Low,
        other => other,
    };
    write_str("BacklightScheduleLevel", Some(normalized.as_str()));
}

pub fn touchpad_haptics() -> Option<TouchpadHaptics> {
    read_str("TouchpadHapticsLevel").and_then(|s| TouchpadHaptics::parse(&s))
}

pub fn set_touchpad_haptics(level: Option<TouchpadHaptics>) {
    write_str(
        "TouchpadHapticsLevel",
        level.map(TouchpadHaptics::as_str),
    );
}

pub fn touchpad_edge_gesture(gesture: TouchpadEdgeGesture) -> Option<bool> {
    read_u32(&format!("TouchpadEdgeGesture.{}", gesture.key_suffix())).map(|v| v != 0)
}

pub fn set_touchpad_edge_gesture(gesture: TouchpadEdgeGesture, enabled: Option<bool>) {
    write_u32(
        &format!("TouchpadEdgeGesture.{}", gesture.key_suffix()),
        enabled.map(|v| if v { 1 } else { 0 }),
    );
}

/// The latest sensor sample lives in memory only: sampling every six seconds must not
/// write to the registry (and so to disk).
static SENSOR_SNAPSHOT: Mutex<Option<String>> = Mutex::new(None);

pub fn set_sensor_snapshot(value: Option<&str>) {
    if let Ok(mut slot) = SENSOR_SNAPSHOT.lock() {
        *slot = value.map(str::to_string);
    }
}

fn sensor_snapshot() -> Option<String> {
    SENSOR_SNAPSHOT.lock().ok().and_then(|slot| slot.clone())
}

/// When a GrandHonor process last wrote the HONOR profile (low 32 bits of `GetTickCount64`,
/// a clock shared by all processes), so the tray host can tell its echo from `Fn+P`.
pub fn profile_write_tick() -> Option<u32> {
    read_u32("ProfileWriteTick")
}

pub fn set_profile_write_tick(tick: u32) {
    write_u32("ProfileWriteTick", Some(tick));
}

pub fn read_tooltip_state() -> TooltipState {
    TooltipState {
        sensor_snapshot: sensor_snapshot(),
        performance_mode_active: read_u32("PerformanceModeActive").is_some_and(|v| v != 0),
        hunter_mode_active: read_u32("HunterModeActive").is_some_and(|v| v != 0),
        keyboard_backlight: read_str("KeyboardBacklightLevel").and_then(|s| BacklightLevel::parse(&s)),
        battery_protection: read_str("BatteryProtectionMode").and_then(|s| BatteryMode::parse(&s)),
    }
}

#[cfg(test)]
mod store_tests {
    use super::{StoredValue, store};
    use std::collections::BTreeMap;

    #[test]
    fn settings_file_round_trips_and_rejects_foreign_json() {
        let mut values = BTreeMap::new();
        values.insert("Language".to_string(), StoredValue::Text("ru".into()));
        values.insert("PresetAcRefresh".to_string(), StoredValue::Dword(120));
        let text = store::render(&values);
        assert_eq!(store::parse(&text), Some(values));
        assert_eq!(store::parse(r#"{"settings":{}}"#), None);
        assert_eq!(store::parse("not json"), None);
        assert!(super::is_user_setting("TouchpadEdgeGesture.Brightness"));
        assert!(!super::is_user_setting("PerformanceModeActive"));
        assert!(!super::is_user_setting("../x"));
    }
}

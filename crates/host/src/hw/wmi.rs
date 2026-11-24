use super::com::{
    ComGuard, OwnedVariant, connect_wmi, get_prop, get_prop_string, get_prop_u64, put_prop,
    wmi_instances, wmi_query,
};
use grandhonor_core::locale::t;
use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::System::Wmi::{IWbemClassObject, IWbemServices};
use windows_core::{BSTR, PCWSTR};

const ROOT_WMI: &str = r"\\.\root\wmi";
const OEM_CLASS: &str = "OemWMIMethod";
const OEM_METHOD: &str = "OemWMIfun";
const PREFERRED_INSTANCE: &str = "HWMI_0";
const INPUT_LEN: usize = 64;
/// Battery firmware refreshes its rate about once a second; querying faster only repeats it.
const POWER_CACHE_TTL: Duration = Duration::from_millis(1_000);

#[derive(Debug)]
pub enum WmiError {
    Rejected { code: u8, message: String },
    Unavailable(String),
}

impl WmiError {
    pub fn is_rejected(&self) -> Option<u8> {
        match self {
            Self::Rejected { code, .. } => Some(*code),
            Self::Unavailable(_) => None,
        }
    }
}

impl fmt::Display for WmiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected { message, .. } | Self::Unavailable(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for WmiError {}

pub fn select_preferred_instance(instances: Vec<IWbemClassObject>) -> Option<IWbemClassObject> {
    let mut first = None;
    for obj in instances {
        if let Some(name) = get_prop_string(&obj, "InstanceName")
            && name
                .to_ascii_uppercase()
                .ends_with(&PREFERRED_INSTANCE.to_ascii_uppercase())
        {
            return Some(obj);
        }
        if first.is_none() {
            first = Some(obj);
        }
    }
    first
}

pub struct HonorWmiSession {
    services: Option<IWbemServices>,
    relpath: Option<String>,
    in_sig: Option<IWbemClassObject>,
}

// MTA WMI proxies are apartment-agile; every operation enters MTA via ComGuard::mta().
unsafe impl Send for HonorWmiSession {}

impl HonorWmiSession {
    pub fn new() -> Self {
        Self {
            services: None,
            relpath: None,
            in_sig: None,
        }
    }

    pub fn reset(&mut self) {
        self.in_sig = None;
        self.relpath = None;
        self.services = None;
    }

    fn ensure_connected(&mut self) -> Result<(), WmiError> {
        let _com = ComGuard::mta();
        if self.services.is_some() && self.relpath.is_some() && self.in_sig.is_some() {
            return Ok(());
        }
        let services = connect_wmi(ROOT_WMI).map_err(|e| {
            WmiError::Unavailable(format!(
                "{}{e}",
                t("Ошибка HONOR BIOS WMI: ", "HONOR BIOS WMI error: ", "HONOR BIOS WMI 错误：")
            ))
        })?;
        let instances = wmi_instances(&services, OEM_CLASS).map_err(|e| {
            WmiError::Unavailable(format!(
                "{}{e}",
                t("Ошибка HONOR BIOS WMI: ", "HONOR BIOS WMI error: ", "HONOR BIOS WMI 错误：")
            ))
        })?;
        let selected = select_preferred_instance(instances).ok_or_else(|| {
            WmiError::Unavailable(
                t(
                    "Интерфейс HONOR BIOS WMI не найден.",
                    "HONOR BIOS WMI interface was not found.",
                    "未找到 HONOR BIOS WMI 接口。",
                )
                .to_string(),
            )
        })?;
        let relpath = get_prop_string(&selected, "__RELPATH").ok_or_else(|| {
            WmiError::Unavailable("Missing __RELPATH on OemWMIMethod".to_string())
        })?;

        let class_bstr = BSTR::from(OEM_CLASS);
        let mut class_obj: Option<IWbemClassObject> = None;
        unsafe { services.GetObject(&class_bstr, Default::default(), None, Some(&mut class_obj), None) }
            .map_err(|e| WmiError::Unavailable(format!("OemWMIMethod GetObject failed: {e}")))?;
        let class_obj = class_obj.ok_or_else(|| {
            WmiError::Unavailable("OemWMIMethod GetObject returned null".to_string())
        })?;
        let method_w = windows_core::HSTRING::from(OEM_METHOD);
        let mut in_sig: Option<IWbemClassObject> = None;
        unsafe {
            class_obj.GetMethod(
                PCWSTR(method_w.as_ptr()),
                0,
                &mut in_sig,
                std::ptr::null_mut(),
            )
        }
        .map_err(|e| WmiError::Unavailable(format!("OemWMIfun GetMethod failed: {e}")))?;

        self.services = Some(services);
        self.relpath = Some(relpath);
        self.in_sig = Some(in_sig.ok_or_else(|| {
            WmiError::Unavailable("OemWMIfun returned no input signature".into())
        })?);
        Ok(())
    }

    pub fn call_bytes(&mut self, input_bytes: &[u8; INPUT_LEN]) -> Result<Vec<u8>, WmiError> {
        let _com = ComGuard::mta();
        let mut last_err: Option<WmiError> = None;
        for _attempt in 0..2 {
            if let Err(err) = self.ensure_connected() {
                last_err = Some(err);
                self.reset();
                continue;
            }
            let services = self.services.as_ref().unwrap();
            let relpath = self.relpath.as_ref().unwrap();
            let in_sig = self.in_sig.as_ref().unwrap();

            let spawn = unsafe { in_sig.SpawnInstance(0) };
            let in_params = match spawn {
                Ok(p) => p,
                Err(e) => {
                    last_err = Some(WmiError::Unavailable(format!("SpawnInstance failed: {e}")));
                    self.reset();
                    continue;
                }
            };
            let var_in = match OwnedVariant::from_u8_slice(input_bytes) {
                Ok(v) => v,
                Err(e) => return Err(WmiError::Unavailable(format!("SafeArray alloc failed: {e}"))),
            };
            if let Err(e) = put_prop(&in_params, "u8Input", &var_in) {
                last_err = Some(WmiError::Unavailable(format!("Put u8Input failed: {e}")));
                self.reset();
                continue;
            }

            let path_bstr = BSTR::from(relpath.as_str());
            let method_bstr = BSTR::from(OEM_METHOD);
            let mut out_params: Option<IWbemClassObject> = None;
            let exec = unsafe {
                services.ExecMethod(
                    &path_bstr,
                    &method_bstr,
                    Default::default(),
                    None,
                    &in_params,
                    Some(&mut out_params),
                    None,
                )
            };
            if let Err(e) = exec {
                let code = e.code().0 as u32;
                if code == 0x80070005 {
                    return Err(WmiError::Unavailable(
                        t(
                            "Для обращения к HONOR BIOS нужны права администратора.",
                            "Administrator privileges are required to access HONOR BIOS.",
                            "访问 HONOR BIOS 需要管理员权限。",
                        )
                        .to_string(),
                    ));
                }
                last_err = Some(WmiError::Unavailable(format!(
                    "{}{e}",
                    t("Ошибка HONOR BIOS WMI: ", "HONOR BIOS WMI error: ", "HONOR BIOS WMI 错误：")
                )));
                self.reset();
                continue;
            }

            let out_obj = out_params.ok_or_else(|| {
                WmiError::Unavailable(
                    t(
                        "BIOS не вернул ответ.",
                        "BIOS did not return a response.",
                        "BIOS 未返回响应。",
                    )
                    .to_string(),
                )
            })?;
            let out_var = get_prop(&out_obj, "u8Output").map_err(|e| {
                WmiError::Unavailable(format!("Could not read u8Output: {e}"))
            })?;
            let bytes = out_var.as_bytes().ok_or_else(|| {
                WmiError::Unavailable(
                    t(
                        "BIOS не вернул ответ.",
                        "BIOS did not return a response.",
                        "BIOS 未返回响应。",
                    )
                    .to_string(),
                )
            })?;

            if bytes.first() == Some(&0) {
                return Ok(bytes);
            }

            let err_code = bytes.first().copied().unwrap_or(255);
            let msg = match grandhonor_core::locale::language() {
                grandhonor_core::locale::AppLanguage::Russian => {
                    format!("BIOS отклонил команду (код {err_code}).")
                }
                grandhonor_core::locale::AppLanguage::Chinese => {
                    format!("BIOS 拒绝了该命令（代码 {err_code}）。")
                }
                grandhonor_core::locale::AppLanguage::English => {
                    format!("BIOS rejected the command (code {err_code}).")
                }
            };
            return Err(WmiError::Rejected {
                code: err_code,
                message: msg,
            });
        }

        let err = last_err.unwrap_or_else(|| {
            WmiError::Unavailable(
                t(
                    "Команда BIOS не выполнена.",
                    "BIOS command was not executed.",
                    "BIOS 命令未执行。",
                )
                .to_string(),
            )
        });
        Err(err)
    }

    pub fn call(&mut self, command: u64) -> Result<Vec<u8>, WmiError> {
        let mut input = [0u8; INPUT_LEN];
        input[..8].copy_from_slice(&command.to_le_bytes());
        self.call_bytes(&input)
    }
}

pub fn honor_wmi_call(command: u64) -> Result<Vec<u8>, WmiError> {
    let _com = ComGuard::mta();
    let mut session = HonorWmiSession::new();
    match session.call(command) {
        Ok(out) => Ok(out),
        Err(err) => {
            crate::log::error_with(&format!("HONOR WMI command 0x{command:X} failed"), &err);
            Err(err)
        }
    }
}

struct AcpiDirectState {
    session: Option<HonorWmiSession>,
    access_denied: bool,
}

static ACPI_DIRECT_GATE: OnceLock<Mutex<AcpiDirectState>> = OnceLock::new();

fn acpi_gate() -> &'static Mutex<AcpiDirectState> {
    ACPI_DIRECT_GATE.get_or_init(|| {
        Mutex::new(AcpiDirectState {
            session: None,
            access_denied: false,
        })
    })
}

pub fn acpi_try_call(group: u8, command: u8, value: u8) -> bool {
    let _com = ComGuard::mta();
    let Ok(mut state) = acpi_gate().lock() else {
        return false;
    };
    if state.access_denied {
        return false;
    }
    let session = state.session.get_or_insert_with(HonorWmiSession::new);
    let mut input = [0u8; INPUT_LEN];
    input[0] = group;
    input[1] = command;
    input[2] = value;

    match session.call_bytes(&input) {
        Ok(_) => true,
        Err(err) => {
            state.access_denied = true;
            state.session = None;
            crate::log::error_with(
                &format!("Honor ACPI direct command {group:02X} {command:02X} unavailable"),
                &err,
            );
            false
        }
    }
}

pub fn acpi_reconnect() {
    let _com = ComGuard::mta();
    if let Ok(mut state) = acpi_gate().lock() {
        state.access_denied = false;
        state.session = None;
    }
}

pub fn try_step_acpi_brightness(up: bool) -> bool {
    // Honor ACPI-WMI display brightness step: 06 35 <0=up | 1=down>
    acpi_try_call(0x06, 0x35, if up { 0x00 } else { 0x01 })
}

pub fn set_mic_mute_led(on: bool) -> bool {
    // Honor ACPI-WMI mic mute LED: 04 0B <0=off | 1=on>
    acpi_try_call(0x04, 0x0B, if on { 1 } else { 0 })
}

struct PowerCache {
    watts: Option<f64>,
    sampled_at: Instant,
}

static POWER_SAMPLE_CACHE: OnceLock<Mutex<Option<PowerCache>>> = OnceLock::new();

pub fn read_battery_power_watts() -> Option<f64> {
    let cache = POWER_SAMPLE_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some(cached) = guard.as_ref()
        && cached.sampled_at.elapsed() < POWER_CACHE_TTL
    {
        return cached.watts;
    }

    let _com = ComGuard::mta();
    let result = (|| -> Option<f64> {
        let services = connect_wmi(ROOT_WMI).ok()?;
        let items = wmi_query(&services, "SELECT ChargeRate, DischargeRate FROM BatteryStatus WHERE Active = TRUE").ok()?;
        let first = items.into_iter().next()?;
        let rate = |property| {
            get_prop_u64(&first, property)
                .filter(|&value| value < u32::MAX as u64)
                .map(|value| value as i64)
        };
        let charge = rate("ChargeRate")?;
        let discharge = rate("DischargeRate")?;
        Some((charge - discharge) as f64 / 1000.0)
    })();

    if let Ok(mut guard) = cache.lock() {
        *guard = Some(PowerCache {
            watts: result,
            sampled_at: Instant::now(),
        });
    }
    result
}

static BRIGHTNESS_GATE: OnceLock<Mutex<()>> = OnceLock::new();

pub fn change_wmi_brightness(delta: i32) -> i32 {
    let gate = BRIGHTNESS_GATE.get_or_init(|| Mutex::new(()));
    let Ok(_g) = gate.lock() else { return -1 };
    let _com = ComGuard::mta();
    match change_wmi_brightness_core(|current| (current + delta).clamp(0, 100) as u32) {
        Ok(target) => target,
        Err(err) => {
            crate::log::error(&format!("WmiSetBrightness failed: {err}"));
            -1
        }
    }
}

/// Sets the built-in display brightness to `percent` (0–100). Returns false if unsupported.
pub fn set_wmi_brightness(percent: u32) -> bool {
    let gate = BRIGHTNESS_GATE.get_or_init(|| Mutex::new(()));
    let Ok(_g) = gate.lock() else { return false };
    let _com = ComGuard::mta();
    match change_wmi_brightness_core(|_| percent.min(100)) {
        Ok(target) => target >= 0,
        Err(err) => {
            crate::log::error(&format!("WmiSetBrightness failed: {err}"));
            false
        }
    }
}

fn change_wmi_brightness_core(target_for: impl Fn(i32) -> u32) -> Result<i32, String> {
    let services = connect_wmi(ROOT_WMI).map_err(|e| e.to_string())?;
    let brightness_objs = wmi_query(
        &services,
        "SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active=True",
    )
    .map_err(|e| e.to_string())?;
    let Some(brightness_obj) = brightness_objs.into_iter().next() else {
        return Ok(-1);
    };
    let current = get_prop_u64(&brightness_obj, "CurrentBrightness").unwrap_or(50) as i32;
    let target = target_for(current);

    let method_objs = wmi_query(
        &services,
        "SELECT __RELPATH FROM WmiMonitorBrightnessMethods WHERE Active=True",
    )
    .map_err(|e| e.to_string())?;
    let class_bstr = BSTR::from("WmiMonitorBrightnessMethods");
    let mut class_obj: Option<IWbemClassObject> = None;
    unsafe { services.GetObject(&class_bstr, Default::default(), None, Some(&mut class_obj), None) }
        .map_err(|e| e.to_string())?;
    let Some(class_obj) = class_obj else {
        return Ok(-1);
    };
    let method_w = windows_core::HSTRING::from("WmiSetBrightness");
    let mut in_sig: Option<IWbemClassObject> = None;
    unsafe {
        class_obj.GetMethod(
            PCWSTR(method_w.as_ptr()),
            0,
            &mut in_sig,
            std::ptr::null_mut(),
        )
    }
    .map_err(|e| e.to_string())?;
    let Some(in_sig) = in_sig else {
        return Ok(-1);
    };

    for obj in method_objs {
        let Some(relpath) = get_prop_string(&obj, "__RELPATH") else {
            continue;
        };
        let in_params = unsafe { in_sig.SpawnInstance(0) }.map_err(|e| e.to_string())?;
        let _ = put_prop(&in_params, "Timeout", &OwnedVariant::from_u32(0));
        let _ = put_prop(&in_params, "Brightness", &OwnedVariant::from_u32(target));
        let path_bstr = BSTR::from(relpath.as_str());
        let method_bstr = BSTR::from("WmiSetBrightness");
        unsafe {
            services.ExecMethod(
                &path_bstr,
                &method_bstr,
                Default::default(),
                None,
                &in_params,
                None,
                None,
            )
        }
        .map_err(|e| e.to_string())?;
    }
    Ok(target as i32)
}

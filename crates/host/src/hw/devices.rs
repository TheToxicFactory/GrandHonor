//! Drivers → Device check, GrandHonor's version of PC Manager's hardware check and repair:
//! devices Windows reports with a problem (Configuration Manager problem codes), restarting a
//! device (what Device Manager's disable/enable does, `DIF_PROPERTYCHANGE`), restarting the
//! Bluetooth or Wi-Fi adapter, and scanning for new hardware. Runs only when the panel asks.

use std::mem::size_of;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_LOCATE_DEVNODE_NORMAL, CM_PROB, CM_REENUMERATE_SYNCHRONOUS, CR_SUCCESS,
    CM_Get_DevNode_Status, CM_Locate_DevNodeW, CM_Reenumerate_DevNode, DI_NEEDREBOOT, DI_NEEDRESTART,
    DIROLLBACKDRIVER_FLAGS, DiRollbackDriver,
    DICS_FLAG_GLOBAL, DICS_PROPCHANGE, DIF_PROPERTYCHANGE, DIGCF_ALLCLASSES, DIGCF_PRESENT, DN_HAS_PROBLEM,
    HDEVINFO, SETUP_DI_REGISTRY_PROPERTY, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W,
    SP_PROPCHANGE_PARAMS, SPDRP_DEVICEDESC, SPDRP_DRIVER, SPDRP_FRIENDLYNAME, SetupDiCallClassInstaller,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstallParamsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW, SetupDiSetClassInstallParamsW,
};
use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
use windows_core::{GUID, PCWSTR};

pub(super) const GUID_DEVCLASS_BLUETOOTH: GUID = GUID::from_u128(0xe0cbf06c_cd8b_4647_bb8a_263b43f0f974);
const GUID_DEVCLASS_NET: GUID = GUID::from_u128(0x4d36e972_e325_11ce_bfc1_08002be10318);
/// Disabled on purpose by the user: not a fault.
const CM_PROB_DISABLED: u32 = 22;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub instance_id: String,
    pub name: String,
    pub problem: u32,
}

pub(super) struct PresentDevice {
    pub class: GUID,
    pub device: Device,
    pub has_driver: bool,
    pub status_known: bool,
}

impl PresentDevice {
    pub fn has_problem(&self) -> bool {
        self.status_known && self.device.problem != 0 && self.device.problem != CM_PROB_DISABLED
    }
}

/// What a problem code means, in a few words (the common Device Manager codes).
pub fn problem_text(code: u32) -> &'static str {
    use grandhonor_core::locale::t;
    match code {
        1 | 18 | 19 => t("неверная настройка, нужна переустановка драйвера", "configuration problem, reinstall the driver", "配置有误，请重新安装驱动"),
        10 => t("устройство не запускается", "the device cannot start", "设备无法启动"),
        12 => t("не хватает ресурсов", "not enough free resources", "资源不足"),
        14 | 21 | 47 => t("нужна перезагрузка", "a restart is needed", "需要重启"),
        24 => t("устройство не найдено или не работает", "missing or not working", "设备不存在或未工作"),
        28 => t("драйвер не установлен", "no driver installed", "未安装驱动程序"),
        31 => t("драйвер не загрузился", "the driver did not load", "驱动程序未加载"),
        32 | 39 | 41 => t("драйвер повреждён или отсутствует", "the driver is damaged or missing", "驱动程序损坏或缺失"),
        37 | 38 => t("драйвер завершился с ошибкой", "the driver failed", "驱动程序出错"),
        43 => t("устройство сообщило о сбое", "the device reported a failure", "设备报告故障"),
        48 | 52 => t("драйвер заблокирован или не подписан", "the driver is blocked or unsigned", "驱动程序被阻止或未签名"),
        _ => t("ошибка устройства", "device error", "设备错误"),
    }
}

fn property(set: HDEVINFO, data: &SP_DEVINFO_DATA, prop: SETUP_DI_REGISTRY_PROPERTY) -> Option<String> {
    let mut buf = [0u16; 512];
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 2)
    };
    unsafe { SetupDiGetDeviceRegistryPropertyW(set, data, prop, None, Some(bytes), None) }.ok()?;
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn instance_id(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Option<String> {
    let mut buf = [0u16; 512];
    unsafe { SetupDiGetDeviceInstanceIdW(set, data, Some(&mut buf), None) }.ok()?;
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Calls `visit` for every present device of `class` (all classes when None) until it returns true.
fn for_each(class: Option<&GUID>, mut visit: impl FnMut(HDEVINFO, &SP_DEVINFO_DATA) -> bool) -> windows_core::Result<()> {
    let flags = if class.is_some() { DIGCF_PRESENT } else { DIGCF_PRESENT | DIGCF_ALLCLASSES };
    let set = unsafe { SetupDiGetClassDevsW(class.map(|g| g as *const GUID), PCWSTR::null(), None, flags) }?;
    let mut data = SP_DEVINFO_DATA { cbSize: size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
    let mut index = 0;
    let result = loop {
        match unsafe { SetupDiEnumDeviceInfo(set, index, &mut data) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_ITEMS.to_hresult() => break Ok(()),
            Err(error) => break Err(error),
        }
        index += 1;
        if visit(set, &data) {
            break Ok(());
        }
    };
    let _ = unsafe { SetupDiDestroyDeviceInfoList(set) };
    result
}

pub(super) fn inventory(cancelled: impl Fn() -> bool) -> windows_core::Result<Vec<PresentDevice>> {
    let mut out = Vec::new();
    for_each(None, |set, data| {
        if cancelled() {
            return true;
        }
        let mut status = CM_DEVNODE_STATUS_FLAGS::default();
        let mut problem = CM_PROB::default();
        let status_known = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, data.DevInst, 0) } == CR_SUCCESS;
        let name = property(set, data, SPDRP_FRIENDLYNAME)
            .or_else(|| property(set, data, SPDRP_DEVICEDESC))
            .unwrap_or_else(|| grandhonor_core::locale::t("Неизвестное устройство", "Unknown device", "未知设备").to_string());
        out.push(PresentDevice {
            class: data.ClassGuid,
            device: Device {
                instance_id: instance_id(set, data).unwrap_or_default(),
                name,
                problem: if status.0 & DN_HAS_PROBLEM.0 != 0 { problem.0 } else { 0 },
            },
            has_driver: property(set, data, SPDRP_DRIVER).is_some(),
            status_known,
        });
        false
    })?;
    out.sort_by(|a, b| a.device.name.cmp(&b.device.name));
    Ok(out)
}

pub fn problems() -> windows_core::Result<(Vec<Device>, bool)> {
    let devices = inventory(|| false)?;
    let complete = !devices.is_empty() && devices.iter().all(|device| device.status_known);
    let problems = devices.into_iter().filter(PresentDevice::has_problem).map(|device| device.device).collect();
    Ok((problems, complete))
}

/// Restarts one device. Ok(true) when Windows says a reboot is needed to finish.
fn restart(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Result<bool, String> {
    let params = SP_PROPCHANGE_PARAMS {
        ClassInstallHeader: SP_CLASSINSTALL_HEADER {
            cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
            InstallFunction: DIF_PROPERTYCHANGE,
        },
        StateChange: DICS_PROPCHANGE,
        Scope: DICS_FLAG_GLOBAL,
        HwProfile: 0,
    };
    unsafe {
        SetupDiSetClassInstallParamsW(
            set,
            Some(data),
            Some(&params.ClassInstallHeader),
            size_of::<SP_PROPCHANGE_PARAMS>() as u32,
        )
        .map_err(|e| e.to_string())?;
        SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set, Some(data)).map_err(|e| e.to_string())?;
        let mut install = SP_DEVINSTALL_PARAMS_W { cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32, ..Default::default() };
        let reboot = SetupDiGetDeviceInstallParamsW(set, Some(data), &mut install).is_ok()
            && install.Flags.0 & (DI_NEEDREBOOT.0 | DI_NEEDRESTART.0) != 0;
        Ok(reboot)
    }
}

/// Restarts the device with this instance id.
pub fn restart_device(instance: &str) -> Result<bool, String> {
    let mut result = Err(grandhonor_core::locale::t("Устройство не найдено", "Device not found", "未找到设备").to_string());
    for_each(None, |set, data| {
        if instance_id(set, data).is_some_and(|id| id.eq_ignore_ascii_case(instance)) {
            result = restart(set, data);
            return true;
        }
        false
    }).map_err(|error| error.to_string())?;
    result
}

/// Device Manager's "Roll back driver": the previous driver Windows kept for this device.
/// Ok(true) when a restart is needed; an error when there is no previous driver.
pub fn rollback_driver(instance: &str) -> Result<bool, String> {
    let mut result = Err(grandhonor_core::locale::t("Устройство не найдено", "Device not found", "未找到设备").to_string());
    for_each(None, |set, data| {
        if instance_id(set, data).is_some_and(|id| id.eq_ignore_ascii_case(instance)) {
            let mut reboot = windows_core::BOOL(0);
            result = unsafe { DiRollbackDriver(set, data, None, DIROLLBACKDRIVER_FLAGS(0), Some(&mut reboot)) }
                .map(|()| reboot.as_bool())
                .map_err(|e| {
                    crate::log::error(&format!("Roll back driver {instance}: {e}"));
                    grandhonor_core::locale::t(
                        "Прежнего драйвера нет или Windows не смогла его вернуть.",
                        "There is no previous driver, or Windows could not restore it.",
                        "没有旧版驱动，或 Windows 无法恢复。",
                    )
                    .to_string()
                });
            return true;
        }
        false
    }).map_err(|error| error.to_string())?;
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Radio {
    Bluetooth,
    WiFi,
}

/// Restarts the Bluetooth or Wi-Fi adapter itself (the PCI/USB device, not the virtual ones).
pub fn restart_radio(radio: Radio) -> Result<bool, String> {
    let class = match radio {
        Radio::Bluetooth => GUID_DEVCLASS_BLUETOOTH,
        Radio::WiFi => GUID_DEVCLASS_NET,
    };
    let mut result = Err(grandhonor_core::locale::t("Адаптер не найден", "Adapter not found", "未找到适配器").to_string());
    for_each(Some(&class), |set, data| {
        let Some(id) = instance_id(set, data) else { return false };
        let bus = id.starts_with("PCI\\") || id.starts_with("USB\\");
        let name = property(set, data, SPDRP_FRIENDLYNAME).or_else(|| property(set, data, SPDRP_DEVICEDESC)).unwrap_or_default();
        let wifi = name.contains("Wi-Fi") || name.contains("Wireless") || name.contains("WLAN") || name.contains("802.11");
        if bus && (radio == Radio::Bluetooth || wifi) {
            result = restart(set, data);
            return true;
        }
        false
    }).map_err(|error| error.to_string())?;
    result
}

/// Asks Windows to look for new or changed hardware (Device Manager → Scan for hardware changes).
pub fn rescan() -> bool {
    let mut root = 0u32;
    unsafe {
        CM_Locate_DevNodeW(&mut root, PCWSTR::null(), CM_LOCATE_DEVNODE_NORMAL) == CR_SUCCESS
            && CM_Reenumerate_DevNode(root, CM_REENUMERATE_SYNCHRONOUS) == CR_SUCCESS
    }
}

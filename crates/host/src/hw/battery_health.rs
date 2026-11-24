//! Battery health from the battery driver itself (`IOCTL_BATTERY_QUERY_INFORMATION`, the source
//! of `powercfg /batteryreport`): design capacity, current full-charge capacity and cycle count.
//! Read on demand when the panel is built; nothing is cached or polled.

use std::mem::size_of;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows_core::{GUID, PCWSTR};

const GUID_DEVICE_BATTERY: GUID = GUID::from_u128(0x72631e54_78a4_11d0_bcf7_00aa00b7b32a);
const IOCTL_BATTERY_QUERY_TAG: u32 = 0x0029_4040;
const IOCTL_BATTERY_QUERY_INFORMATION: u32 = 0x0029_4044;
const BATTERY_INFORMATION_LEVEL: i32 = 0;

#[repr(C)]
#[derive(Default)]
struct BatteryQueryInformation {
    tag: u32,
    level: i32,
    at_rate: i32,
}

#[repr(C)]
#[derive(Default)]
struct BatteryInformation {
    capabilities: u32,
    technology: u8,
    reserved: [u8; 3],
    chemistry: [u8; 4],
    designed_capacity: u32,
    full_charged_capacity: u32,
    default_alert1: u32,
    default_alert2: u32,
    critical_bias: u32,
    cycle_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryHealth {
    /// mWh, as designed.
    pub design_mwh: u32,
    /// mWh the battery holds now when full.
    pub full_mwh: u32,
    pub cycles: Option<u32>,
}

impl BatteryHealth {
    /// Full-charge capacity as a share of the design capacity, 0–100 (a new battery can
    /// slightly exceed its design rating; that is shown as 100).
    pub fn percent(&self) -> u32 {
        if self.design_mwh == 0 {
            return 0;
        }
        ((u64::from(self.full_mwh) * 100 + u64::from(self.design_mwh) / 2) / u64::from(self.design_mwh)).min(100) as u32
    }
}

/// Reads the first system battery. None when there is none or the driver does not answer.
pub fn read() -> Option<BatteryHealth> {
    unsafe {
        let set = SetupDiGetClassDevsW(Some(&GUID_DEVICE_BATTERY), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE).ok()?;
        let mut result = None;
        let mut iface = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        for index in 0..4 {
            if SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVICE_BATTERY, index, &mut iface).is_err() {
                break;
            }
            let mut needed = 0u32;
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &iface, None, 0, Some(&mut needed), None);
            if needed == 0 {
                continue;
            }
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
            let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &iface, Some(detail), needed, None, None).is_err() {
                continue;
            }
            let path = PCWSTR((*detail).DevicePath.as_ptr());
            let Ok(handle) = CreateFileW(
                path,
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) else {
                continue;
            };
            result = query(handle);
            let _ = CloseHandle(handle);
            if result.is_some() {
                break;
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        result
    }
}

unsafe fn query(handle: windows::Win32::Foundation::HANDLE) -> Option<BatteryHealth> {
    unsafe {
        let wait: u32 = 0;
        let mut tag = 0u32;
        let mut returned = 0u32;
        DeviceIoControl(
            handle,
            IOCTL_BATTERY_QUERY_TAG,
            Some(&wait as *const u32 as *const _),
            4,
            Some(&mut tag as *mut u32 as *mut _),
            4,
            Some(&mut returned),
            None,
        )
        .ok()?;
        if tag == 0 {
            return None;
        }
        let q = BatteryQueryInformation { tag, level: BATTERY_INFORMATION_LEVEL, at_rate: 0 };
        let mut info = BatteryInformation::default();
        DeviceIoControl(
            handle,
            IOCTL_BATTERY_QUERY_INFORMATION,
            Some(&q as *const _ as *const _),
            size_of::<BatteryQueryInformation>() as u32,
            Some(&mut info as *mut _ as *mut _),
            size_of::<BatteryInformation>() as u32,
            Some(&mut returned),
            None,
        )
        .ok()?;
        // Capacities in mWh only when the battery reports absolute values (BATTERY_CAPACITY_RELATIVE unset).
        const BATTERY_CAPACITY_RELATIVE: u32 = 0x4000_0000;
        if info.capabilities & BATTERY_CAPACITY_RELATIVE != 0 || info.designed_capacity == 0 || info.full_charged_capacity == 0 {
            return None;
        }
        Some(BatteryHealth {
            design_mwh: info.designed_capacity,
            full_mwh: info.full_charged_capacity,
            cycles: (info.cycle_count > 0).then_some(info.cycle_count),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_rounded_and_capped() {
        let h = BatteryHealth { design_mwh: 95_600, full_mwh: 92_041, cycles: Some(2) };
        assert_eq!(h.percent(), 96);
        let new = BatteryHealth { design_mwh: 90_000, full_mwh: 91_000, cycles: None };
        assert_eq!(new.percent(), 100);
    }
}

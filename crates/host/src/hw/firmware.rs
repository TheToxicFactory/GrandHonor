//! The laptop's identity from the firmware's own SMBIOS table (`GetSystemFirmwareTable`,
//! parsed by `grandhonor_core::smbios`). Unlike `HKLM\SOFTWARE\HONOR\BIOS`, which HONOR's
//! software writes and which goes stale (or disappears) once that software is removed, this
//! is always current. Read once per process; SMBIOS changes only with a BIOS update and reboot.

use grandhonor_core::smbios::{Firmware, parse};
use std::sync::OnceLock;
use windows::Win32::System::SystemInformation::{
    FIRMWARE_TABLE_PROVIDER, GetSystemFirmwareTable,
};

pub fn identity() -> &'static Firmware {
    static CACHE: OnceLock<Firmware> = OnceLock::new();
    CACHE.get_or_init(|| {
        let rsmb = FIRMWARE_TABLE_PROVIDER(u32::from_be_bytes(*b"RSMB"));
        let size = unsafe { GetSystemFirmwareTable(rsmb, 0, None) };
        if size == 0 {
            return Firmware::default();
        }
        let mut buffer = vec![0u8; size as usize];
        let written = unsafe { GetSystemFirmwareTable(rsmb, 0, Some(&mut buffer)) };
        buffer.truncate(written as usize);
        parse(&buffer)
    })
}

/// BIOS version: firmware first, then Windows' copy, then HONOR's (possibly stale) one.
pub fn bios_version() -> Option<String> {
    let clean = |s: String| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    identity()
        .bios_version
        .clone()
        .or_else(|| crate::settings::read_hklm_string(r"HARDWARE\DESCRIPTION\System\BIOS", "BIOSVersion").and_then(clean))
        .or_else(|| crate::settings::read_hklm_string(r"SOFTWARE\HONOR\BIOS", "BIOSVersion").and_then(clean))
}

//! Clean-up for the CPU power limit of GrandHonor 2.2–2.5. That limit lowered Windows' "maximum
//! processor state" to keep package power under a number of watts; it did not hold the limit
//! reliably and was removed. At start and on uninstall, a value it left lowered is put back
//! (`restore_original`) and its settings are deleted. Nothing runs afterwards.

use crate::settings;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::System::Power::{
    PowerGetActiveScheme, PowerSetActiveScheme, PowerWriteACValueIndex, PowerWriteDCValueIndex,
};
use windows_core::GUID;

const SUB_PROCESSOR: GUID = GUID::from_u128(0x54533251_82be_4824_96c1_47b60b740d00);
const PROC_MAX: GUID = GUID::from_u128(0xbc5038f7_23e0_4960_96da_33abaf5935ec);
const PROC_MAX_CLASS1: GUID = GUID::from_u128(0xbc5038f7_23e0_4960_96da_33abaf5935ed);

fn source_key(ac: bool) -> &'static str {
    if ac { "Ac" } else { "Dc" }
}

fn write_cap(ac: bool, percent: u32) -> bool {
    unsafe {
        let mut scheme: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(None, &mut scheme).is_err() || scheme.is_null() {
            return false;
        }
        let mut ok = true;
        for setting in [PROC_MAX, PROC_MAX_CLASS1] {
            let written = if ac {
                PowerWriteACValueIndex(None, scheme, Some(&SUB_PROCESSOR), Some(&setting), percent).is_ok()
            } else {
                PowerWriteDCValueIndex(None, scheme, Some(&SUB_PROCESSOR), Some(&setting), percent) == 0
            };
            ok &= written || setting == PROC_MAX_CLASS1; // class 1 is absent on non-hybrid CPUs
        }
        let _ = PowerSetActiveScheme(None, Some(scheme));
        let _ = LocalFree(Some(HLOCAL(scheme as *mut _)));
        ok
    }
}

/// Puts back the scheme's own maximum processor state for both power sources and forgets
/// the old limit settings.
pub fn restore_original() {
    for ac in [true, false] {
        if let Some(original) = settings::cpu_cap_original(source_key(ac))
            && write_cap(ac, original)
        {
            settings::set_cpu_cap_original(source_key(ac), None);
        }
    }
    settings::clear_cpu_limits();
}

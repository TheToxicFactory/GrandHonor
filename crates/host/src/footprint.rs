//! Keeps the always-running host as small as possible.
//!
//! - `enter_efficiency_mode` asks Windows for EcoQoS: the scheduler prefers efficient cores and
//!   low clocks for this process. Task Manager shows it as "Efficiency mode". Event handling
//!   (Fn keys, touchpad, power events) still runs immediately; it is only scheduled economically.
//! - `trim_working_set` returns pages that are not in use to Windows. It is called once the host
//!   has started and every time the panel closes, so idle RAM stays at the minimum.

use std::ffi::c_void;

const PROCESS_POWER_THROTTLING: i32 = 4; // ProcessPowerThrottling
const PROCESS_POWER_THROTTLING_CURRENT_VERSION: u32 = 1;
const PROCESS_POWER_THROTTLING_EXECUTION_SPEED: u32 = 0x1;

#[repr(C)]
struct PowerThrottlingState {
    version: u32,
    control_mask: u32,
    state_mask: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn SetProcessInformation(
        process: *mut c_void,
        class: i32,
        info: *const c_void,
        size: u32,
    ) -> i32;
    fn K32EmptyWorkingSet(process: *mut c_void) -> i32;
}

pub fn enter_efficiency_mode() {
    let state = PowerThrottlingState {
        version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        control_mask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        state_mask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    unsafe {
        let _ = SetProcessInformation(
            GetCurrentProcess(),
            PROCESS_POWER_THROTTLING,
            &state as *const _ as *const c_void,
            size_of::<PowerThrottlingState>() as u32,
        );
    }
}

pub fn trim_working_set() {
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}

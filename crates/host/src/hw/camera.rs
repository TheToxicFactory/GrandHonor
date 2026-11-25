use crate::settings;
use std::sync::{Mutex, OnceLock};

const SETTINGS_PATH: &str = r"SOFTWARE\HONOR\ASvidDMFT\Settings";
const CAMERA_STATUS: &str = "CameraStatus";

static GATE: OnceLock<Mutex<()>> = OnceLock::new();

/// The camera privacy switch is implemented by the HONOR build of Realtek's camera driver
/// (`RsDMFTNB.dll`, a device MFT that blanks the picture), which reads `CameraStatus` from
/// `HKLM\SOFTWARE\HONOR\ASvidDMFT\Settings`: 1 = on, 0 = off. The value is normally created by
/// HONOR's software; when that is gone the driver still honours it, so GrandHonor treats a
/// missing value as "on" and creates it when you switch.
fn privacy_driver_present() -> bool {
    crate::known_dirs::system32().join("RsDMFTNB.dll").is_file()
}

pub fn get_camera_off() -> Option<bool> {
    match settings::read_hklm_dword(SETTINGS_PATH, CAMERA_STATUS) {
        Some(0) => Some(true),
        Some(1) => Some(false),
        Some(_) => None,
        None => privacy_driver_present().then_some(false),
    }
}

pub fn set_camera_off(off: bool) -> Result<(), String> {
    if get_camera_off().is_none() {
        return Err("The HONOR camera switch is unavailable.".to_string());
    }
    settings::create_hklm_dword(SETTINGS_PATH, CAMERA_STATUS, if off { 0 } else { 1 })
        .map_err(|e| format!("Could not write camera status ({:?})", e))
}

pub fn toggle_camera() -> Option<bool> {
    let gate = GATE.get_or_init(|| Mutex::new(()));
    let Ok(_g) = gate.lock() else {
        return None;
    };
    let current_off = get_camera_off()?;
    let next = !current_off;
    match set_camera_off(next) {
        Ok(()) => Some(next),
        Err(err) => {
            crate::log::error(&format!("Could not switch the OEM camera setting: {err}"));
            None
        }
    }
}

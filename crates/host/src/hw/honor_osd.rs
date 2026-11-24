//! HONOR's own on-screen display (service `OSDServer`, `C:\Windows\System32\RPC\OSD`, which
//! starts `OSD_Daemon.exe`). GrandHonor shows all of its notices itself, so while HONOR OSD is
//! installed the host keeps that service disabled at every start (otherwise each Fn key shows
//! two HUDs); uninstalling GrandHonor sets it back to automatic and starts it. Nothing of
//! HONOR's is deleted.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const SERVICE: &str = "OSDServer";
const SERVICE_KEY: &str = r"SYSTEM\CurrentControlSet\Services\OSDServer";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const START_DISABLED: u32 = 4;

/// True when HONOR OSD is installed on this laptop.
pub fn installed() -> bool {
    crate::settings::read_hklm_dword(SERVICE_KEY, "Start").is_some()
}

/// True when GrandHonor has replaced HONOR OSD (its service is disabled).
pub fn replaced() -> bool {
    crate::settings::read_hklm_dword(SERVICE_KEY, "Start") == Some(START_DISABLED)
}

fn system32(exe: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join(exe)
}

fn run(exe: &str, args: &[&str]) -> bool {
    Command::new(system32(exe))
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Replaces (true) or restores (false) HONOR OSD. Returns an error text for the panel.
pub fn set_replaced(replace: bool) -> Result<(), String> {
    if !installed() {
        return Err("HONOR OSD is not installed.".into());
    }
    if replace {
        let configured = run("sc.exe", &["config", SERVICE, "start=", "disabled"]);
        let _ = run("sc.exe", &["stop", SERVICE]);
        let _ = run("taskkill.exe", &["/F", "/IM", "OSD_Daemon.exe"]);
        if !configured {
            return Err("Could not disable the HONOR OSD service.".into());
        }
    } else {
        if !run("sc.exe", &["config", SERVICE, "start=", "auto"]) {
            return Err("Could not re-enable the HONOR OSD service.".into());
        }
        let _ = run("sc.exe", &["start", SERVICE]);
    }
    crate::log::info(&format!("HONOR OSD {}", if replace { "replaced by GrandHonor" } else { "restored" }));
    Ok(())
}

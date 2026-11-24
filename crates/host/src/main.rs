#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cache;
mod drivers;
mod footprint;
mod hotkeys;
mod hw;
mod log;
mod menu_builder;
mod osd;
mod panel_host;
mod presets;
mod report;
mod scheduler;
mod settings;
mod safe_fs;
mod settings_transfer;
mod tray;
mod unelevated;
mod updater;
mod verify;

use grandhonor_core::protocol::{BacklightLevel, BatteryMode};
use std::process::ExitCode;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows_core::HSTRING;

#[derive(Debug, PartialEq, Eq)]
pub enum CliAction {
    RunTray { open_panel: bool },
    SetBatteryMode { interactive: bool, mode: BatteryMode },
    SetPowerUnlock { interactive: bool, enabled: bool },
    SetKeyboardBacklight { interactive: bool, level: BacklightLevel },
    SetKeyboardBacklightTimeout { interactive: bool, seconds: u16 },
    SetCameraOff { interactive: bool, off: bool },
    ReadSensors { request_id: String },
    ExportDrivers { path: String, pipe_id: Option<String> },
    ImportDrivers { path: String, pipe_id: Option<String> },
    CheckDrivers,
    VerifyDriverPackage { component_id: i32 },
    InstallPrivilegedTasks,
    UninstallPrivilegedTasks,
    ApplyUpdate { setup: String },
    ApplyRollback,
    Relaunch,
    /// Verification hooks for hardware checks: print live samples, show state, apply a preset.
    SampleLive { count: u32 },
    PrintState,
    ApplyPreset { key: &'static str },
    SetWindowsMode { mode: u32 },
    SetRefreshRate { hz: u32 },
}

pub fn parse_cli_args(args: &[String]) -> Option<CliAction> {
    if args.is_empty() {
        return Some(CliAction::RunTray { open_panel: true });
    }
    let cmd = args[0].as_str();
    let val = args.get(1).map(String::as_str);
    let extra = args.get(2).cloned();

    match cmd {
        "--background" => Some(CliAction::RunTray { open_panel: false }),
        "--set-battery-mode" => Some(CliAction::SetBatteryMode {
            interactive: true,
            mode: BatteryMode::parse(val?)?,
        }),
        "--apply-battery-mode" => Some(CliAction::SetBatteryMode {
            interactive: false,
            mode: BatteryMode::parse(val?)?,
        }),
        "--set-power-unlock" => Some(CliAction::SetPowerUnlock {
            interactive: true,
            enabled: parse_bool(val?)?,
        }),
        "--apply-power-unlock" => Some(CliAction::SetPowerUnlock {
            interactive: false,
            enabled: parse_bool(val?)?,
        }),
        "--set-keyboard-backlight" => Some(CliAction::SetKeyboardBacklight {
            interactive: true,
            level: BacklightLevel::parse(val?)?,
        }),
        "--apply-keyboard-backlight" => Some(CliAction::SetKeyboardBacklight {
            interactive: false,
            level: BacklightLevel::parse(val?)?,
        }),
        "--set-keyboard-backlight-timeout" => Some(CliAction::SetKeyboardBacklightTimeout {
            interactive: true,
            seconds: val?.parse::<u16>().ok()?,
        }),
        "--apply-keyboard-backlight-timeout" => Some(CliAction::SetKeyboardBacklightTimeout {
            interactive: false,
            seconds: val?.parse::<u16>().ok()?,
        }),
        "--set-camera-off" => Some(CliAction::SetCameraOff {
            interactive: true,
            off: parse_bool(val?)?,
        }),
        "--apply-camera-off" => Some(CliAction::SetCameraOff {
            interactive: false,
            off: parse_bool(val?)?,
        }),
        "--read-sensors" => {
            let id = val?;
            if id.is_empty() {
                return None;
            }
            Some(CliAction::ReadSensors {
                request_id: id.to_string(),
            })
        }
        "--export-drivers" => {
            let path = val?;
            if path.trim().is_empty() {
                return None;
            }
            Some(CliAction::ExportDrivers {
                path: path.to_string(),
                pipe_id: extra,
            })
        }
        "--import-drivers" => {
            let path = val?;
            if path.trim().is_empty() {
                return None;
            }
            Some(CliAction::ImportDrivers {
                path: path.to_string(),
                pipe_id: extra,
            })
        }
        "--check-drivers" => Some(CliAction::CheckDrivers),
        "--verify-driver-package" => {
            let id = val?.parse::<i32>().ok()?;
            if id <= 0 {
                return None;
            }
            Some(CliAction::VerifyDriverPackage { component_id: id })
        }
        "--install-privileged-tasks" => Some(CliAction::InstallPrivilegedTasks),
        "--uninstall-privileged-tasks" => Some(CliAction::UninstallPrivilegedTasks),
        "--apply-update" => {
            let setup = val?;
            if !setup.to_ascii_lowercase().ends_with(".exe") {
                return None;
            }
            Some(CliAction::ApplyUpdate { setup: setup.to_string() })
        }
        "--apply-rollback" => Some(CliAction::ApplyRollback),
        "--relaunch" => Some(CliAction::Relaunch),
        "--sample-live" => Some(CliAction::SampleLive {
            count: val?.parse::<u32>().ok().filter(|n| (1..=600).contains(n))?,
        }),
        "--print-state" => Some(CliAction::PrintState),
        "--apply-preset" => Some(CliAction::ApplyPreset {
            key: match val? {
                "ac" => "Ac",
                "dc" => "Dc",
                _ => return None,
            },
        }),
        "--set-windows-mode" => Some(CliAction::SetWindowsMode {
            mode: match val? {
                "efficiency" => 1,
                "balanced" => 2,
                "performance" => 3,
                _ => return None,
            },
        }),
        "--set-refresh-rate" => Some(CliAction::SetRefreshRate {
            hz: val?.parse::<u32>().ok().filter(|hz| (24..=500).contains(hz))?,
        }),
        _ => None,
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    if s.eq_ignore_ascii_case("true") {
        Some(true)
    } else if s.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn main() -> ExitCode {
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from("GrandHonor.App"));
    }
    grandhonor_core::locale::set_language_override(settings::language_preference());

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(action) = parse_cli_args(&args) else {
        return ExitCode::from(2);
    };

    let code = match action {
        CliAction::RunTray { open_panel } => run_single_instance(open_panel),
        CliAction::SetBatteryMode { interactive, mode } => {
            run_action(interactive, || {
                hw::battery::set_battery_mode(mode).map_err(|e| e.to_string())
            })
        }
        CliAction::SetPowerUnlock { interactive, enabled } => {
            run_action(interactive, || {
                hw::power::set_power_unlock(enabled).map_err(|e| e.to_string())
            })
        }
        CliAction::SetKeyboardBacklight { interactive, level } => {
            run_action(interactive, || {
                hw::backlight::set_backlight_level(level).map_err(|e| e.to_string())
            })
        }
        CliAction::SetKeyboardBacklightTimeout { interactive, seconds } => {
            run_action(interactive, || {
                hw::backlight::set_backlight_timeout(seconds).map_err(|e| e.to_string())
            })
        }
        CliAction::SetCameraOff { interactive, off } => {
            run_action(interactive, || hw::camera::set_camera_off(off))
        }
        CliAction::ReadSensors { request_id } => run_action(true, || {
            hw::sensors::read_and_store(&request_id)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }),
        CliAction::ExportDrivers { path, pipe_id } => {
            drivers::transfer::export_cli(Some(&path), pipe_id.as_deref())
        }
        CliAction::ImportDrivers { path, pipe_id } => {
            drivers::transfer::import_cli(Some(&path), pipe_id.as_deref())
        }
        CliAction::CheckDrivers => drivers::diagnostics::run_cli(None),
        CliAction::VerifyDriverPackage { component_id } => {
            drivers::diagnostics::run_cli(Some(component_id))
        }
        // Called by setup (possibly silent): never block it with a message box.
        CliAction::InstallPrivilegedTasks => run_action(false, scheduler::ensure_registered),
        CliAction::UninstallPrivilegedTasks => run_action(true, || {
            // Never leave a capped processor behind after removal.
            hw::cpu_limit::restore_original();
            // Give HONOR its own on-screen display back if GrandHonor had replaced it.
            if hw::honor_osd::replaced() {
                let _ = hw::honor_osd::set_replaced(false);
            }
            scheduler::remove_registered()
        }),
        CliAction::ApplyUpdate { setup } => updater::apply_update(&setup),
        CliAction::ApplyRollback => updater::apply_rollback(),
        CliAction::Relaunch => updater::relaunch(),
        CliAction::SampleLive { count } => verify::sample_live(count),
        CliAction::PrintState => verify::print_state(),
        CliAction::ApplyPreset { key } => {
            presets::apply(key);
            0
        }
        CliAction::SetWindowsMode { mode } => verify::set_windows_mode(mode),
        CliAction::SetRefreshRate { hz } => i32::from(!hw::display::set_refresh_rate(hz)),
    };

    ExitCode::from(code as u8)
}

fn run_action<F: FnOnce() -> Result<(), String>>(interactive: bool, func: F) -> i32 {
    match func() {
        Ok(()) => 0,
        Err(err) => {
            if interactive {
                menu_builder::show_error_box(&err);
            } else {
                log::error(&err);
            }
            1
        }
    }
}

struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn run_single_instance(open_panel: bool) -> i32 {
    let ev_name = HSTRING::from("GrandHonor.OpenPanel");
    let mutex_name = HSTRING::from("GrandHonor.SingleInstance");

    let Ok(event) = (unsafe { CreateEventW(None, false, false, &ev_name) }) else {
        return 1;
    };
    let _event_guard = HandleGuard(event);

    let Ok(mutex) = (unsafe { CreateMutexW(None, false, &mutex_name) }) else {
        return 1;
    };
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let _mutex_guard = HandleGuard(mutex);

    if open_panel {
        unsafe {
            let _ = SetEvent(event);
        }
    }
    if already_exists {
        return 0;
    }

    tray::run_tray_loop(event, open_panel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(arr: &[&str]) -> Vec<String> {
        arr.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn cli_validation_rejects_invalid_inputs() {
        assert!(parse_cli_args(&s(&["--unknown"])).is_none());
        assert!(parse_cli_args(&s(&["--apply-battery-mode", "42"])).is_none());
        assert!(parse_cli_args(&s(&["--apply-keyboard-backlight", "-1"])).is_none());
        assert!(parse_cli_args(&s(&["--apply-power-unlock", "invalid"])).is_none());
        assert!(parse_cli_args(&s(&["--export-drivers"])).is_none());
        assert!(parse_cli_args(&s(&["--import-drivers"])).is_none());
        assert!(parse_cli_args(&s(&["--verify-driver-package", "0"])).is_none());
        assert!(parse_cli_args(&s(&["--sample-live", "0"])).is_none());
        assert!(parse_cli_args(&s(&["--apply-preset", "both"])).is_none());
        assert!(parse_cli_args(&s(&["--set-windows-mode", "turbo"])).is_none());
        assert!(parse_cli_args(&s(&["--set-refresh-rate", "5"])).is_none());
        assert_eq!(parse_cli_args(&s(&["--apply-preset", "dc"])), Some(CliAction::ApplyPreset { key: "Dc" }));
        assert_eq!(parse_cli_args(&s(&["--sample-live", "20"])), Some(CliAction::SampleLive { count: 20 }));

        assert_eq!(
            parse_cli_args(&s(&[])),
            Some(CliAction::RunTray { open_panel: true })
        );
        assert_eq!(
            parse_cli_args(&s(&["--background"])),
            Some(CliAction::RunTray { open_panel: false })
        );
        assert_eq!(
            parse_cli_args(&s(&["--apply-battery-mode", "Home"])),
            Some(CliAction::SetBatteryMode {
                interactive: false,
                mode: BatteryMode::Home
            })
        );
    }
}

//! Drivers → Device check → System report: one text for support or a GitHub issue, copied to
//! the clipboard. It holds the model, BIOS, Windows build, GrandHonor version, battery and SSD
//! health, devices with a problem and the last lines of `GrandHonor.log` (its event log).
//! Nothing is sent anywhere.

use crate::settings;
use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;

const LOG_LINES: usize = 60;

fn windows_build() -> String {
    let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let name = settings::read_hklm_string(key, "ProductName").unwrap_or_else(|| "Windows".into());
    let display = settings::read_hklm_string(key, "DisplayVersion").unwrap_or_default();
    let build = settings::read_hklm_string(key, "CurrentBuild").unwrap_or_default();
    let ubr = settings::read_hklm_dword(key, "UBR").map(|u| format!(".{u}")).unwrap_or_default();
    format!("{name} {display} (build {build}{ubr})")
}

/// The last lines of the GrandHonor log.
pub fn log_tail(lines: usize) -> Vec<String> {
    let text = std::fs::read_to_string(crate::log::log_path()).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].iter().map(|s| private(s)).collect()
}

/// The report is meant for a public GitHub issue: the user's profile folder (it holds the
/// account name) becomes %USERPROFILE%.
fn private(text: &str) -> String {
    let profile = crate::known_dirs::roaming_app_data();
    let Some(home) = profile.parent().and_then(|p| p.parent()) else { return text.to_string() };
    let home = home.to_string_lossy();
    if home.is_empty() {
        return text.to_string();
    }
    let mut out = text.replace(home.as_ref(), "%USERPROFILE%");
    out = out.replace(&home.to_lowercase(), "%USERPROFILE%");
    out
}

pub fn build() -> String {
    let bios_key = r"HARDWARE\DESCRIPTION\System\BIOS";
    let mut out = vec![format!("GrandHonor {}", env!("CARGO_PKG_VERSION"))];
    let family = settings::read_hklm_string(bios_key, "SystemFamily").unwrap_or_default();
    let product = settings::read_hklm_string(bios_key, "SystemProductName").unwrap_or_default();
    out.push(format!("Model: {} {}", family.trim(), product.trim()));
    out.push(format!("BIOS: {}", crate::hw::firmware::bios_version().unwrap_or_else(|| "?".into())));
    out.push(format!("Windows: {}", windows_build()));
    if let Some(h) = crate::hw::battery_health::read() {
        let cycles = h.cycles.map(|c| format!(", {c} cycles")).unwrap_or_default();
        out.push(format!("Battery: {}% health, {} of {} mWh{cycles}", h.percent(), h.full_mwh, h.design_mwh));
    }
    if let Some(s) = crate::hw::storage::read() {
        out.push(format!(
            "SSD: {}, {}% used, {}% spare, {} °C, {:.1} TB written, {} h, {} media errors{}",
            s.model,
            s.used_percent,
            s.spare_percent,
            s.temperature_c,
            s.written_tb,
            s.power_on_hours,
            s.media_errors,
            if s.warning { ", CRITICAL WARNING" } else { "" }
        ));
    }
    let problems = crate::hw::devices::problems();
    if problems.is_empty() {
        out.push("Devices: no problems".into());
    } else {
        out.push("Devices with a problem:".into());
        for d in problems {
            // Only the bus and vendor/product part: the rest can be a device serial number.
            let id: String = d.instance_id.split('\\').take(2).collect::<Vec<_>>().join("\\");
            out.push(format!("  code {}: {} ({id})", d.problem, d.name));
        }
    }
    out.push(String::new());
    out.push(format!("Log ({}), last {LOG_LINES} lines:", private(&crate::log::log_path().display().to_string())));
    out.extend(log_tail(LOG_LINES));
    out.join("\r\n")
}

/// Puts text on the clipboard as Unicode.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        OpenClipboard(None).map_err(|e| e.to_string())?;
        let result = (|| {
            EmptyClipboard().map_err(|e| e.to_string())?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| e.to_string())?;
            let ptr = GlobalLock(mem) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err("GlobalLock failed".to_string());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e.to_string());
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

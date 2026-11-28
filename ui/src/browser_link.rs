//! Native messaging host for the GrandHonor browser extension.
//!
//! The browser starts `GrandHonor.Panel.exe chrome-extension://<id>/` unelevated with its stdio
//! as pipes. This mode sends the extension's options from `settings.json` (key `BrowserVsr`)
//! and sends them again whenever the file changes. It waits on a directory change notification,
//! so it uses no CPU between changes, and exits when the browser closes the port (stdin EOF).

use std::io::{Read, Write};
use std::path::PathBuf;
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_FILE_NAME, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
use windows::core::PCWSTR;

/// The only extension allowed to talk to GrandHonor (fixed by the key in its manifest). The key is
/// public, so the origin is not a secret: this link only ever sends options and accepts no commands.
pub const EXTENSION_ORIGIN: &str = "chrome-extension://hkhjfheinfpppebmejofemdgpimphgpd/";

/// Bits of `BrowserVsr`; the default turns restoration, badge and frame rate on.
const ENABLED: u32 = 1;
const BADGE: u32 = 2;
const STATS: u32 = 4;
const COMPARE: u32 = 8;
/// NPU restoration, and Intel options whose bits switch things off; same layout as the host's
/// `settings::browser_vsr`.
const NPU: u32 = 16;
const NO_UPSCALE: u32 = 32;
const NO_FRAME_GEN: u32 = 64;
const SHARPNESS_SHIFT: u32 = 7;
const FRAMES_120: u32 = 512;
/// Battery saver off (stored inverted so it is on by default).
const NO_SAVER: u32 = 1024;
const DEFAULT: u32 = ENABLED | BADGE | STATS | NPU | NO_UPSCALE;

pub fn is_invocation(arg: &std::ffi::OsStr) -> bool {
    arg.to_str().is_some_and(|s| s.starts_with("chrome-extension://"))
}

pub fn run(origin: &std::ffi::OsStr) {
    if origin.to_str() != Some(EXTENSION_ORIGIN) {
        return;
    }
    let Some(dir) = settings_dir() else { return };
    // The browser closes stdin when the port closes: end the process then.
    std::thread::spawn(|| {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
        std::process::exit(0);
    });
    let mut last = None;
    if send_if_changed(&dir, &mut last).is_err() {
        return;
    }
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let Ok(handle) = (unsafe {
        FindFirstChangeNotificationW(PCWSTR(wide.as_ptr()), false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME)
    }) else {
        // No notification: keep the first answer until the browser closes the port.
        loop {
            std::thread::park();
        }
    };
    loop {
        if unsafe { WaitForSingleObject(handle, INFINITE) }.0 != 0 {
            break;
        }
        // Settings are written to a temp file and renamed; let the rename finish.
        std::thread::sleep(std::time::Duration::from_millis(50));
        if send_if_changed(&dir, &mut last).is_err() || unsafe { FindNextChangeNotification(handle) }.is_err() {
            break;
        }
    }
    unsafe {
        let _ = FindCloseChangeNotification(handle);
    }
}

/// <profile>\AppData\Roaming\GrandHonor, built like the host's known_dirs::roaming_app_data
/// (from the profile directory, not the redirectable shell folder), so both read one file.
fn settings_dir() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::UI::Shell::GetUserProfileDirectoryW;
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = GetUserProfileDirectoryW(token, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len).is_ok();
        let _ = CloseHandle(token);
        let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
        if !ok || n == 0 {
            return None;
        }
        let profile = PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]));
        Some(profile.join("AppData").join("Roaming").join("GrandHonor"))
    }
}

/// The options, or None while the file cannot be read or parsed (being replaced): a missing
/// file or key means the defaults, a failed read must not push them over the user's choice.
/// The interface language chosen in GrandHonor (`ru`, `en`, `zh`), or None for the system one.
fn read_language(dir: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(dir.join("settings.json")).ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    let lang = value.get("settings")?.get("Language")?.as_str()?.to_ascii_lowercase();
    ["ru", "en", "zh"].into_iter().find(|code| lang.starts_with(code)).map(str::to_string)
}

fn read_bits(dir: &std::path::Path) -> Option<u32> {
    let bytes = match std::fs::read(dir.join("settings.json")) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(DEFAULT),
        Err(_) => return None,
    };
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    Some(value.get("settings").and_then(|s| s.get("BrowserVsr")).and_then(|v| v.as_u64()).map_or(DEFAULT, |v| v as u32 & 2047))
}

/// The NPU restoration model shipped next to this executable.
fn npu_model_present() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("BrowserVsr").join("models").join("liveaction-webnn.bin")))
        .is_some_and(|path| path.is_file())
}

fn send_if_changed(dir: &std::path::Path, last: &mut Option<(u32, Option<String>)>) -> std::io::Result<()> {
    let mut bits = read_bits(dir);
    for _ in 0..5 {
        if bits.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        bits = read_bits(dir);
    }
    let Some(bits) = bits else { return Ok(()) };
    let language = read_language(dir).or_else(|| Some(grandhonor_core::locale::system_language().code().to_string()));
    let current = Some((bits, language.clone()));
    if *last == current {
        return Ok(());
    }
    *last = current;
    let message = serde_json::json!({
        "enabled": bits & ENABLED != 0,
        "badge": bits & BADGE != 0,
        "stats": bits & STATS != 0,
        "compare": bits & COMPARE != 0,
        // NPU restoration needs its model, which builds without redistributable weights lack.
        "npu": bits & NPU != 0 && npu_model_present(),
        "upscale": bits & NO_UPSCALE == 0,
        "frameGen": bits & NO_FRAME_GEN == 0,
        "frameRate": if bits & FRAMES_120 != 0 { 120 } else { 60 },
        // Stored 0 is the default (medium); the extension gets 0 off .. 3 strong.
        "sharpness": match (bits >> SHARPNESS_SHIFT) & 3 { 1 => 0, 2 => 1, 3 => 3, _ => 2 },
        // On battery the extension limits frame generation (it reads the charger state itself).
        "saver": bits & NO_SAVER == 0,
        "language": language,
    })
    .to_string();
    let mut out = std::io::stdout().lock();
    out.write_all(&(message.len() as u32).to_le_bytes())?;
    out.write_all(message.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    #[test]
    fn origin_must_match() {
        assert!(super::is_invocation(std::ffi::OsStr::new(super::EXTENSION_ORIGIN)));
        assert!(!super::is_invocation(std::ffi::OsStr::new("--shortcut-icon")));
    }
}

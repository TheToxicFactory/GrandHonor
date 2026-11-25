//! System folders from Windows itself, never from environment variables: the host runs
//! elevated, and a user program can set `SystemRoot`, `ProgramFiles` or `ProgramData` in
//! `HKCU\Environment`, which would point an elevated launch or write at its own folder.

use std::path::PathBuf;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::SystemInformation::{GetSystemDirectoryW, GetSystemWindowsDirectoryW};
use windows::Win32::UI::Shell::{
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows_core::GUID;

fn from_buffer(f: impl Fn(&mut [u16]) -> u32, fallback: &str) -> PathBuf {
    let mut buf = [0u16; 512];
    let len = f(&mut buf) as usize;
    if len == 0 || len >= buf.len() {
        return PathBuf::from(fallback);
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..len]))
}

/// `C:\Windows`.
pub fn windows() -> PathBuf {
    from_buffer(|b| unsafe { GetSystemWindowsDirectoryW(Some(b)) }, r"C:\Windows")
}

/// `C:\Windows\System32`.
pub fn system32() -> PathBuf {
    from_buffer(|b| unsafe { GetSystemDirectoryW(Some(b)) }, r"C:\Windows\System32")
}

fn known(id: &GUID, fallback: &str) -> PathBuf {
    unsafe {
        match SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            Ok(p) => {
                let path = p.to_string().map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(fallback));
                CoTaskMemFree(Some(p.0 as *const _));
                path
            }
            Err(_) => PathBuf::from(fallback),
        }
    }
}

/// `C:\Program Files` (the 64-bit one: the host is a 64-bit process).
pub fn program_files() -> PathBuf {
    known(&FOLDERID_ProgramFiles, r"C:\Program Files")
}

/// `C:\ProgramData`.
pub fn program_data() -> PathBuf {
    known(&FOLDERID_ProgramData, r"C:\ProgramData")
}

/// The signed-in user's profile folder (`C:\Users\name`). Windows takes it from
/// `HKLM\...\ProfileList`, which only administrators can change; the per-user shell folder
/// settings (`HKCU\...\User Shell Folders`) behind `%APPDATA%` and `SHGetKnownFolderPath` can
/// be rewritten by any program the user runs, so the elevated host does not use them.
fn profile() -> PathBuf {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::UI::Shell::GetUserProfileDirectoryW;
    // Without a profile, a folder inside the protected ProgramData\GrandHonor.
    let fallback = || program_data().join("GrandHonor").join("no-profile");
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return fallback();
        }
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = GetUserProfileDirectoryW(token, Some(windows_core::PWSTR(buf.as_mut_ptr())), &mut len).is_ok();
        let _ = CloseHandle(token);
        let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
        if !ok || n == 0 {
            return fallback();
        }
        PathBuf::from(String::from_utf16_lossy(&buf[..n]))
    }
}

/// The user's roaming AppData (`C:\Users\name\AppData\Roaming`).
pub fn roaming_app_data() -> PathBuf {
    profile().join(r"AppData\Roaming")
}

/// The user's local AppData (`C:\Users\name\AppData\Local`).
pub fn local_app_data() -> PathBuf {
    profile().join(r"AppData\Local")
}

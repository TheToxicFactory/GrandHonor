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

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

static LOG_GATE: OnceLock<Mutex<()>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn log_path() -> &'static PathBuf {
    LOG_PATH.get_or_init(|| {
        let base = crate::known_dirs::local_app_data();
        base.join("GrandHonor").join("GrandHonor.log")
    })
}

pub fn info(message: &str) {
    write_entry("INFO", message);
}

pub fn error(message: &str) {
    write_entry("ERROR", message);
}

pub fn error_with(context: &str, err: &dyn std::fmt::Display) {
    write_entry("ERROR", &format!("{context} | {err}"));
}

fn write_entry(level: &str, message: &str) {
    let gate = LOG_GATE.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = gate.lock() else {
        return;
    };
    let path = log_path();
    // The log folder is the user's and the host is elevated: no writes through links.
    if path.parent().is_none_or(|dir| crate::safe_fs::prepare_dir(dir).is_err()) {
        return;
    }
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.len() > MAX_LOG_BYTES
    {
        let _ = crate::safe_fs::rename(path, &path.with_extension("log.1"));
    }
    let st = unsafe { GetLocalTime() };
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{}] {}\r\n",
        st.wYear,
        st.wMonth,
        st.wDay,
        st.wHour,
        st.wMinute,
        st.wSecond,
        st.wMilliseconds,
        level,
        message
    );
    let _ = crate::safe_fs::append(path, line.as_bytes());
}

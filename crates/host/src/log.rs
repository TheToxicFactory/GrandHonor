use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

static LOG_GATE: OnceLock<Mutex<()>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn log_path() -> &'static PathBuf {
    LOG_PATH.get_or_init(|| {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
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
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(meta) = fs::metadata(path)
        && meta.len() > MAX_LOG_BYTES
    {
        let backup = path.with_extension("log.1");
        let _ = fs::rename(path, backup);
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
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

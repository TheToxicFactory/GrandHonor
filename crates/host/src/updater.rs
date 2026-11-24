//! GrandHonor self-update from its GitHub releases, with automatic and manual rollback.
//!
//! 1. `check` reads the latest release from the GitHub API (api.github.com only).
//! 2. `start_update` downloads `GrandHonor-Setup-x64.exe` into a Windows Temp folder only
//!    Administrators and SYSTEM can write, and requires the file's SHA-256 to equal the
//!    `digest` GitHub publishes for that asset. It backs up the installed host and panel to
//!    `%ProgramData%\GrandHonor\rollback`, copies itself next to the setup as the helper, starts
//!    `helper --apply-update <setup>` and lets the host quit.
//! 3. The helper runs the setup silently, starts the new version and waits 15 s. If setup fails
//!    or the new host does not stay up, it restores the backup and starts the old version.
//! 4. `start_rollback` brings back the backed-up version on request (System tab).

use crate::drivers::http::{download_to_file_with, get_text};
use crate::drivers::trust::compute_sha256_hex;
use crate::scheduler::{create_protected_directory, install_root, is_installation_path};
use grandhonor_core::url_guard::validate_update_redirect;
use grandhonor_core::version::is_newer_version;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows_core::HSTRING;

const RELEASES_API: &str = "https://api.github.com/repos/TheToxicFactory/GrandHonor/releases/latest";
const SETUP_ASSET: &str = "GrandHonor-Setup-x64.exe";
const MAX_SETUP_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub enum State {
    UpToDate,
    Available(Release),
    Failed(String),
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub fn state() -> Option<State> {
    STATE.lock().ok().and_then(|s| s.clone())
}

fn set_state(state: State) {
    if let Ok(mut slot) = STATE.lock() {
        *slot = Some(state);
    }
}

/// Asks GitHub for the latest release and remembers the answer for the panel.
pub fn check() {
    set_state(match fetch_latest() {
        Ok(Some(release)) => State::Available(release),
        Ok(None) => State::UpToDate,
        Err(err) => {
            crate::log::error(&format!("Update check failed: {err}"));
            State::Failed(err)
        }
    });
}

fn fetch_latest() -> Result<Option<Release>, String> {
    let response = get_text(RELEASES_API, 15_000)?;
    if matches!(response.status, 403 | 429) && is_rate_limited(&response.body) {
        return Err(rate_limit_message(response.rate_limit_reset));
    }
    if response.status != 200 {
        return Err(format!("GitHub answered {}", response.status));
    }
    parse_release(&response.body, env!("CARGO_PKG_VERSION"))
}

/// GitHub allows 60 unauthenticated API requests an hour per address; a VPN or a shared
/// address can use them up. Its answer then says "API rate limit exceeded".
fn is_rate_limited(body: &[u8]) -> bool {
    String::from_utf8_lossy(body).contains("rate limit")
}

fn rate_limit_message(reset: Option<u64>) -> String {
    use grandhonor_core::locale::t;
    let when = reset.and_then(|unix| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        let minutes = unix.saturating_sub(now).div_ceil(60).max(1);
        Some(format!(" {} {minutes} {}", t("через", "in", ""), t("мин", "min", "分钟后")))
    });
    format!(
        "{}{}",
        t(
            "GitHub временно ограничил проверки с вашего адреса (часто из-за VPN). Повторите",
            "GitHub has paused checks from your address for a while (often a VPN). Try again",
            "GitHub 暂时限制了您的地址（常见于 VPN），请稍后重试",
        ),
        when.unwrap_or_else(|| t(" позже", " later", "").to_string()),
    )
}

fn parse_release(body: &[u8], current: &str) -> Result<Option<Release>, String> {
    let json: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let tag = json["tag_name"].as_str().ok_or("Release has no tag")?;
    let version = tag.trim_start_matches('v').to_string();
    if !is_newer_version(&version, current) {
        return Ok(None);
    }
    let asset = json["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"].as_str() == Some(SETUP_ASSET)))
        .ok_or("The release has no setup file")?;
    let url = asset["browser_download_url"].as_str().ok_or("No download URL")?.to_string();
    let size = asset["size"].as_u64().ok_or("No size")?;
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("GitHub published no SHA-256 for the setup; refusing to install it")?
        .to_string();
    if size == 0 || size > MAX_SETUP_SIZE {
        return Err("Unexpected setup size".into());
    }
    Ok(Some(Release { version, url, size, sha256 }))
}

pub fn rollback_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("GrandHonor")
        .join("rollback")
}

/// Version kept for rollback, if a complete backup exists.
pub fn rollback_version() -> Option<String> {
    let dir = rollback_dir();
    if !dir.join("GrandHonor.exe").is_file() || !dir.join("GrandHonor.Panel.exe").is_file() {
        return None;
    }
    std::fs::read_to_string(dir.join("version.txt")).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn staging_dir(tag: &str) -> Result<PathBuf, String> {
    let windows = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let dir = windows.join("Temp").join(format!("GrandHonor-{tag}-{}", std::process::id()));
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    create_protected_directory(&dir, true)?;
    Ok(dir)
}

fn backup_current() -> Result<(), String> {
    let app = install_root();
    let dir = rollback_dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("Could not clear the old backup: {e}"))?;
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    create_protected_directory(&dir, true)?;
    for file in ["GrandHonor.exe", "GrandHonor.Panel.exe"] {
        std::fs::copy(app.join(file), dir.join(file)).map_err(|e| format!("Backup of {file} failed: {e}"))?;
    }
    std::fs::write(dir.join("version.txt"), env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())
}

fn spawn_helper(dir: &Path, args: &[&str]) -> Result<(), String> {
    let helper = dir.join("GrandHonor-updater.exe");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::copy(&me, &helper).map_err(|e| e.to_string())?;
    Command::new(&helper).args(args).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Downloads, verifies and hands over to the helper. On `Ok` the caller must quit the host.
pub fn start_update(release: &Release) -> Result<(), String> {
    if !is_installation_path() {
        return Err("Updates install only into the regular Program Files installation.".into());
    }
    let url = url::Url::parse(&release.url).map_err(|e| e.to_string())?;
    if url.scheme() != "https" || !grandhonor_core::url_guard::is_trusted_update_host(url.host_str().unwrap_or("")) {
        return Err("The download address is not GrandHonor's GitHub release.".into());
    }
    let dir = staging_dir("update")?;
    let setup = dir.join(SETUP_ASSET);
    let status = download_to_file_with(&url, &setup, release.size, 120_000, validate_update_redirect)?;
    if status != 200 {
        return Err(format!("Download failed ({status})"));
    }
    let size = std::fs::metadata(&setup).map_err(|e| e.to_string())?.len();
    let hash = compute_sha256_hex(&setup)?;
    if size != release.size || !hash.eq_ignore_ascii_case(&release.sha256) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("The downloaded setup does not match GitHub's checksum. Nothing was changed.".into());
    }
    backup_current()?;
    crate::settings_transfer::refresh_backup();
    spawn_helper(&dir, &["--apply-update", &setup.to_string_lossy()])
}

/// Hands the rollback to the helper. On `Ok` the caller must quit the host.
pub fn start_rollback() -> Result<(), String> {
    if rollback_version().is_none() {
        return Err("No previous version is stored.".into());
    }
    let dir = staging_dir("rollback")?;
    spawn_helper(&dir, &["--apply-rollback"])
}

fn host_running() -> bool {
    let name = HSTRING::from("GrandHonor.SingleInstance");
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &name) } {
        Ok(handle) => {
            unsafe {
                let _ = CloseHandle(handle);
            }
            true
        }
        Err(_) => false,
    }
}

fn wait_host_exit(limit: Duration) -> bool {
    let start = Instant::now();
    while host_running() {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    true
}

fn start_installed_host() -> Option<std::process::Child> {
    Command::new(install_root().join("GrandHonor.exe")).arg("--background").spawn().ok()
}

/// The new host must still be running 15 s after start.
fn healthy(child: Option<std::process::Child>) -> bool {
    let Some(mut child) = child else { return false };
    std::thread::sleep(Duration::from_secs(15));
    matches!(child.try_wait(), Ok(None)) && host_running()
}

fn restore_backup() -> Result<(), String> {
    let app = install_root();
    let dir = rollback_dir();
    for file in ["GrandHonor.exe", "GrandHonor.Panel.exe"] {
        let mut last = String::new();
        let ok = (0..20).any(|_| match std::fs::copy(dir.join(file), app.join(file)) {
            Ok(_) => true,
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(500));
                false
            }
        });
        if !ok {
            return Err(format!("Could not restore {file}: {last}"));
        }
    }
    Ok(())
}

/// Helper entry: `--apply-update <setup>`.
pub fn apply_update(setup: &str) -> i32 {
    crate::log::info("Update helper started");
    if !wait_host_exit(Duration::from_secs(30)) {
        crate::log::error("Update aborted: GrandHonor did not exit");
        return 1;
    }
    let code = Command::new(setup)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"])
        .status()
        .ok()
        .and_then(|s| s.code());
    if code == Some(0) && healthy(start_installed_host()) {
        crate::log::info("Update installed and the new version is running");
        return 0;
    }
    crate::log::error(&format!("Update failed (setup exit {code:?}); restoring the previous version"));
    let _ = wait_host_exit(Duration::from_secs(10));
    match restore_backup() {
        Ok(()) => {
            let _ = start_installed_host();
            2
        }
        Err(err) => {
            crate::log::error(&err);
            3
        }
    }
}

/// `--relaunch`: waits for the running GrandHonor to exit, then starts this executable again
/// (after a settings import, so every setting is applied by a clean start).
pub fn relaunch() -> i32 {
    if !wait_host_exit(Duration::from_secs(30)) {
        return 1;
    }
    match std::env::current_exe().and_then(|exe| Command::new(exe).arg("--background").spawn()) {
        Ok(_) => 0,
        Err(err) => {
            crate::log::error(&format!("Relaunch failed: {err}"));
            1
        }
    }
}

/// Starts the `--relaunch` helper and returns; the caller then quits the host.
pub fn spawn_relaunch() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Command::new(exe).arg("--relaunch").spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Helper entry: `--apply-rollback`.
pub fn apply_rollback() -> i32 {
    if !wait_host_exit(Duration::from_secs(30)) {
        return 1;
    }
    match restore_backup() {
        Ok(()) => {
            crate::log::info("Rolled back to the stored version");
            let _ = start_installed_host();
            0
        }
        Err(err) => {
            crate::log::error(&err);
            let _ = start_installed_host();
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"{"tag_name":"v2.9.0","assets":[{"name":"GrandHonor-Setup-x64.exe","size":3913582,
        "digest":"sha256:16a1b30d36cebdc1001c922de1de41e0a77d2666be3d962a7aadb27680fedc90",
        "browser_download_url":"https://github.com/o/r/releases/download/v2.9.0/GrandHonor-Setup-x64.exe"}]}"#;

    #[test]
    fn parses_newer_release_and_requires_digest() {
        let release = parse_release(BODY.as_bytes(), "2.1.6").unwrap().unwrap();
        assert_eq!(release.version, "2.9.0");
        assert_eq!(release.size, 3913582);
        assert!(parse_release(BODY.as_bytes(), "2.9.0").unwrap().is_none());
        let no_digest = BODY.replace("sha256:16a1", "md5:16a1");
        assert!(parse_release(no_digest.as_bytes(), "2.1.6").is_err());
    }
}
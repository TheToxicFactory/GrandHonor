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

use crate::drivers::http::{download_to_file_with, get_text, get_text_once};
use crate::drivers::trust::compute_sha256_hex;
use crate::scheduler::{create_protected_directory, install_root, is_installation_path, is_path_protected};
use grandhonor_core::url_guard::validate_update_redirect;
use grandhonor_core::version::is_newer_version;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows_core::HSTRING;

/// Release files of this repository only (not any github.com path).
const RELEASE_DOWNLOAD_PATH: &str = "/TheToxicFactory/GrandHonor/releases/download/";
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

static CHECKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LAST_CHECK: Mutex<Option<Instant>> = Mutex::new(None);
pub const CHECK_INTERVAL: Duration = Duration::from_secs(3600);
const CHECK_COALESCE: Duration = Duration::from_secs(55 * 60);

/// True while a check runs (the panel shows "Checking…" and asks again shortly).
pub fn checking() -> bool {
    CHECKING.load(std::sync::atomic::Ordering::Acquire)
}

/// Asks GitHub for the latest release on a background thread (up to 15 s) and remembers the
/// answer for the panel; the panel stays usable meanwhile.
pub fn check() {
    start_check(false);
}

pub fn check_automatically() {
    if crate::settings::automatic_update_checks() && !installing() {
        start_check(true);
    }
}

fn start_check(automatic: bool) {
    use std::sync::atomic::Ordering;
    if CHECKING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Ok(mut last) = LAST_CHECK.lock() {
        if automatic && last.is_some_and(|previous| previous.elapsed() < CHECK_COALESCE) {
            CHECKING.store(false, Ordering::Release);
            return;
        }
        *last = Some(Instant::now());
    }
    let spawned = std::thread::Builder::new().name("update-check".into()).stack_size(256 * 1024).spawn(move || {
        match fetch_latest(automatic) {
            Ok(Some(release)) => {
                let changed = !matches!(state(), Some(State::Available(previous)) if previous.version == release.version);
                set_state(State::Available(release));
                if automatic && changed && crate::settings::automatic_update_checks() {
                    crate::osd::notify(crate::osd::Icon::Display, grandhonor_core::locale::t(
                        "Доступно обновление GrandHonor",
                        "GrandHonor update available",
                        "GrandHonor 有可用更新",
                    ));
                }
            }
            Ok(None) => set_state(State::UpToDate),
            Err(err) => {
                crate::log::error(&format!("Update check failed: {err}"));
                if !automatic {
                    set_state(State::Failed(err));
                }
            }
        }
        CHECKING.store(false, Ordering::Release);
    });
    if spawned.is_err() {
        CHECKING.store(false, Ordering::Release);
    }
}

fn fetch_latest(automatic: bool) -> Result<Option<Release>, String> {
    let response = if automatic { get_text_once(RELEASES_API, 8_000) } else { get_text(RELEASES_API, 15_000) }?;
    if matches!(response.status, 403 | 429) && is_rate_limited(&response.body) {
        return Err(rate_limit_message(response.rate_limit_reset));
    }
    if response.status != 200 {
        return Err(if response.status == 403 {
            grandhonor_core::locale::t(
                "GitHub отказал в проверке (часто из-за VPN или лимита запросов). Повторите позже",
                "GitHub refused the check (often a VPN or the request limit). Try again later",
                "GitHub 拒绝了检查（常见于 VPN 或请求限制），请稍后重试",
            )
            .to_string()
        } else {
            format!("{} {}", grandhonor_core::locale::t("Ответ GitHub:", "GitHub answered", "GitHub 返回"), response.status)
        });
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
    let minutes = reset.and_then(|unix| {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(unix.saturating_sub(now).div_ceil(60).max(1))
    });
    // Whole sentences per language: word order differs, so no glued pieces.
    match minutes {
        Some(m) => t(
            "GitHub временно ограничил проверки с вашего адреса (часто из-за VPN). Повторите через {m} мин",
            "GitHub has paused checks from your address for a while (often a VPN). Try again in {m} min",
            "GitHub 暂时限制了您的地址（常见于 VPN），请在 {m} 分钟后重试",
        )
        .replace("{m}", &m.to_string()),
        None => t(
            "GitHub временно ограничил проверки с вашего адреса (часто из-за VPN). Повторите позже",
            "GitHub has paused checks from your address for a while (often a VPN). Try again later",
            "GitHub 暂时限制了您的地址（常见于 VPN），请稍后重试",
        )
        .to_string(),
    }
}

fn parse_release(body: &[u8], current: &str) -> Result<Option<Release>, String> {
    let json: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let tag = json["tag_name"].as_str().ok_or(grandhonor_core::locale::t("У выпуска нет версии", "Release has no tag", "发布没有版本号"))?;
    let version = tag.trim_start_matches('v').to_string();
    if !is_newer_version(&version, current) {
        return Ok(None);
    }
    let asset = json["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"].as_str() == Some(SETUP_ASSET)))
        .ok_or(grandhonor_core::locale::t("В выпуске нет установщика", "The release has no setup file", "发布中没有安装程序"))?;
    let url = asset["browser_download_url"].as_str().ok_or(grandhonor_core::locale::t("Нет адреса загрузки", "No download URL", "没有下载地址"))?.to_string();
    let size = asset["size"].as_u64().ok_or(grandhonor_core::locale::t("Не указан размер", "No size", "未注明大小"))?;
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or(grandhonor_core::locale::t("GitHub не опубликовал SHA-256 установщика, устанавливать нельзя", "GitHub published no SHA-256 for the setup; refusing to install it", "GitHub 未公布安装程序的 SHA-256，拒绝安装"))?
        .to_string();
    if size == 0 || size > MAX_SETUP_SIZE {
        return Err(grandhonor_core::locale::t("Неожиданный размер установщика", "Unexpected setup size", "安装程序大小异常").into());
    }
    Ok(Some(Release { version, url, size, sha256 }))
}

pub fn rollback_dir() -> PathBuf {
    crate::known_dirs::program_data()
        .join("GrandHonor")
        .join("rollback")
}

/// `C:\ProgramData\GrandHonor` must be writable by administrators and SYSTEM only: under
/// ProgramData a normal user may create folders, and a folder they own could hold a forged
/// "previous version" that a rollback would copy into Program Files and start elevated.
/// A folder that is not protected is removed and created again with a protected ACL.
fn ensure_protected_root() -> Result<(), String> {
    let root = rollback_dir().parent().map(Path::to_path_buf).ok_or(grandhonor_core::locale::t("Нет папки ProgramData", "No ProgramData folder", "没有 ProgramData 文件夹"))?;
    if root.exists() && !is_path_protected(&root) {
        crate::log::error(&format!(
            "{} was not protected ({}); recreating it",
            root.display(),
            crate::scheduler::describe_security(&root)
        ));
        std::fs::remove_dir_all(&root).map_err(|e| format!("{} {}: {e}", grandhonor_core::locale::t("Не удалось удалить", "Could not remove", "无法删除"), root.display()))?;
    }
    if !root.exists() {
        create_protected_directory(&root, true)?;
    }
    if is_path_protected(&root) { Ok(()) } else { Err(format!("{}: {}", grandhonor_core::locale::t("Папка не защищена", "Folder is not protected", "文件夹未受保护"), root.display())) }
}

/// The backup can be trusted: it and its parent are protected folders.
fn backup_trusted() -> bool {
    let dir = rollback_dir();
    dir.parent().is_some_and(is_path_protected) && is_path_protected(&dir)
}

/// Version kept for rollback, if a complete and trusted backup exists.
pub fn rollback_version() -> Option<String> {
    let dir = rollback_dir();
    if !backup_trusted() {
        return None;
    }
    if !dir.join("GrandHonor.exe").is_file() || !dir.join("GrandHonor.Panel.exe").is_file() {
        return None;
    }
    std::fs::read_to_string(dir.join("version.txt")).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn staging_dir(tag: &str) -> Result<PathBuf, String> {
    let windows = crate::known_dirs::windows();
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
    ensure_protected_root()?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", grandhonor_core::locale::t("Не удалось очистить старую копию", "Could not clear the old backup", "无法清除旧备份")))?;
    }
    create_protected_directory(&dir, true)?;
    // The whole installation (executables, extension, native messaging manifests), so a
    // rollback returns every file of the previous version, not only the two executables.
    copy_tree(&app, &dir, true).map_err(|e| format!("{} {e}", grandhonor_core::locale::t("Не удалось сохранить копию", "Backup failed:", "备份失败：")))?;
    std::fs::write(dir.join("version.txt"), env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())
}

/// Files of the installation that a backup skips: the uninstaller (setup writes its own) and
/// renamed copies of running executables.
fn skipped_in_backup(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("unins") || lower.ends_with(".old") || lower == "version.txt"
}

/// Copies `from` into `to` recursively (no links followed). `top` marks the installation root,
/// where uninstaller files are skipped.
fn copy_tree(from: &Path, to: &Path, top: bool) -> Result<(), String> {
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if top && skipped_in_backup(&name.to_string_lossy()) {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let target = to.join(&name);
        if kind.is_symlink() {
            continue;
        } else if kind.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            copy_tree(&entry.path(), &target, false)?;
        } else {
            copy_with_retry(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// A file can be held for a moment by the exiting host or the browser: up to 10 s of retries.
fn copy_with_retry(from: &Path, to: &Path) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..20 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    Err(format!("{}: {last}", to.display()))
}

fn spawn_helper(dir: &Path, args: &[&str]) -> Result<(), String> {
    let helper = dir.join("GrandHonor-updater.exe");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    std::fs::copy(&me, &helper).map_err(|e| e.to_string())?;
    Command::new(&helper).args(args).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True while an update downloads and is checked (the panel keeps asking meanwhile).
pub fn installing() -> bool {
    INSTALLING.load(std::sync::atomic::Ordering::Acquire)
}

/// Runs `start_update` on its own thread so the panel stays responsive during the download;
/// `done` gets the result (on `Ok` it must quit the host).
pub fn start_update_in_background(release: Release, done: impl FnOnce(Result<(), String>) + Send + 'static) {
    use std::sync::atomic::Ordering;
    if INSTALLING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new().name("update-install".into()).spawn(move || {
        let result = start_update(&release);
        // The result first: the panel keeps asking while INSTALLING is set, so the banner
        // is in place before it stops.
        done(result);
        INSTALLING.store(false, Ordering::Release);
    });
    if spawned.is_err() {
        INSTALLING.store(false, Ordering::Release);
    }
}

/// Downloads, verifies and hands over to the helper. On `Ok` the caller must quit the host.
pub fn start_update(release: &Release) -> Result<(), String> {
    use grandhonor_core::locale::t;
    if !is_installation_path() {
        return Err(t(
            "Обновление ставится только в обычную установку в Program Files.",
            "Updates install only into the regular Program Files installation.",
            "更新只能安装到 Program Files 中的常规安装。",
        )
        .into());
    }
    let url = url::Url::parse(&release.url).map_err(|e| e.to_string())?;
    if url.scheme() != "https"
        || !grandhonor_core::url_guard::is_trusted_update_host(url.host_str().unwrap_or(""))
        || !(url.host_str() == Some("github.com") && url.path().starts_with(RELEASE_DOWNLOAD_PATH))
    {
        return Err(t("Адрес загрузки не ведёт на выпуск GrandHonor на GitHub.", "The download address is not GrandHonor's GitHub release.", "下载地址不是 GrandHonor 的 GitHub 发布。").into());
    }
    let dir = staging_dir("update")?;
    let setup = dir.join(SETUP_ASSET);
    let status = download_to_file_with(&url, &setup, release.size, 120_000, validate_update_redirect)?;
    if status != 200 {
        return Err(format!("{} ({status})", t("Загрузка не удалась", "Download failed", "下载失败")));
    }
    let size = std::fs::metadata(&setup).map_err(|e| e.to_string())?.len();
    let hash = compute_sha256_hex(&setup)?;
    if size != release.size || !hash.eq_ignore_ascii_case(&release.sha256) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(t("Скачанный установщик не совпадает с контрольной суммой GitHub. Ничего не изменено.", "The downloaded setup does not match GitHub's checksum. Nothing was changed.", "下载的安装程序与 GitHub 校验和不符，未做任何更改。").into());
    }
    backup_current()?;
    crate::settings_transfer::refresh_backup();
    spawn_helper(&dir, &["--apply-update", &setup.to_string_lossy()])
}

/// Hands the rollback to the helper. On `Ok` the caller must quit the host.
pub fn start_rollback() -> Result<(), String> {
    if rollback_version().is_none() {
        return Err(grandhonor_core::locale::t("Прошлая версия не сохранена.", "No previous version is stored.", "没有保存的旧版本。").into());
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
    if !backup_trusted() {
        return Err(grandhonor_core::locale::t("Сохранённая прошлая версия лежит в незащищённой папке, восстанавливать её нельзя", "The stored previous version is not in a protected folder; not restoring it", "保存的旧版本不在受保护的文件夹中，无法恢复").into());
    }
    // A running executable (the browser may hold the panel as its settings link) cannot be
    // overwritten but can be renamed: move it aside first, then copy.
    for file in ["GrandHonor.Panel.exe", "GrandHonor.Video.exe"] {
        let path = app.join(file);
        if path.is_file() && std::fs::OpenOptions::new().write(true).open(&path).is_err() {
            let _ = std::fs::rename(&path, app.join(format!("{file}.old")));
        }
    }
    copy_tree(&dir, &app, true).map_err(|e| format!("{} {e}", grandhonor_core::locale::t("Не удалось восстановить", "Could not restore", "无法恢复")))
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
    let restore_processor_values = crate::hw::power::linked()
        && rollback_version().is_some_and(|version| {
            grandhonor_core::version::compare_versions(&version, "2.5.6") != std::cmp::Ordering::Greater
        });
    match restore_backup() {
        Ok(()) => {
            // Earlier versions only knew about their own processor profile selections.
            // Restore each scheme before the older host reapplies those selections.
            if restore_processor_values {
                crate::hw::cpu_profile::restore_original();
            }
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

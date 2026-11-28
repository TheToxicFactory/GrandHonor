//! Link between GrandHonor and its browser extension.
//!
//! One native messaging host, `com.grandhonor.vsr`, registered for Chrome and Edge under HKCU
//! with its manifest next to the executable: the panel executable sends the extension's options
//! from GrandHonor's settings (`BrowserVsr`). Video is processed inside the page (WebGPU); the
//! earlier Intel layer over the player is no longer shipped. The uninstaller removes the keys.

use std::path::PathBuf;

pub const HOST: &str = "com.grandhonor.vsr";
const ORIGIN: &str = "chrome-extension://hkhjfheinfpppebmejofemdgpimphgpd/";
const MANIFEST: &str = "GrandHonor.BrowserHost.json";
/// (host name, manifest file, executable, description)
const HOSTS: [(&str, &str, &str, &str); 1] = [
    (HOST, MANIFEST, "GrandHonor.Panel.exe", "GrandHonor settings for the browser extension"),
];

#[derive(Clone, Copy)]
pub enum Browser {
    Edge,
    Chrome,
}

impl Browser {
    pub const ALL: [Browser; 2] = [Browser::Edge, Browser::Chrome];

    pub fn name(self) -> &'static str {
        match self {
            Browser::Edge => "Microsoft Edge",
            Browser::Chrome => "Google Chrome",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Browser::Edge => "msedge.exe",
            Browser::Chrome => "chrome.exe",
        }
    }

    fn extensions_page(self) -> &'static str {
        match self {
            Browser::Edge => "edge://extensions",
            Browser::Chrome => "chrome://extensions",
        }
    }

    /// Path of the installed browser from App Paths, when it is installed.
    pub fn executable(self) -> Option<String> {
        let path = crate::settings::read_hklm_string(
            &format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{}", self.program()),
            "",
        )?;
        let path = path.trim_matches('"').to_string();
        std::path::Path::new(&path).is_file().then_some(path)
    }
}

fn program_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

/// Folder of the unpacked extension shipped with this build, when it has one.
pub fn extension_dir() -> Option<PathBuf> {
    let dir = program_dir()?.join("BrowserVsr");
    dir.join("manifest.json").is_file().then_some(dir)
}

/// The NPU restoration model is part of this build (it is left out where its licence forbids
/// redistribution).
pub fn npu_model_present() -> bool {
    extension_dir().is_some_and(|dir| dir.join("models").join("liveaction-webnn.bin").is_file())
}

pub fn available() -> bool {
    extension_dir().is_some()
}

/// Writes the native messaging manifest and registers it for both browser families. The
/// manifest goes through `safe_fs` (no links followed), as a portable folder may be user-writable.
fn register() -> Result<(), String> {
    let dir = program_dir().ok_or("no program folder")?;
    for (host, file, exe, description) in HOSTS {
        let program = dir.join(exe);
        if !program.is_file() {
            return Err(format!("{} is missing", program.display()));
        }
        let manifest = serde_json::json!({
            "name": host,
            "description": description,
            "path": program.display().to_string(),
            "type": "stdio",
            "allowed_origins": [ORIGIN],
        });
        let path = dir.join(file);
        crate::safe_fs::write_atomic(&path, &serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?)?;
        let value = path.display().to_string();
        for family in [r"Software\Google\Chrome", r"Software\Microsoft\Edge"] {
            crate::settings::create_hkcu_string(&format!(r"{family}\NativeMessagingHosts\{host}"), &value)
                .map_err(|e| format!("{family}: {e:?}"))?;
        }
    }
    Ok(())
}

/// Rewrites the manifest at start when the link was installed before, so a repair, update or
/// roll back that replaced the program folder does not leave the browsers pointing at nothing.
pub fn refresh_registration() {
    let missing = program_dir().is_some_and(|dir| HOSTS.iter().any(|(_, file, _, _)| !dir.join(file).is_file()));
    if available() && missing && registered() {
        if let Err(err) = register() {
            crate::log::error(&format!("Browser link not restored: {err}"));
        }
    }
}

fn registered() -> bool {
    crate::settings::read_hkcu_string(&format!(r"Software\Microsoft\Edge\NativeMessagingHosts\{HOST}"), "").is_some()
        || crate::settings::read_hkcu_string(&format!(r"Software\Google\Chrome\NativeMessagingHosts\{HOST}"), "").is_some()
}

/// Registers the link, opens the browser's extensions page and the extension folder.
/// Errors are for the user (through `t()`); the technical detail goes to the log.
pub fn install(browser: Browser) -> Result<(), String> {
    use grandhonor_core::locale::t;
    let folder = extension_dir().ok_or_else(|| t(
        "В этой сборке нет расширения для браузера",
        "This build has no browser extension",
        "此版本不含浏览器扩展",
    ).to_string())?;
    if let Err(err) = register() {
        crate::log::error(&format!("Browser link registration failed: {err}"));
        return Err(t(
            "Не удалось связать GrandHonor с браузером. Подробности в журнале",
            "Could not link GrandHonor with the browser. Details are in the log",
            "无法将 GrandHonor 与浏览器关联，详情见日志",
        ).to_string());
    }
    let exe = browser.executable().ok_or_else(|| format!(
        "{} {}", browser.name(), t("не установлен", "is not installed", "未安装"),
    ))?;
    crate::unelevated::open_with(&exe, browser.extensions_page());
    crate::unelevated::open(&folder.display().to_string());
    Ok(())
}
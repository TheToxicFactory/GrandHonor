//! Export and import of GrandHonor settings, and the automatic settings backup.
//!
//! Every user choice lives in `%APPDATA%\GrandHonor\settings.json` (`settings::store`). Updates
//! and repairs never touch that file, so nothing is lost on update. On top of that:
//! - System → Export settings writes them to a JSON file the user picks;
//! - System → Import settings reads such a file, stores the values and restarts GrandHonor,
//!   which applies everything (limits, backlight, touchpad, presets, language) at start-up;
//! - `%LOCALAPPDATA%\GrandHonor\settings-backup.json` is refreshed at every start and before
//!   every update, and is the default file offered by Import.

use crate::settings::{self, StoredValue};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

const FORMAT: &str = "GrandHonor settings";

pub fn backup_path() -> PathBuf {
    crate::log::log_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
        .join("settings-backup.json")
}

fn to_json() -> Value {
    let mut map = Map::new();
    for (name, value) in settings::all_user_settings() {
        let v = match value {
            StoredValue::Dword(n) => json!(n),
            StoredValue::Text(s) => json!(s),
        };
        map.insert(name, v);
    }
    json!({ "format": FORMAT, "version": env!("CARGO_PKG_VERSION"), "settings": map })
}

fn parse(text: &str) -> Result<Vec<(String, StoredValue)>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("Not a settings file: {e}"))?;
    if root["format"].as_str() != Some(FORMAT) {
        return Err("This file is not a GrandHonor settings export.".into());
    }
    let map = root["settings"].as_object().ok_or("The file has no settings.")?;
    let values = map
        .iter()
        .filter(|(name, _)| settings::is_user_setting(name))
        .filter_map(|(name, v)| match v {
            Value::Number(n) => n.as_u64().filter(|&n| n <= u32::MAX as u64).map(|n| StoredValue::Dword(n as u32)),
            Value::String(s) => Some(StoredValue::Text(s.clone())),
            _ => None,
        }.map(|v| (name.clone(), v)))
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err("The file contains no settings.".into());
    }
    Ok(values)
}

pub fn write_file(path: &Path) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&to_json()).map_err(|e| e.to_string())?;
    crate::safe_fs::write_atomic(path, text.as_bytes())
}

/// Refreshes the automatic backup (start-up, before updates). Errors are only logged.
pub fn refresh_backup() {
    if settings::all_user_settings().is_empty() {
        return; // never overwrite a good backup with an empty one
    }
    if let Err(err) = write_file(&backup_path()) {
        crate::log::error(&format!("Settings backup failed: {err}"));
    }
}

/// System → Export settings.
pub fn export_dialog() -> Result<bool, String> {
    let Some(path) = crate::drivers::transfer::save_file_dialog(
        grandhonor_core::locale::t("Экспорт настроек GrandHonor", "Export GrandHonor settings", "导出 GrandHonor 设置"),
        "GrandHonor-settings.json",
        "JSON",
        "*.json",
        "json",
    )?
    else {
        return Ok(false);
    };
    write_file(&path)?;
    Ok(true)
}

/// System → Import settings. On `Ok(true)` the caller restarts GrandHonor.
pub fn import_dialog() -> Result<bool, String> {
    let Some(path) = crate::drivers::transfer::open_file_dialog(
        grandhonor_core::locale::t("Импорт настроек GrandHonor", "Import GrandHonor settings", "导入 GrandHonor 设置"),
        "JSON",
        "*.json",
    )?
    else {
        return Ok(false);
    };
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let values = parse(&text)?;
    // Keep what was there before, in case the imported file was the wrong one.
    let _ = write_file(&backup_path().with_file_name("settings-before-import.json"));
    let written = settings::store_user_settings(&values);
    crate::log::info(&format!("Imported {written} settings from {}", path.display()));
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exports_and_rejects_foreign_files() {
        let ok = r#"{"format":"GrandHonor settings","version":"2.2.3","settings":
            {"Language":"ru","CpuLimitBalanced":30,"PerformanceModeActive":1,"Bad-Name":2}}"#;
        let values = parse(ok).unwrap();
        assert!(values.contains(&("Language".into(), StoredValue::Text("ru".into()))));
        assert!(values.contains(&("CpuLimitBalanced".into(), StoredValue::Dword(30))));
        assert!(!values.iter().any(|(n, _)| n == "PerformanceModeActive" || n == "Bad-Name"));
        assert!(parse(r#"{"format":"other","settings":{"A":1}}"#).is_err());
        assert!(parse("not json").is_err());
    }
}

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppLanguage {
    English,
    Russian,
    Chinese,
}

impl AppLanguage {
    pub const ALL: [AppLanguage; 3] = [AppLanguage::Russian, AppLanguage::English, AppLanguage::Chinese];

    pub fn code(self) -> &'static str {
        match self {
            AppLanguage::English => "en",
            AppLanguage::Russian => "ru",
            AppLanguage::Chinese => "zh",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "en" => Some(AppLanguage::English),
            "ru" => Some(AppLanguage::Russian),
            "zh" => Some(AppLanguage::Chinese),
            _ => None,
        }
    }

    /// Name of the language written in that language.
    pub fn native_name(self) -> &'static str {
        match self {
            AppLanguage::English => "English",
            AppLanguage::Russian => "Русский",
            AppLanguage::Chinese => "中文",
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            AppLanguage::Russian => 1,
            AppLanguage::English => 2,
            AppLanguage::Chinese => 3,
        }
    }

    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(AppLanguage::Russian),
            2 => Some(AppLanguage::English),
            3 => Some(AppLanguage::Chinese),
            _ => None,
        }
    }
}

#[cfg(windows)]
fn query_ui_language() -> AppLanguage {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }

    let lang_id = unsafe { GetUserDefaultUILanguage() };
    let primary = lang_id & 0x03FF;
    match primary {
        0x19 => AppLanguage::Russian,
        0x04 => AppLanguage::Chinese,
        _ => AppLanguage::English,
    }
}

#[cfg(not(windows))]
fn query_ui_language() -> AppLanguage {
    AppLanguage::English
}

/// 0 = follow Windows, otherwise the chosen language.
static OVERRIDE: AtomicU8 = AtomicU8::new(0);

/// The Windows display language, used when no language was chosen.
pub fn system_language() -> AppLanguage {
    static LANG: OnceLock<AppLanguage> = OnceLock::new();
    *LANG.get_or_init(query_ui_language)
}

/// Sets the language chosen by the user. `None` follows the Windows language.
pub fn set_language_override(language: Option<AppLanguage>) {
    OVERRIDE.store(language.map_or(0, AppLanguage::to_u8), Ordering::Release);
}

pub fn language_override() -> Option<AppLanguage> {
    AppLanguage::from_u8(OVERRIDE.load(Ordering::Acquire))
}

pub fn language() -> AppLanguage {
    language_override().unwrap_or_else(system_language)
}

pub fn t<'a>(ru: &'a str, en: &'a str, zh: &'a str) -> &'a str {
    match language() {
        AppLanguage::Russian => ru,
        AppLanguage::Chinese => {
            if zh.is_empty() {
                en
            } else {
                zh
            }
        }
        AppLanguage::English => en,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_switches_and_resets() {
        set_language_override(Some(AppLanguage::Chinese));
        assert_eq!(t("ru", "en", "zh"), "zh");
        set_language_override(Some(AppLanguage::Russian));
        assert_eq!(t("ru", "en", "zh"), "ru");
        set_language_override(None);
        assert_eq!(language(), system_language());
        for lang in AppLanguage::ALL {
            assert_eq!(AppLanguage::from_code(lang.code()), Some(lang));
        }
    }
}

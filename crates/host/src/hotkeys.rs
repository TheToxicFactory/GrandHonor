//! Keyboard → Keys: what the Copilot key and the HONOR YOYO key do. The Copilot key sends
//! Win+Shift+F23 (Explorer has it registered); the YOYO key sends a plain F11 (checked on
//! ZQC-P). With an action chosen, GrandHonor installs a low-level keyboard hook on its own
//! thread that looks only for those two: F23 while Win is held (the Win release is masked so
//! Start does not open) and F11 with no modifier. It swallows that key and asks the tray window
//! to run the action (tray.rs `run_key_action`). Every other key passes untouched and nothing is
//! recorded. With "Windows default" for both there is no hook at all.

use crate::settings;
use grandhonor_core::locale::t;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_CONTROL, VK_F11, VK_F23, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG, PostMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_QUIT,
    WM_SYSKEYDOWN,
};

/// Posted to the tray window: (re)read the key settings.
pub const WM_APP_KEYS: u32 = WM_APP + 6;
/// Posted to the tray window: a remapped key was pressed (wParam = key index).
pub const WM_APP_KEY_PRESSED: u32 = WM_APP + 7;

pub const COPILOT: &str = "Copilot";
pub const YOYO: &str = "Yoyo";
pub const KEYS: [&str; 2] = [COPILOT, YOYO];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Default,
    Panel,
    Screenshot,
    Microphone,
    PowerMode,
    EyeComfort,
    App,
}

impl Action {
    pub const ALL: [Action; 7] = [
        Action::Default,
        Action::Panel,
        Action::Screenshot,
        Action::Microphone,
        Action::PowerMode,
        Action::EyeComfort,
        Action::App,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Panel => "panel",
            Self::Screenshot => "screenshot",
            Self::Microphone => "mic",
            Self::PowerMode => "mode",
            Self::EyeComfort => "eye",
            Self::App => "app",
        }
    }

    pub fn parse(code: &str) -> Self {
        Self::ALL.into_iter().find(|a| a.code() == code).unwrap_or(Self::Default)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => t("Как в Windows", "Windows default", "Windows 默认"),
            Self::Panel => t("Открыть GrandHonor", "Open GrandHonor", "打开 GrandHonor"),
            Self::Screenshot => t("Снимок экрана", "Screenshot", "截图"),
            Self::Microphone => t("Микрофон вкл./выкл.", "Microphone on/off", "麦克风开关"),
            Self::PowerMode => t("Сменить режим питания", "Next power mode", "切换电源模式"),
            Self::EyeComfort => t("Защита зрения вкл./выкл.", "Eye comfort on/off", "护眼开关"),
            Self::App => t("Запустить программу", "Open a program", "打开程序"),
        }
    }
}

pub fn action(key: &str) -> Action {
    settings::key_action(key).map(|c| Action::parse(&c)).unwrap_or(Action::Default)
}

static TRAY: AtomicIsize = AtomicIsize::new(0);
/// Thread id of the hook thread, 0 when there is no hook.
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
static HOOK_LOCK: Mutex<()> = Mutex::new(());
static COPILOT_ON: AtomicBool = AtomicBool::new(false);
static YOYO_ON: AtomicBool = AtomicBool::new(false);
/// F11 is down: auto-repeat must not run the action again.
static YOYO_DOWN: AtomicBool = AtomicBool::new(false);
/// An unassigned key code, sent so a swallowed Win+key does not open Start.
const MASK_KEY: VIRTUAL_KEY = VIRTUAL_KEY(0xE8);

/// Applies the key settings; call on the tray window's thread at start and after a change.
pub fn register(hwnd: HWND) {
    TRAY.store(hwnd.0 as isize, Ordering::Release);
    let copilot = action(COPILOT) != Action::Default;
    let yoyo = action(YOYO) != Action::Default;
    COPILOT_ON.store(copilot, Ordering::Release);
    YOYO_ON.store(yoyo, Ordering::Release);
    let want = copilot || yoyo;
    let Ok(_g) = HOOK_LOCK.lock() else { return };
    let running = HOOK_THREAD.load(Ordering::Acquire);
    if want && running == 0 {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new().name("copilot-key".into()).stack_size(128 * 1024).spawn(move || hook_thread(tx));
        if let Ok(id) = rx.recv() {
            HOOK_THREAD.store(id, Ordering::Release);
        }
    } else if !want && running != 0 {
        HOOK_THREAD.store(0, Ordering::Release);
        unsafe {
            let _ = PostThreadMessageW(running, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn hook_thread(ready: std::sync::mpsc::Sender<u32>) {
    unsafe {
        let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) {
            Ok(h) => h,
            Err(err) => {
                crate::log::error(&format!("Copilot key: keyboard hook failed: {err}"));
                let _ = ready.send(0);
                return;
            }
        };
        let _ = ready.send(GetCurrentThreadId());
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = UnhookWindowsHookEx(hook);
    }
}

fn held(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(i32::from(vk.0)) as u16 & 0x8000) != 0 }
}

fn win_held() -> bool {
    held(VK_LWIN) || held(VK_RWIN)
}

fn tap(vk: VIRTUAL_KEY) {
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 },
        },
    };
    unsafe {
        SendInput(&[key(false), key(true)], size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam.0 != 0 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
        if COPILOT_ON.load(Ordering::Acquire) && info.vkCode == u32::from(VK_F23.0) && win_held() {
            if down {
                tap(MASK_KEY);
                pressed(COPILOT);
            }
            return LRESULT(1);
        }
        if YOYO_ON.load(Ordering::Acquire)
            && info.vkCode == u32::from(VK_F11.0)
            && !(held(VK_CONTROL) || held(VK_MENU) || held(VK_SHIFT) || win_held())
        {
            if down && !YOYO_DOWN.swap(true, Ordering::AcqRel) {
                pressed(YOYO);
            } else if !down {
                YOYO_DOWN.store(false, Ordering::Release);
            }
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(Some(HHOOK::default()), code, wparam, lparam) }
}

/// Asks the tray window to run the key's action (also used for the YOYO firmware event).
pub fn pressed(key: &str) {
    let hwnd = TRAY.load(Ordering::Acquire);
    let Some(index) = KEYS.iter().position(|k| *k == key) else { return };
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_KEY_PRESSED, WPARAM(index), LPARAM(0));
        }
    }
}

/// The panel changed a key: re-apply on the tray thread.
pub fn settings_changed() {
    let hwnd = TRAY.load(Ordering::Acquire);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_KEYS, WPARAM(0), LPARAM(0));
        }
    }
}

/// The program file name for the "Open a program" action, for the panel.
pub fn app_name(key: &str) -> Option<String> {
    let path = settings::key_app(key)?;
    std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::Action;

    #[test]
    fn codes_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::parse(a.code()), a);
        }
        assert_eq!(Action::parse("junk"), Action::Default);
    }
}

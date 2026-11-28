use crate::drivers::trust::compute_sha256_hex;
use crate::menu_builder::{ActionFn, HostContext, build_panel_snapshot};
use grandhonor_core::locale::t;
use grandhonor_core::protocol::{PANEL_TELEMETRY_COMMAND, PanelState};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows::Win32::UI::Shell::{NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW,
    SW_RESTORE, SetForegroundWindow, ShowWindow, WM_CLOSE,
};
use windows_core::BOOL;

/// A tray click first deactivates the open panel, which closes itself. A click arriving this
/// soon after that close is the same "toggle off" gesture and must not reopen the panel.
const REOPEN_GUARD_MS: u64 = 400;

pub struct PanelHost {
    ctx: HostContext,
    child: Arc<Mutex<Option<Child>>>,
    disposed: Arc<AtomicBool>,
    closed_at_ms: Arc<AtomicU64>,
}

impl PanelHost {
    pub fn new(ctx: HostContext) -> Self {
        Self {
            ctx,
            child: Arc::new(Mutex::new(None)),
            disposed: Arc::new(AtomicBool::new(false)),
            closed_at_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn toggle_or_open(&self, focus_if_open: bool) {
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut guard) = self.child.lock() else {
            return;
        };
        if let Some(existing) = guard.as_mut() {
            match existing.try_wait() {
                Ok(None) => {
                    let pid = existing.id();
                    if focus_if_open {
                        focus_process_window(pid);
                    } else {
                        close_process_window(pid);
                        let _ = existing.kill();
                    }
                    return;
                }
                _ => {
                    *guard = None;
                }
            }
        }
        drop(guard);

        let closed_at = self.closed_at_ms.load(Ordering::Acquire);
        if !focus_if_open
            && closed_at != 0
            && crate::hw::events::monotonic_ms().saturating_sub(closed_at) < REOPEN_GUARD_MS
        {
            return;
        }

        if let Err(err) = self.spawn_panel() {
            crate::log::error(&format!("Could not open GrandHonor panel: {err}"));
            crate::menu_builder::show_error_box(t(
                "Не удалось открыть панель. Проверьте файлы установки.",
                "Could not open the panel. Check the installation files.",
                "无法打开控制面板。请检查安装文件。",
            ));
        }
    }

    fn spawn_panel(&self) -> Result<(), String> {
        let exe_dir = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("Missing host directory")?
            .to_path_buf();
        let panel_path = exe_dir.join("GrandHonor.Panel.exe");
        if !panel_path.is_file() {
            return Err(format!("Missing {}", panel_path.to_string_lossy()));
        }

        let _panel_lock = if let Some(expected_hash) = option_env!("GRANDHONOR_PANEL_HASH")
            && !expected_hash.is_empty()
        {
            let lock = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ.0)
                .open(&panel_path)
                .map_err(|e| e.to_string())?;
            let actual = compute_sha256_hex(&panel_path)?;
            if !actual.eq_ignore_ascii_case(expected_hash) {
                return Err("GrandHonor.Panel.exe does not match this build.".to_string());
            }
            Some(lock)
        } else {
            None
        };

        let mut cmd = Command::new(&panel_path);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        cmd.env("GRANDHONOR_HOST_PID", std::process::id().to_string());
        if let Some([l, t, r, b]) = query_tray_icon_rect(self.ctx.tray_hwnd) {
            cmd.env("GRANDHONOR_TRAY_RECT", format!("{l},{t},{r},{b}"));
        }
        let mut proc = cmd
            .spawn()
            .map_err(|e| format!("Failed to launch GrandHonor Panel: {e}"))?;

        let stdin = proc.stdin.take().ok_or("Missing panel stdin")?;
        let stdout = proc.stdout.take().ok_or("Missing panel stdout")?;
        let pid = proc.id();
        // The tray click gave the host the foreground right; pass it on so the panel becomes
        // the active window. That also light-dismisses the tray overflow (^) flyout.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(pid);
        }

        if let Ok(mut guard) = self.child.lock() {
            *guard = Some(proc);
        }

        let ctx = self.ctx.clone();
        let child_ref = self.child.clone();
        let disposed = self.disposed.clone();
        let closed_at_ms = self.closed_at_ms.clone();

        thread::Builder::new()
            .name("panel-ipc".into())
            .spawn(move || {
                serve_ipc(ctx, stdin, stdout, disposed);
                closed_at_ms.store(crate::hw::events::monotonic_ms(), Ordering::Release);
                crate::footprint::trim_working_set();
                if let Ok(mut guard) = child_ref.lock()
                    && let Some(mut child) = guard.take()
                {
                    if child.id() == pid {
                        let _ = child.kill();
                        let _ = child.wait();
                    } else {
                        *guard = Some(child);
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub fn shutdown(&self) {
        self.disposed.store(true, Ordering::Release);
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            close_process_window(child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for PanelHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve_ipc(
    ctx: HostContext,
    mut stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    disposed: Arc<AtomicBool>,
) {
    // One COM apartment and one WMI connection for the whole time the panel is open.
    let _com = crate::hw::com::ComGuard::mta();
    let _live_session = crate::hw::sensors::keep_session_on_this_thread();
    // Build the first snapshot while the panel process is still starting GPUI, so its first
    // request is answered at once instead of after the sensor and system queries.
    // A fresh sensor sample first, so the first page shows live readings, not dashes that a
    // poll replaces half a second later.
    let telemetry = crate::hw::sensors::sample_live();
    let (mut snapshot, commands) = build_panel_snapshot(&ctx);
    snapshot.telemetry = telemetry;
    let mut prebuilt = Some((snapshot, commands));
    let mut reader = BufReader::new(stdout);
    let mut commands: HashMap<u32, ActionFn> = HashMap::new();
    let mut line = String::new();

    while !disposed.load(Ordering::Acquire) {
        line.clear();
        let Ok(n) = reader.read_line(&mut line) else {
            break;
        };
        if n == 0 {
            break;
        }
        let Ok(cmd_id) = line.trim().parse::<u32>() else {
            break;
        };

        let response = if cmd_id == PANEL_TELEMETRY_COMMAND {
            serde_json::to_vec(&crate::hw::sensors::sample_live())
        } else if cmd_id == 0
            && let Some((snapshot, new_commands)) = prebuilt.take()
        {
            commands = new_commands;
            serde_json::to_vec(&snapshot)
        } else {
            if let Some(action) = commands.get(&cmd_id) {
                // Messages from the action become a banner in the next snapshot.
                crate::menu_builder::run_panel_action(action);
            }
            if disposed.load(Ordering::Acquire) {
                break;
            }
            let (snapshot, new_commands): (PanelState, HashMap<u32, ActionFn>) =
                build_panel_snapshot(&ctx);
            commands = new_commands;
            serde_json::to_vec(&snapshot)
        };
        let Ok(mut json) = response else {
            break;
        };
        json.push(b'\n');
        if stdin.write_all(&json).is_err() || stdin.flush().is_err() {
            break;
        }
    }
}

fn query_tray_icon_rect(hwnd_raw: isize) -> Option<[i32; 4]> {
    if hwnd_raw == 0 {
        return None;
    }
    let id = NOTIFYICONIDENTIFIER {
        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: HWND(hwnd_raw as *mut std::ffi::c_void),
        uID: 1,
        ..Default::default()
    };
    let rc = unsafe { Shell_NotifyIconGetRect(&id) }.ok()?;
    if rc.right > rc.left && rc.bottom > rc.top {
        Some([rc.left, rc.top, rc.right, rc.bottom])
    } else {
        None
    }
}

fn find_process_hwnd(target_pid: u32) -> Option<HWND> {
    struct Search {
        pid: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        if pid == state.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            state.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search {
        pid: target_pid,
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

fn focus_process_window(pid: u32) {
    if let Some(hwnd) = find_process_hwnd(pid) {
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn close_process_window(pid: u32) {
    if let Some(hwnd) = find_process_hwnd(pid) {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default());
        }
    }
}

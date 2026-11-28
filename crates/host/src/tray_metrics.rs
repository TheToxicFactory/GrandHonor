use crate::hw::sensors;
use grandhonor_core::locale::t;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, mpsc};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, PostMessageW};

pub const LABELS: [(&str, &str, &str); 3] = [
    ("Температура процессора", "CPU temperature", "处理器温度"),
    ("Температура батареи", "Battery temperature", "电池温度"),
    ("Обороты вентиляторов", "Fan speed", "风扇转速"),
];

const FIRST_ID: u32 = 2;
static DISPLAY_ON: AtomicBool = AtomicBool::new(true);
static TOOLTIP: AtomicBool = AtomicBool::new(false);

pub fn set_display_on(on: bool) {
    DISPLAY_ON.store(on, Ordering::Release);
}

pub fn display_on() -> bool {
    DISPLAY_ON.load(Ordering::Acquire)
}

/// Coalesces panel-independent timer and hover requests on one WMI apartment.
pub fn request(hwnd: HWND, result_message: u32, tooltip: bool) {
    if !display_on() {
        return;
    }
    static WORK: OnceLock<Option<mpsc::SyncSender<()>>> = OnceLock::new();
    let worker = WORK.get_or_init(|| {
        let raw = hwnd.0 as isize;
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("tray-sensors".into()).stack_size(256 * 1024).spawn(move || {
            let _com = crate::hw::com::ComGuard::mta();
            let _session = sensors::keep_session_on_this_thread();
            while receive.recv().is_ok() {
                let tooltip = TOOLTIP.swap(false, Ordering::AcqRel);
                let mask = crate::settings::tray_metrics();
                if display_on() && (tooltip || mask != 0) {
                    if tooltip {
                        // The tooltip shows everything, the slow values included.
                        sensors::refresh_tray(true, true);
                    } else {
                        // Icons: 1 = CPU temperature, 2 = battery temperature, 4 = fans.
                        sensors::refresh_tray_icons(mask & 1 != 0, mask & 4 != 0, mask & 2 != 0);
                    }
                    unsafe {
                        let _ = PostMessageW(Some(HWND(raw as *mut _)), result_message, WPARAM(tooltip as usize), LPARAM(0));
                    }
                }
            }
        }).ok().map(|_| send)
    });
    if let Some(worker) = worker {
        if tooltip {
            TOOLTIP.store(true, Ordering::Release);
        }
        let _ = worker.try_send(());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct IconState {
    value: Option<i32>,
    dark: bool,
    size: i32,
}

#[derive(Default)]
pub struct Icons {
    shown: [Option<IconState>; 3],
    pub timer_active: bool,
}

impl Icons {
    pub fn is_empty(&self) -> bool {
        self.shown.iter().all(Option::is_none)
    }

    pub fn explorer_restarted(&mut self) {
        self.shown = [None; 3];
    }

    pub fn refresh(&mut self, hwnd: HWND, callback: u32, mask: u32, dark: bool, size: i32) {
        let readings = if display_on() { sensors::tray_readings() } else { [None; 3] };
        for (index, (slot, value)) in self.shown.iter_mut().zip(readings).enumerate() {
            let mut data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: FIRST_ID + index as u32,
                ..Default::default()
            };
            if mask & (1 << index) == 0 {
                if slot.take().is_some() {
                    unsafe { let _ = Shell_NotifyIconW(NIM_DELETE, &data); }
                }
                continue;
            }
            let value = if index == 2 { value.map(|rpm| (rpm + 50) / 100) } else { value };
            let next = IconState { value, dark, size };
            if *slot == Some(next) {
                continue;
            }
            let text = match next.value {
                Some(tenths) if index == 2 => format!("{}.{}", tenths / 10, tenths % 10),
                Some(degrees) => degrees.to_string(),
                None => "-".into(),
            };
            let icon = crate::tray::create_icon_from_bgra(size, &pixels(size, dark, &text));
            if icon.is_invalid() {
                continue;
            }
            let detail = match next.value {
                Some(tenths) if index == 2 => format!("{} {}", tenths * 100, t("об/мин", "rpm", "转/分")),
                Some(degrees) => format!("{degrees} °C"),
                None => t("Нет свежих данных", "No recent reading", "暂无最新读数").into(),
            };
            let (ru, en, zh) = LABELS[index];
            crate::tray::write_wide(&mut data.szTip, &format!("{} · {detail}", t(ru, en, zh)));
            data.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
            data.uCallbackMessage = callback;
            data.hIcon = icon;
            let ok = unsafe {
                let existing = slot.is_some() && Shell_NotifyIconW(NIM_MODIFY, &data).as_bool();
                let ok = existing || Shell_NotifyIconW(NIM_ADD, &data).as_bool() || Shell_NotifyIconW(NIM_MODIFY, &data).as_bool();
                let _ = DestroyIcon(icon);
                ok
            };
            if ok {
                *slot = Some(next);
            }
        }
    }
}

fn pixels(size: i32, dark: bool, text: &str) -> Vec<u32> {
    const DIGITS: [[u8; 5]; 10] = [
        [7, 5, 5, 5, 7], [2, 6, 2, 2, 7], [7, 1, 7, 4, 7], [7, 1, 7, 1, 7], [5, 5, 7, 1, 1],
        [7, 4, 7, 1, 7], [7, 4, 7, 5, 7], [7, 1, 1, 1, 1], [7, 5, 7, 5, 7], [7, 5, 7, 1, 7],
    ];
    let width = text.bytes().map(|c| if c == b'.' { 2 } else { 4 }).sum::<i32>() - 1;
    let scale = ((size - 2) / width).min((size - 2) / 5).max(1);
    let mut left = (size - width * scale) / 2;
    let top = (size - 5 * scale) / 2;
    let mut pixels = vec![0; (size * size) as usize];
    let color = if dark { 0xffd0d0d0 } else { 0xff353535 };
    for ch in text.bytes() {
        let (glyph, width) = match ch {
            b'0'..=b'9' => (DIGITS[(ch - b'0') as usize], 3),
            b'.' => ([0, 0, 0, 0, 1], 1),
            _ => ([0, 0, 7, 0, 0], 3),
        };
        for (row, bits) in glyph.into_iter().enumerate() {
            for column in 0..width {
                if bits & (1 << (width - column - 1)) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let x = left + column * scale + dx;
                            let y = top + row as i32 * scale + dy;
                            pixels[(y * size + x) as usize] = color;
                        }
                    }
                }
            }
        }
        left += (width + 1) * scale;
    }
    pixels
}

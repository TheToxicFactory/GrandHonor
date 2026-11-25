//! On-screen display: a rounded HUD slightly above the bottom centre of the screen for Fn keys
//! and firmware events (microphone, camera, keyboard light, touchpad, Fn lock, Windows key,
//! refresh rate, HONOR profile): an OLED-black plate with a lit rim and a soft shadow.
//!
//! Icons are the SVG files in `Assets/osd/` (24×24, 1.5 stroke, round caps), compiled in and
//! drawn as vectors with GDI+ anti-aliasing into a per-pixel-alpha layered window; labels use
//! the embedded Inter SemiBold. All of this runs in GrandHonor.exe on one thread that sleeps in
//! `GetMessageW` while nothing is shown: no GPU device, no extra process, timers only while a
//! notice is visible.

use crate::settings;
use grandhonor_core::shadow::blur_shadow;
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::{OnceLock, mpsc};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BI_RGB,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC,
    GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ReleaseDC, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, GetMessageW,
    HTTRANSPARENT, HWND_TOPMOST, KillTimer, MSG, PostMessageW, RegisterClassExW, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetTimer, SetWindowPos,
    ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WM_APP, WM_NCHITTEST, WM_TIMER,
    WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows_core::{HSTRING, PCWSTR};

const WM_OSD_SHOW: u32 = WM_APP + 40;
const TIMER_HOLD: usize = 1;
const HOLD_MS: u32 = 1_500;
/// Animations are timed in milliseconds and stepped on every DWM frame (`DwmFlush`), so they
/// run at the display rate: 120 steps per second on a 120 Hz panel, 60 on a 60 Hz one.
const FADE_IN_MS: f32 = 260.0;
const FADE_OUT_MS: f32 = 240.0;
const RISE: f32 = 14.0; // logical px the HUD rises while appearing

/// HUD geometry in logical pixels (scaled by the monitor DPI).
const BOX: f32 = 172.0;
const RADIUS: f32 = 36.0;
const ICON: f32 = 60.0;
const ICON_TOP: f32 = 30.0;
const LIFT: f32 = 96.0; // distance of the plate above the work-area bottom
const MARGIN: f32 = 24.0; // transparent room around the plate for its shadow

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Mic,
    MicOff,
    Camera,
    CameraOff,
    KeyboardLight,
    KeyboardOff,
    Touchpad,
    TouchpadOff,
    Lock,
    Unlock,
    WindowsKey,
    WindowsKeyOff,
    Display,
    Performance,
    Balanced,
    Normal,
    Warning,
    Charger,
    Battery,
}

fn svg(icon: Icon) -> &'static str {
    match icon {
        Icon::Mic => include_str!("../../../Assets/osd/mic.svg"),
        Icon::MicOff => include_str!("../../../Assets/osd/mic-off.svg"),
        Icon::Camera => include_str!("../../../Assets/osd/camera.svg"),
        Icon::CameraOff => include_str!("../../../Assets/osd/camera-off.svg"),
        Icon::KeyboardLight => include_str!("../../../Assets/osd/keyboard-light.svg"),
        Icon::KeyboardOff => include_str!("../../../Assets/osd/keyboard-off.svg"),
        Icon::Touchpad => include_str!("../../../Assets/osd/touchpad.svg"),
        Icon::TouchpadOff => include_str!("../../../Assets/osd/touchpad-off.svg"),
        Icon::Lock => include_str!("../../../Assets/osd/lock.svg"),
        Icon::Unlock => include_str!("../../../Assets/osd/unlock.svg"),
        Icon::WindowsKey => include_str!("../../../Assets/osd/windows-key.svg"),
        Icon::WindowsKeyOff => include_str!("../../../Assets/osd/windows-key-off.svg"),
        Icon::Display => include_str!("../../../Assets/osd/display.svg"),
        Icon::Performance => include_str!("../../../Assets/osd/performance.svg"),
        Icon::Balanced => include_str!("../../../Assets/osd/balanced.svg"),
        Icon::Normal => include_str!("../../../Assets/osd/normal.svg"),
        Icon::Warning => include_str!("../../../Assets/osd/warning.svg"),
        Icon::Charger => include_str!("../../../Assets/osd/charger.svg"),
        Icon::Battery => include_str!("../../../Assets/osd/battery.svg"),
    }
}

struct Notice {
    icon: Icon,
    text: String,
    /// Filled and total segments of the level bar (keyboard light), if any.
    level: Option<(u8, u8)>,
}

type Fallback = std::sync::Arc<dyn Fn(&str, &str) + Send + Sync>;
static FALLBACK: std::sync::OnceLock<Fallback> = std::sync::OnceLock::new();

/// The tray notification used by `notify` when on-screen notices are off (set by tray.rs).
pub fn set_fallback(f: Fallback) {
    let _ = FALLBACK.set(f);
}

/// A notice about something the user did not just do (a reminder, a battery event): on screen,
/// or as a Windows notification when on-screen notices are off, so it is never lost.
pub fn notify(icon: Icon, text: &str) {
    if settings::osd_enabled() {
        show(icon, text, None);
    } else if let Some(f) = FALLBACK.get() {
        f("GrandHonor", text);
    }
}

/// A short title on screen and the full text as a Windows notification (when notifications
/// are on): the 172 px plate cannot hold a whole error message.
pub fn notify_detail(icon: Icon, title: &str, body: &str) {
    if settings::osd_enabled() {
        show(icon, title, None);
    }
    if (settings::notifications_enabled() || !settings::osd_enabled())
        && let Some(f) = FALLBACK.get()
    {
        f(title, body);
    }
}

/// Shows a notice for about 1.5 s. Non-blocking; does nothing when on-screen notices are off
/// (used to confirm the user's own actions; background events use `notify`).
pub fn show(icon: Icon, text: &str, level: Option<(u8, u8)>) {
    if !settings::osd_enabled() {
        return;
    }
    let Some(hwnd) = window() else { return };
    let raw = Box::into_raw(Box::new(Notice { icon, text: text.to_string(), level }));
    unsafe {
        if PostMessageW(Some(hwnd), WM_OSD_SHOW, WPARAM(0), LPARAM(raw as isize)).is_err() {
            drop(Box::from_raw(raw));
        }
    }
}

fn window() -> Option<HWND> {
    static HWND_RAW: OnceLock<Option<isize>> = OnceLock::new();
    let raw = HWND_RAW.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("osd".into())
            .stack_size(256 * 1024)
            .spawn(move || run(tx))
            .ok()?;
        rx.recv().ok().flatten()
    });
    raw.map(|h| HWND(h as *mut c_void))
}

fn run(tx: mpsc::Sender<Option<isize>>) {
    unsafe {
        let mut token = 0usize;
        let input = GdiplusStartupInput { version: 1, callback: std::ptr::null_mut(), no_thread: 0, no_codecs: 1 };
        if GdiplusStartup(&mut token, &input, std::ptr::null_mut()) != 0 {
            let _ = tx.send(None);
            return;
        }
        let instance = GetModuleHandleW(None).map(Into::into).unwrap_or_default();
        let class = HSTRING::from("GrandHonorOsd");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            &class,
            &HSTRING::from("GrandHonor OSD"),
            WS_POPUP,
            0, 0, 1, 1,
            None, None, Some(instance), None,
        );
        let Ok(hwnd) = hwnd else {
            let _ = tx.send(None);
            return;
        };
        let _ = tx.send(Some(hwnd.0 as isize));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// The rendered frame kept while a notice is visible (the fade re-blends it).
struct Frame {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    pos: POINT,
    size: SIZE,
    alpha: u8,
    /// Device pixels of the rise animation.
    rise: i32,
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

thread_local! {
    static FRAME: RefCell<Option<Frame>> = const { RefCell::new(None) };
}

fn blend(hwnd: HWND, frame: &Frame) {
    blend_at(hwnd, frame, frame.pos);
}

fn blend_at(hwnd: HWND, frame: &Frame, pos: POINT) {
    let bf = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: frame.alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let src = POINT::default();
    unsafe {
        let screen = GetDC(None);
        let _ = UpdateLayeredWindow(
            hwnd, Some(screen), Some(&pos), Some(&frame.size), Some(frame.dc), Some(&src),
            COLORREF(0), Some(&bf), ULW_ALPHA,
        );
        ReleaseDC(None, screen);
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_OSD_SHOW => {
            let notice = unsafe { Box::from_raw(lparam.0 as *mut Notice) };
            let was_visible = FRAME.with(|slot| slot.borrow().is_some());
            if let Some(frame) = unsafe { render(&notice) } {
                FRAME.with(|slot| *slot.borrow_mut() = Some(frame));
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_HOLD);
                    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
                if was_visible {
                    // Replacing a visible notice: swap the content in place, no second entrance.
                    FRAME.with(|slot| {
                        if let Some(frame) = slot.borrow().as_ref() {
                            blend(hwnd, frame);
                        }
                    });
                } else {
                    animate(hwnd, true);
                }
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    SetTimer(Some(hwnd), TIMER_HOLD, HOLD_MS, None);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_HOLD => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_HOLD);
            }
            if animate(hwnd, false) {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                // Free the bitmap: nothing is kept while the HUD is hidden.
                FRAME.with(|slot| slot.borrow_mut().take());
            }
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmFlush() -> i32;
}

/// Fades the HUD in (rising on a light spring) or out (sinking slightly), one step per DWM
/// frame. A fade-out stops early and returns false when a new notice arrives, so the next one
/// appears without waiting.
fn animate(hwnd: HWND, appear: bool) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{PM_NOREMOVE, PeekMessageW};
    let total = if appear { FADE_IN_MS } else { FADE_OUT_MS };
    let start = std::time::Instant::now();
    loop {
        let t = (start.elapsed().as_secs_f32() * 1000.0 / total).min(1.0);
        let fade = 1.0 - (1.0 - t).powi(3);
        let (level, travel) = if appear {
            // Back-out spring: a ≈4 % overshoot above the resting place, then settle.
            let x = t - 1.0;
            let spring = 1.0 + 2.2 * x * x * x + 1.2 * x * x;
            (fade, 1.0 - spring)
        } else {
            (1.0 - fade, -0.35 * fade)
        };
        let stop = FRAME.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(frame) = slot.as_mut() else { return true };
            frame.alpha = (level.clamp(0.0, 1.0) * 255.0).round() as u8;
            let offset = (travel * frame.rise as f32).round() as i32;
            blend_at(hwnd, frame, POINT { x: frame.pos.x, y: frame.pos.y + offset });
            false
        });
        if stop || t >= 1.0 {
            return true;
        }
        if !appear {
            let mut msg = MSG::default();
            if unsafe { PeekMessageW(&mut msg, Some(hwnd), WM_OSD_SHOW, WM_OSD_SHOW, PM_NOREMOVE) }.as_bool() {
                return false;
            }
        }
        if unsafe { DwmFlush() } < 0 {
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }
}

#[link(name = "shcore")]
unsafe extern "system" {
    fn GetDpiForMonitor(monitor: isize, kind: u32, x: *mut u32, y: *mut u32) -> i32;
}

/// Inter SemiBold for labels, loaded once from the copy compiled into GrandHonor.exe.
/// Chinese labels use Microsoft YaHei UI (Inter has no CJK glyphs).
fn label_family(chinese: bool) -> *mut c_void {
    thread_local! {
        static FAMILIES: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0, 0]) };
    }
    FAMILIES.with(|slot| {
        let mut cached = slot.get();
        let ix = usize::from(chinese);
        if cached[ix] != 0 {
            return cached[ix] as *mut c_void;
        }
        static INTER: &[u8] = include_bytes!("../../../Assets/fonts/Inter-SemiBold.ttf");
        let mut family: *mut c_void = std::ptr::null_mut();
        unsafe {
            if !chinese {
                let mut collection: *mut c_void = std::ptr::null_mut();
                if GdipNewPrivateFontCollection(&mut collection) == 0
                    && GdipPrivateAddMemoryFont(collection, INTER.as_ptr() as *const c_void, INTER.len() as i32) == 0
                {
                    // GDI+ names a static weight by its legacy family name.
                    for name in ["Inter SemiBold", "Inter"] {
                        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
                        if GdipCreateFontFamilyFromName(wide.as_ptr(), collection, &mut family) == 0 {
                            break;
                        }
                        family = std::ptr::null_mut();
                    }
                }
            }
            if family.is_null() {
                for name in ["Microsoft YaHei UI", "Segoe UI Semibold", "Segoe UI"] {
                    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
                    if GdipCreateFontFamilyFromName(wide.as_ptr(), std::ptr::null_mut(), &mut family) == 0 {
                        break;
                    }
                    family = std::ptr::null_mut();
                }
            }
        }
        // Kept for the life of the OSD thread (one collection and family per script).
        cached[ix] = family as usize;
        slot.set(cached);
        family
    })
}

/// Draws the HUD into a premultiplied 32-bit bitmap positioned on the active monitor.
/// An OLED-black plate with white ink, a lit rim and a soft drop shadow.
unsafe fn render(notice: &Notice) -> Option<Frame> {
    unsafe {
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor.0 as isize, 0, &mut dx, &mut dy);
        let s = dx.max(96) as f32 / 96.0;
        let side = (BOX * s).round() as i32;
        let margin = (MARGIN * s).round() as i32;
        let full = side + 2 * margin;
        let work = info.rcWork;
        let plate_x = work.left + (work.right - work.left - side) / 2;
        let plate_y = work.bottom - side - (LIFT * s).round() as i32;
        let pos = POINT { x: plate_x - margin, y: plate_y - margin };

        let ink: u32 = 0xFF_FF_FF;

        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: full,
            biHeight: -full,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let bmi = BITMAPINFO { bmiHeader: header, ..Default::default() };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, HGDIOBJ(bitmap.0));
        let len = (full * full * 4) as usize;
        std::ptr::write_bytes(bits as *mut u8, 0, len);

        let mut image: *mut c_void = std::ptr::null_mut();
        GdipCreateBitmapFromScan0(full, full, full * 4, PIXEL_FORMAT_32BPP_PARGB, bits, &mut image);
        let mut g: *mut c_void = std::ptr::null_mut();
        GdipGetImageGraphicsContext(image, &mut g);
        GdipSetSmoothingMode(g, SMOOTHING_ANTIALIAS);
        GdipSetPixelOffsetMode(g, PIXEL_OFFSET_HIGH_QUALITY);
        GdipSetInterpolationMode(g, INTERPOLATION_HIGH_QUALITY_BILINEAR);
        GdipSetTextRenderingHint(g, TEXT_ANTIALIAS);

        let f = side as f32;
        let m = margin as f32;
        let radius = RADIUS * s;

        // 1. Drop shadow: the plate shape, lower by 8 px, blurred on the CPU.
        let shadow = rounded_rect(m, m + 8.0 * s, f, f, radius);
        let mut shadow_fill: *mut c_void = std::ptr::null_mut();
        GdipCreateSolidFill(0x73_00_00_00, &mut shadow_fill);
        GdipFillPath(g, shadow_fill, shadow);
        GdipDeleteBrush(shadow_fill);
        GdipDeletePath(shadow);
        GdipFlush(g, 1);
        blur_shadow(std::slice::from_raw_parts_mut(bits as *mut u8, len), full as usize, full as usize, (6.0 * s).round() as usize);

        // 2. The plate.
        let plate = rounded_rect(m + 0.5, m + 0.5, f - 1.0, f - 1.0, radius);
        let mut fill: *mut c_void = std::ptr::null_mut();
        GdipCreateSolidFill(0xFF_00_00_00, &mut fill);
        GdipFillPath(g, fill, plate);
        GdipDeleteBrush(fill);

        // 3. Lit rim: bright along the top edge, fading towards the bottom.
        let (rim_top, rim_bottom) = (0x47_FF_FF_FF_u32, 0x14_FF_FF_FF_u32);
        let mut rim_brush: *mut c_void = std::ptr::null_mut();
        let top = PointF { x: 0.0, y: m };
        let bottom = PointF { x: 0.0, y: m + f };
        GdipCreateLineBrush(&top, &bottom, rim_top, rim_bottom, WRAP_TILE_FLIP_XY, &mut rim_brush);
        let mut rim: *mut c_void = std::ptr::null_mut();
        GdipCreatePen2(rim_brush, s.max(1.0), UNIT_PIXEL, &mut rim);
        GdipDrawPath(g, rim, plate);
        GdipDeletePen(rim);
        GdipDeleteBrush(rim_brush);
        GdipDeletePath(plate);

        // 4. The icon, scaled from its 24×24 grid.
        let k = ICON * s / 24.0;
        let ox = m + (f - ICON * s) / 2.0;
        let oy = m + ICON_TOP * s;
        let mut pen: *mut c_void = std::ptr::null_mut();
        GdipCreatePen1(0xFF_00_00_00 | ink, 1.5 * k, UNIT_PIXEL, &mut pen);
        GdipSetPenStartCap(pen, LINE_CAP_ROUND);
        GdipSetPenEndCap(pen, LINE_CAP_ROUND);
        GdipSetPenLineJoin(pen, LINE_JOIN_ROUND);
        for d in path_data(svg(notice.icon)) {
            let path = build_path(&parse_path(d), k, ox, oy);
            GdipDrawPath(g, pen, path);
            GdipDeletePath(path);
        }
        GdipDeletePen(pen);

        // 5. Label: Inter SemiBold, centred, up to two lines.
        let chinese = grandhonor_core::locale::language() == grandhonor_core::locale::AppLanguage::Chinese;
        let family = label_family(chinese);
        let mut font: *mut c_void = std::ptr::null_mut();
        GdipCreateFont(family, 14.0 * s, 0, UNIT_PIXEL, &mut font);
        // Typographic format: natural letter spacing (the default one pads every glyph).
        let mut typographic: *mut c_void = std::ptr::null_mut();
        GdipStringFormatGetGenericTypographic(&mut typographic);
        let mut format: *mut c_void = std::ptr::null_mut();
        GdipCloneStringFormat(typographic, &mut format);
        GdipSetStringFormatAlign(format, STRING_ALIGN_CENTER);
        GdipSetStringFormatLineAlign(format, STRING_ALIGN_CENTER);
        let mut text_brush: *mut c_void = std::ptr::null_mut();
        GdipCreateSolidFill(0xF0_00_00_00 | ink, &mut text_brush);
        let label_top = oy + ICON * s + 12.0 * s;
        let label_height = 38.0 * s;
        let rect = RectF { x: m + 14.0 * s, y: label_top, w: f - 28.0 * s, h: label_height };
        let text: Vec<u16> = notice.text.encode_utf16().collect();
        GdipDrawString(g, text.as_ptr(), text.len() as i32, font, &rect, format, text_brush);
        GdipDeleteBrush(text_brush);
        GdipDeleteStringFormat(format);
        GdipDeleteFont(font);

        // 6. Level bar (keyboard light, haptics): capsule segments, filled ones in full ink.
        if let Some((filled, total)) = notice.level.filter(|(_, t)| *t > 0) {
            let seg_w = 24.0 * s;
            let gap = 5.0 * s;
            let seg_h = 5.0 * s;
            let width = total as f32 * seg_w + (total as f32 - 1.0) * gap;
            let y = label_top + label_height + 2.0 * s;
            for i in 0..total {
                let x = m + (f - width) / 2.0 + i as f32 * (seg_w + gap);
                let color = if i < filled { 0xF2_00_00_00 | ink } else { 0x3D_00_00_00 | ink };
                let mut brush: *mut c_void = std::ptr::null_mut();
                GdipCreateSolidFill(color, &mut brush);
                let bar = rounded_rect(x, y, seg_w, seg_h, seg_h / 2.0);
                GdipFillPath(g, brush, bar);
                GdipDeletePath(bar);
                GdipDeleteBrush(brush);
            }
        }

        GdipDeleteGraphics(g);
        GdipDisposeImage(image);
        Some(Frame { dc, bitmap, old, pos, size: SIZE { cx: full, cy: full }, alpha: 255, rise: (RISE * s).round() as i32 })
    }
}

unsafe fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> *mut c_void {
    let mut path: *mut c_void = std::ptr::null_mut();
    let d = (r * 2.0).min(w).min(h);
    unsafe {
        GdipCreatePath(0, &mut path);
        GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
        GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
        GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
        GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
        GdipClosePathFigure(path);
    }
    path
}

// ---- SVG path data -------------------------------------------------------------------------

/// The `d` attributes of every `<path>` in an SVG file.
fn path_data(svg: &str) -> impl Iterator<Item = &str> {
    svg.split(" d=\"").skip(1).filter_map(|rest| rest.split('"').next())
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Parses SVG path data (M L H V C S A Z, absolute and relative) into moves, lines and cubics.
fn parse_path(d: &str) -> Vec<Seg> {
    let mut out = Vec::new();
    let bytes = d.as_bytes();
    let mut i = 0;
    let mut cmd = b'M';
    let (mut x, mut y, mut sx, mut sy) = (0f32, 0f32, 0f32, 0f32);
    let mut last_c2: Option<(f32, f32)> = None;

    let skip = |i: &mut usize| {
        while *i < bytes.len() && (bytes[*i] == b' ' || bytes[*i] == b',' || bytes[*i] == b'\n') {
            *i += 1;
        }
    };
    let number = |i: &mut usize| -> Option<f32> {
        skip(i);
        let start = *i;
        if *i < bytes.len() && (bytes[*i] == b'-' || bytes[*i] == b'+') {
            *i += 1;
        }
        let mut dot = false;
        while *i < bytes.len() && (bytes[*i].is_ascii_digit() || (bytes[*i] == b'.' && !dot)) {
            dot |= bytes[*i] == b'.';
            *i += 1;
        }
        d.get(start..*i)?.parse().ok()
    };
    let flag = |i: &mut usize| -> Option<bool> {
        skip(i);
        let v = *bytes.get(*i)?;
        *i += 1;
        Some(v == b'1')
    };

    loop {
        skip(&mut i);
        if i >= bytes.len() {
            break;
        }
        if bytes[i].is_ascii_alphabetic() {
            cmd = bytes[i];
            i += 1;
            if cmd == b'z' || cmd == b'Z' {
                out.push(Seg::Close);
                x = sx;
                y = sy;
                last_c2 = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (bx, by) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let (Some(nx), Some(ny)) = (number(&mut i), number(&mut i)) else { break };
                x = bx + nx;
                y = by + ny;
                sx = x;
                sy = y;
                out.push(Seg::Move(x, y));
                cmd = if rel { b'l' } else { b'L' };
                last_c2 = None;
            }
            b'L' => {
                let (Some(nx), Some(ny)) = (number(&mut i), number(&mut i)) else { break };
                x = bx + nx;
                y = by + ny;
                out.push(Seg::Line(x, y));
                last_c2 = None;
            }
            b'H' => {
                let Some(nx) = number(&mut i) else { break };
                x = bx + nx;
                out.push(Seg::Line(x, y));
                last_c2 = None;
            }
            b'V' => {
                let Some(ny) = number(&mut i) else { break };
                y = by + ny;
                out.push(Seg::Line(x, y));
                last_c2 = None;
            }
            b'C' => {
                let v: Option<Vec<f32>> = (0..6).map(|_| number(&mut i)).collect();
                let Some(v) = v else { break };
                let seg = Seg::Cubic(bx + v[0], by + v[1], bx + v[2], by + v[3], bx + v[4], by + v[5]);
                last_c2 = Some((bx + v[2], by + v[3]));
                x = bx + v[4];
                y = by + v[5];
                out.push(seg);
            }
            b'S' => {
                let v: Option<Vec<f32>> = (0..4).map(|_| number(&mut i)).collect();
                let Some(v) = v else { break };
                let (c1x, c1y) = last_c2.map_or((x, y), |(cx, cy)| (2.0 * x - cx, 2.0 * y - cy));
                out.push(Seg::Cubic(c1x, c1y, bx + v[0], by + v[1], bx + v[2], by + v[3]));
                last_c2 = Some((bx + v[0], by + v[1]));
                x = bx + v[2];
                y = by + v[3];
            }
            b'A' => {
                let (Some(rx), Some(ry), Some(rot)) = (number(&mut i), number(&mut i), number(&mut i)) else { break };
                let (Some(large), Some(sweep)) = (flag(&mut i), flag(&mut i)) else { break };
                let (Some(nx), Some(ny)) = (number(&mut i), number(&mut i)) else { break };
                let (ex, ey) = (bx + nx, by + ny);
                arc_to_cubics(&mut out, (x, y), (rx, ry), rot, large, sweep, (ex, ey));
                x = ex;
                y = ey;
                last_c2 = None;
            }
            _ => break,
        }
    }
    out
}

/// SVG elliptical arc → cubic Béziers (SVG 1.1 implementation notes, F.6).
fn arc_to_cubics(out: &mut Vec<Seg>, p1: (f32, f32), r: (f32, f32), rot_deg: f32, large: bool, sweep: bool, p2: (f32, f32)) {
    let (mut rx, mut ry) = (r.0.abs(), r.1.abs());
    if rx == 0.0 || ry == 0.0 || p1 == p2 {
        out.push(Seg::Line(p2.0, p2.1));
        return;
    }
    let phi = rot_deg.to_radians();
    let (sin, cos) = phi.sin_cos();
    let dx = (p1.0 - p2.0) / 2.0;
    let dy = (p1.1 - p2.1) / 2.0;
    let x1 = cos * dx + sin * dy;
    let y1 = -sin * dx + cos * dy;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let sign = if large == sweep { -1.0 } else { 1.0 };
    let coef = sign * (num / den).max(0.0).sqrt();
    let cx1 = coef * rx * y1 / ry;
    let cy1 = -coef * ry * x1 / rx;
    let cx = cos * cx1 - sin * cy1 + (p1.0 + p2.0) / 2.0;
    let cy = sin * cx1 + cos * cy1 + (p1.1 + p2.1) / 2.0;
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let theta1 = angle(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut delta = angle((x1 - cx1) / rx, (y1 - cy1) / ry, (-x1 - cx1) / rx, (-y1 - cy1) / ry);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    let n = (delta.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let step = delta / n as f32;
    let t = 4.0 / 3.0 * (step / 4.0).tan();
    let map = |ux: f32, uy: f32| (cx + rx * cos * ux - ry * sin * uy, cy + rx * sin * ux + ry * cos * uy);
    for k in 0..n {
        let a1 = theta1 + step * k as f32;
        let a2 = a1 + step;
        let (s1, c1) = a1.sin_cos();
        let (s2, c2) = a2.sin_cos();
        let q1 = map(c1 - t * s1, s1 + t * c1);
        let q2 = map(c2 + t * s2, s2 - t * c2);
        let end = map(c2, s2);
        out.push(Seg::Cubic(q1.0, q1.1, q2.0, q2.1, end.0, end.1));
    }
}

unsafe fn build_path(segs: &[Seg], k: f32, ox: f32, oy: f32) -> *mut c_void {
    let mut path: *mut c_void = std::ptr::null_mut();
    let tx = |v: f32| ox + v * k;
    let ty = |v: f32| oy + v * k;
    unsafe {
        GdipCreatePath(0, &mut path);
        let (mut cx, mut cy) = (0f32, 0f32);
        for seg in segs {
            match *seg {
                Seg::Move(x, y) => {
                    GdipStartPathFigure(path);
                    cx = x;
                    cy = y;
                }
                Seg::Line(x, y) => {
                    GdipAddPathLine(path, tx(cx), ty(cy), tx(x), ty(y));
                    cx = x;
                    cy = y;
                }
                Seg::Cubic(a, b, c, d, x, y) => {
                    GdipAddPathBezier(path, tx(cx), ty(cy), tx(a), ty(b), tx(c), ty(d), tx(x), ty(y));
                    cx = x;
                    cy = y;
                }
                Seg::Close => {
                    GdipClosePathFigure(path);
                }
            }
        }
    }
    path
}

// ---- GDI+ flat API (gdiplus.dll ships with every Windows) -----------------------------------

#[repr(C)]
struct GdiplusStartupInput {
    version: u32,
    callback: *mut c_void,
    no_thread: i32,
    no_codecs: i32,
}

#[repr(C)]
struct PointF {
    x: f32,
    y: f32,
}

#[repr(C)]
struct RectF {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;
const SMOOTHING_ANTIALIAS: i32 = 4;
const PIXEL_OFFSET_HIGH_QUALITY: i32 = 2;
const TEXT_ANTIALIAS: i32 = 4;
const INTERPOLATION_HIGH_QUALITY_BILINEAR: i32 = 6;
const WRAP_TILE_FLIP_XY: i32 = 3;
const UNIT_PIXEL: i32 = 2;
const LINE_CAP_ROUND: i32 = 2;
const LINE_JOIN_ROUND: i32 = 2;
const STRING_ALIGN_CENTER: i32 = 1;

#[link(name = "gdiplus")]
unsafe extern "system" {
    fn GdiplusStartup(token: *mut usize, input: *const GdiplusStartupInput, output: *mut c_void) -> i32;
    fn GdipCreateBitmapFromScan0(w: i32, h: i32, stride: i32, format: i32, scan0: *mut c_void, bitmap: *mut *mut c_void) -> i32;
    fn GdipGetImageGraphicsContext(image: *mut c_void, graphics: *mut *mut c_void) -> i32;
    fn GdipSetSmoothingMode(g: *mut c_void, mode: i32) -> i32;
    fn GdipSetPixelOffsetMode(g: *mut c_void, mode: i32) -> i32;
    fn GdipSetTextRenderingHint(g: *mut c_void, hint: i32) -> i32;
    fn GdipCreatePath(fill_mode: i32, path: *mut *mut c_void) -> i32;
    fn GdipStartPathFigure(path: *mut c_void) -> i32;
    fn GdipClosePathFigure(path: *mut c_void) -> i32;
    fn GdipAddPathLine(path: *mut c_void, x1: f32, y1: f32, x2: f32, y2: f32) -> i32;
    #[allow(clippy::too_many_arguments)]
    fn GdipAddPathBezier(path: *mut c_void, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32, x4: f32, y4: f32) -> i32;
    fn GdipAddPathArc(path: *mut c_void, x: f32, y: f32, w: f32, h: f32, start: f32, sweep: f32) -> i32;
    fn GdipDeletePath(path: *mut c_void) -> i32;
    fn GdipCreateSolidFill(argb: u32, brush: *mut *mut c_void) -> i32;
    fn GdipDeleteBrush(brush: *mut c_void) -> i32;
    fn GdipFillPath(g: *mut c_void, brush: *mut c_void, path: *mut c_void) -> i32;
    fn GdipCreatePen1(argb: u32, width: f32, unit: i32, pen: *mut *mut c_void) -> i32;
    fn GdipSetPenStartCap(pen: *mut c_void, cap: i32) -> i32;
    fn GdipSetPenEndCap(pen: *mut c_void, cap: i32) -> i32;
    fn GdipSetPenLineJoin(pen: *mut c_void, join: i32) -> i32;
    fn GdipDeletePen(pen: *mut c_void) -> i32;
    fn GdipDrawPath(g: *mut c_void, pen: *mut c_void, path: *mut c_void) -> i32;
    fn GdipCreateFontFamilyFromName(name: *const u16, collection: *mut c_void, family: *mut *mut c_void) -> i32;
    fn GdipCreateFont(family: *mut c_void, size: f32, style: i32, unit: i32, font: *mut *mut c_void) -> i32;
    fn GdipDeleteFont(font: *mut c_void) -> i32;
    fn GdipStringFormatGetGenericTypographic(format: *mut *mut c_void) -> i32;
    fn GdipCloneStringFormat(format: *mut c_void, clone: *mut *mut c_void) -> i32;
    fn GdipSetStringFormatAlign(format: *mut c_void, align: i32) -> i32;
    fn GdipSetStringFormatLineAlign(format: *mut c_void, align: i32) -> i32;
    fn GdipDeleteStringFormat(format: *mut c_void) -> i32;
    fn GdipDrawString(g: *mut c_void, text: *const u16, len: i32, font: *mut c_void, rect: *const RectF, format: *mut c_void, brush: *mut c_void) -> i32;
    fn GdipDeleteGraphics(g: *mut c_void) -> i32;
    fn GdipFlush(g: *mut c_void, intention: i32) -> i32;
    fn GdipSetInterpolationMode(g: *mut c_void, mode: i32) -> i32;
    fn GdipCreateLineBrush(p1: *const PointF, p2: *const PointF, c1: u32, c2: u32, wrap: i32, brush: *mut *mut c_void) -> i32;
    fn GdipCreatePen2(brush: *mut c_void, width: f32, unit: i32, pen: *mut *mut c_void) -> i32;
    fn GdipNewPrivateFontCollection(collection: *mut *mut c_void) -> i32;
    fn GdipPrivateAddMemoryFont(collection: *mut c_void, memory: *const c_void, length: i32) -> i32;
    fn GdipDisposeImage(image: *mut c_void) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_icon() {
        for icon in [Icon::Mic, Icon::MicOff, Icon::Camera, Icon::CameraOff, Icon::KeyboardLight, Icon::KeyboardOff,
            Icon::Touchpad, Icon::TouchpadOff, Icon::Lock, Icon::Unlock, Icon::WindowsKey, Icon::WindowsKeyOff,
            Icon::Display, Icon::Performance, Icon::Balanced, Icon::Normal, Icon::Warning, Icon::Charger, Icon::Battery]
        {
            let paths: Vec<_> = path_data(svg(icon)).collect();
            assert!(!paths.is_empty(), "{icon:?}");
            for d in paths {
                let segs = parse_path(d);
                assert!(matches!(segs.first(), Some(Seg::Move(..))), "{icon:?}: {d}");
                assert!(segs.len() >= 2, "{icon:?}: {d}");
            }
        }
    }

    #[test]
    fn arc_ends_at_target() {
        let segs = parse_path("M12 3a3 3 0 0 0-3 3");
        let Some(Seg::Cubic(.., x, y)) = segs.last().copied() else { panic!() };
        assert!((x - 9.0).abs() < 1e-3 && (y - 6.0).abs() < 1e-3);
    }
}

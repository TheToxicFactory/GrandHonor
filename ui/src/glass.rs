//! Glass backdrop for System → Style → Glass.
//!
//! GPUI's DirectComposition surface does not let the Windows acrylic material show through on
//! this window, so the panel makes its own frosted glass, as macOS does for sheets: it hides
//! itself for one composition frame (DWM cloak), copies the desktop under its rectangle with GDI,
//! then shrinks, blurs and slightly saturates that copy on the CPU. The result is a small
//! BGRA image (one eighth of the window side) that the GPU stretches smoothly behind the content.
//! It is taken once when Glass is first shown and freed with the panel.

use grandhonor_core::frost::frost;
use std::ffi::c_void;

/// Downscale factor of the copy before blurring. The stretched result is a soft, even blur.
const SHRINK: i32 = 8;
/// Box-blur radius at the reduced size; three passes approximate a Gaussian.
const RADIUS: i32 = 3;
/// Vibrancy: colour behind glass reads a little richer, as on iOS and macOS.
const SATURATION: f32 = 1.45;

pub struct Backdrop {
    pub width: u32,
    pub height: u32,
    /// Top-down BGRA, opaque.
    pub bgra: Vec<u8>,
}

/// Copies and frosts the desktop behind `hwnd`. Returns `None` if any GDI step fails.
pub fn capture(hwnd: isize) -> Option<Backdrop> {
    const DWMWA_CLOAK: u32 = 13;
    const SRCCOPY: u32 = 0x00CC_0020;
    let mut rect = [0i32; 4];
    if unsafe { GetWindowRect(hwnd, rect.as_mut_ptr()) } == 0 {
        return None;
    }
    let (x, y) = (rect[0], rect[1]);
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    if w <= 0 || h <= 0 {
        return None;
    }

    // Hide the panel for the copy. GDI screen copies never include this DirectComposition
    // window reliably either way, but cloaking makes sure the copy shows only the desktop.
    let on: u32 = 1;
    let off: u32 = 0;
    let pixels = unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &on, 4);
        let _ = DwmFlush();
        let _ = DwmFlush();
        let screen = GetDC(0);
        let memory = CreateCompatibleDC(screen);
        let info = BitmapInfoHeader {
            size: size_of::<BitmapInfoHeader>() as u32,
            width: w,
            height: -h,
            planes: 1,
            bit_count: 32,
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(memory, &info, 0, &mut bits, 0, 0);
        let result = if bitmap != 0 && !bits.is_null() {
            let old = SelectObject(memory, bitmap);
            let copied = BitBlt(memory, 0, 0, w, h, screen, x, y, SRCCOPY) != 0;
            let data = copied.then(|| std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec());
            SelectObject(memory, old);
            DeleteObject(bitmap);
            data
        } else {
            None
        };
        DeleteDC(memory);
        ReleaseDC(0, screen);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &off, 4);
        result
    }?;

    let f = frost(&pixels, w as usize, h as usize, SHRINK as usize, RADIUS as usize, SATURATION);
    Some(Backdrop { width: f.width, height: f.height, bgra: f.bgra })
}

#[repr(C)]
#[derive(Default)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_ppm: i32,
    y_ppm: i32,
    clr_used: u32,
    clr_important: u32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetWindowRect(hwnd: isize, rect: *mut i32) -> i32;
    fn GetDC(hwnd: isize) -> isize;
    fn ReleaseDC(hwnd: isize, dc: isize) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(dc: isize) -> isize;
    fn DeleteDC(dc: isize) -> i32;
    fn CreateDIBSection(dc: isize, info: *const BitmapInfoHeader, usage: u32, bits: *mut *mut c_void, section: isize, offset: u32) -> isize;
    fn SelectObject(dc: isize, object: isize) -> isize;
    fn DeleteObject(object: isize) -> i32;
    fn BitBlt(dst: isize, x: i32, y: i32, w: i32, h: i32, src: isize, sx: i32, sy: i32, rop: u32) -> i32;
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(window: isize, attribute: u32, value: *const u32, size: u32) -> i32;
    fn DwmFlush() -> i32;
}

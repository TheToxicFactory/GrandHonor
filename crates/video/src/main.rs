//! GrandHonor.Video.exe: the browser starts it as the native messaging host
//! `com.grandhonor.video` for one tab with a playing video. It captures the video's area of the
//! browser window, runs Intel's AI upscaling and frame generation on the GPU (Intel VPL from the
//! graphics driver) and shows the result in a click-through layer over the player. It exits
//! when the browser closes the port. It runs unelevated, as the browser's child.
#![windows_subsystem = "windows"]

mod badge;
mod gpu;
mod overlay;
mod sharpen;
mod vpl;

/// The only extension allowed to start this host (fixed by the key in its manifest).
const EXTENSION_ORIGIN: &str = "chrome-extension://hkhjfheinfpppebmejofemdgpimphgpd/";

fn main() {
    if std::env::args().nth(1).as_deref() != Some(EXTENSION_ORIGIN) {
        return;
    }
    unsafe {
        // Real pixels: the layer must line up with the browser's device pixels.
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
    }
    let gpu = match gpu::Gpu::new() {
        Ok(gpu) => gpu,
        Err(e) => {
            overlay::log(&format!("D3D11: {e}"));
            return;
        }
    };
    let _ = overlay::run(gpu);
}

//! The layer: started by the browser as a native messaging host for one tab.
//! The extension sends where the video sits in the viewport and its own size; the page draws the
//! video at that size in the middle of the player. This process finds the browser window,
//! captures that area, runs Intel VPL frame generation and AI upscaling, and shows the result in
//! a click-through, topmost window over the player. It quits when the browser closes the port.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::core::{w, Interface, BOOL};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::badge::Badge;
use crate::gpu::{align16, Gpu};
use crate::vpl::{Filter, Stage, NV12, RGB4};

/// Diagnostics for this prototype stage: %TEMP%\grandhonor-video.log (kept under 1 MB).
pub fn log(msg: &str) {
    let path = std::env::temp_dir().join("grandhonor-video.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) { let _ = std::fs::remove_file(&path); }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{:?} {msg}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64());
    }
}

#[derive(Clone, Default, PartialEq)]
struct PageState {
    active: bool,
    nat_w: u32,
    nat_h: u32,
    dpr: f64,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    scale: f64,
    inner_w: f64,
    inner_h: f64,
    upscale: bool,
    frame_gen: bool,
    badge: bool,
    stats: bool,
    ru: bool,
    /// The video's own frame rate and the frame generation target (60 or 120).
    fps: f64,
    frame_target: u32,
    /// Sharpening after processing, 0 off to 3 strong.
    sharpness: u32,
    /// The extension's NPU badge is visible in the picture's corner.
    stack: bool,
}

fn parse(v: &Value) -> Option<PageState> {
    if v.get("type")?.as_str()? != "state" { return None; }
    let f = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    Some(PageState {
        active: v.get("active").and_then(Value::as_bool).unwrap_or(false),
        nat_w: f("natW") as u32, nat_h: f("natH") as u32, dpr: f("dpr").max(0.5),
        x: f("x"), y: f("y"), w: f("w"), h: f("h"), scale: f("scale"), inner_w: f("innerW"), inner_h: f("innerH"),
        upscale: b("upscale"), frame_gen: b("frameGen"), badge: b("badge"), stats: b("stats"), ru: b("ru"),
        sharpness: (f("sharpness") as u32).min(3), stack: b("stack"),
        fps: f("fps"), frame_target: if f("frameRate") >= 100.0 { 120 } else { 60 },
    })
}

struct Shared {
    state: Option<PageState>,
    closed: bool,
}

fn reader(shared: Arc<Mutex<Shared>>) {
    let mut stdin = std::io::stdin().lock();
    loop {
        let mut len = [0u8; 4];
        if stdin.read_exact(&mut len).is_err() { break; }
        let size = u32::from_le_bytes(len) as usize;
        if size > 64 * 1024 { break; } // page state is a few hundred bytes
        let mut buf = vec![0u8; size];
        if stdin.read_exact(&mut buf).is_err() { break; }
        if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
            if let Some(s) = parse(&v) { shared.lock().unwrap().state = Some(s); }
        }
    }
    shared.lock().unwrap().closed = true;
}

fn reply(v: Value) {
    let text = v.to_string();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&(text.len() as u32).to_le_bytes());
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// The browser process: Chrome and Edge start native hosts through cmd.exe, so walk up the
/// parents to the first process that is not cmd.exe or conhost.exe.
fn parent_pid() -> u32 {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return 0 };
        let mut table = Vec::new();
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            table.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase()));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        let mut pid = GetCurrentProcessId();
        for _ in 0..4 {
            let Some(&(_, parent, _)) = table.iter().find(|p| p.0 == pid) else { return 0 };
            pid = parent;
            match table.iter().find(|p| p.0 == pid) {
                Some((_, _, name)) if name == "cmd.exe" || name == "conhost.exe" => continue,
                Some(_) => return pid,
                None => return 0,
            }
        }
        0
    }
}
fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The browser's top-level window whose page area has the size the page reports, and the screen
/// position of that page area. The foreground window wins when several match.
fn find_browser(pid: u32, want_w: i32, want_h: i32) -> Option<(HWND, POINT)> {
    struct Ctx { pid: u32, want: (i32, i32), found: Vec<(HWND, POINT)> }
    unsafe extern "system" fn child(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = &mut *(lp.0 as *mut (Ctx, HWND));
        if class_of(hwnd) == "Chrome_RenderWidgetHostHWND" && IsWindowVisible(hwnd).as_bool() {
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            if (rc.right - ctx.0.want.0).abs() <= 3 && (rc.bottom - ctx.0.want.1).abs() <= 3 {
                let mut p = POINT::default();
                let _ = ClientToScreen(hwnd, &mut p);
                ctx.0.found.push((ctx.1, p));
            }
        }
        true.into()
    }
    unsafe extern "system" fn top(hwnd: HWND, lp: LPARAM) -> BOOL {
        let ctx = &mut *(lp.0 as *mut Ctx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == ctx.pid && IsWindowVisible(hwnd).as_bool() && class_of(hwnd) == "Chrome_WidgetWin_1" {
            let mut pair = (Ctx { pid: ctx.pid, want: ctx.want, found: Vec::new() }, hwnd);
            let _ = EnumChildWindows(Some(hwnd), Some(child), LPARAM(&mut pair as *mut _ as isize));
            ctx.found.extend(pair.0.found);
        }
        true.into()
    }
    let mut ctx = Ctx { pid, want: (want_w, want_h), found: Vec::new() };
    unsafe { let _ = EnumWindows(Some(top), LPARAM(&mut ctx as *mut _ as isize)); }
    let fg = unsafe { GetForegroundWindow() };
    ctx.found.iter().find(|(h, _)| *h == fg).or(ctx.found.first()).copied()
}

/// True when the player area is on screen and no other window lies over it (checked at the
/// centre and near the corners; our own click-through layer does not count).
fn uncovered(top: HWND, layer: HWND, origin: POINT, s: &PageState) -> bool {
    let (x0, y0) = (origin.x as f64 + s.x * s.dpr, origin.y as f64 + s.y * s.dpr);
    let (w, h) = (s.w * s.dpr, s.h * s.dpr);
    [(0.5, 0.5), (0.1, 0.1), (0.9, 0.1), (0.1, 0.9), (0.9, 0.9)].iter().all(|(fx, fy)| unsafe {
        let p = POINT { x: (x0 + w * fx) as i32, y: (y0 + h * fy) as i32 };
        let root = GetAncestor(WindowFromPoint(p), GA_ROOT);
        root == top || root == layer
    })
}

struct Capture {
    hwnd: HWND,
    pool: Direct3D11CaptureFramePool,
    _session: GraphicsCaptureSession,
    size: SizeInt32,
    device: IDirect3DDevice,
}

impl Capture {
    fn start(gpu: &Gpu, hwnd: HWND) -> Result<Capture, String> {
        unsafe {
            let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(|e| e.to_string())?;
            let item: GraphicsCaptureItem = interop.CreateForWindow(hwnd).map_err(|e| format!("capture item: {e}"))?;
            let size = item.Size().map_err(|e| e.to_string())?;
            let dxgi: IDXGIDevice = gpu.device.cast().map_err(|e| e.to_string())?;
            let device: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).map_err(|e| e.to_string())?.cast().map_err(|e| e.to_string())?;
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size).map_err(|e| e.to_string())?;
            let session = pool.CreateCaptureSession(&item).map_err(|e| e.to_string())?;
            let _ = session.SetIsCursorCaptureEnabled(false);
            let _ = session.SetIsBorderRequired(false);
            session.StartCapture().map_err(|e| e.to_string())?;
            Ok(Capture { hwnd, pool, _session: session, size, device })
        }
    }

    /// The newest captured frame, if a new one arrived (older queued frames are dropped).
    fn latest(&mut self) -> Option<ID3D11Texture2D> {
        let mut newest = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            if let Some(old) = newest.replace(frame) { let _ = old.Close(); }
        }
        let frame = newest?;
        let size = frame.ContentSize().ok()?;
        if size.Width != self.size.Width || size.Height != self.size.Height {
            self.size = size;
            let _ = self.pool.Recreate(&self.device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, size);
            let _ = frame.Close();
            return None;
        }
        let tex = unsafe {
            frame.Surface().ok()?.cast::<IDirect3DDxgiInterfaceAccess>().ok()?.GetInterface::<ID3D11Texture2D>().ok()?
        };
        let _ = frame.Close();
        Some(tex)
    }
}

/// Crop of the captured window (the player as the browser shows it, `sw`x`sh`) scaled to the
/// video's own size (`w`x`h`) and converted to NV12 for VPL.
struct Converter {
    sw: u32,
    sh: u32,
    w: u32,
    h: u32,
    bgra: ID3D11Texture2D,
    nv12: ID3D11Texture2D,
    vctx: ID3D11VideoContext,
    vp: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    output: ID3D11VideoProcessorOutputView,
}

impl Converter {
    fn new(gpu: &Gpu, sw: u32, sh: u32, w: u32, h: u32) -> Result<Converter, String> {
        let e = |e: windows::core::Error| e.to_string();
        unsafe {
            let bgra = gpu.texture(sw, sh, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, false).map_err(e)?;
            let nv12 = gpu.texture(align16(w), align16(h), DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, true).map_err(e)?;
            let vdev: ID3D11VideoDevice = gpu.device.cast().map_err(e)?;
            let vctx: ID3D11VideoContext = gpu.context.cast().map_err(e)?;
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 }, InputWidth: sw, InputHeight: sh,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 30, Denominator: 1 }, OutputWidth: align16(w), OutputHeight: align16(h),
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let en = vdev.CreateVideoProcessorEnumerator(&desc).map_err(e)?;
            let vp = vdev.CreateVideoProcessor(&en, 0).map_err(e)?;
            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC { FourCC: 0, ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } } };
            let mut input = None;
            vdev.CreateVideoProcessorInputView(&bgra, &en, &in_desc, Some(&mut input)).map_err(e)?;
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC { ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } } };
            let mut output = None;
            vdev.CreateVideoProcessorOutputView(&nv12, &en, &out_desc, Some(&mut output)).map_err(e)?;
            if let Ok(v1) = vctx.cast::<ID3D11VideoContext1>() {
                v1.VideoProcessorSetStreamColorSpace1(&vp, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                v1.VideoProcessorSetOutputColorSpace1(&vp, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            }
            vctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            let target = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            vctx.VideoProcessorSetOutputTargetRect(&vp, true, Some(&target));
            vctx.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&target));
            let source = RECT { left: 0, top: 0, right: sw as i32, bottom: sh as i32 };
            vctx.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&source));
            Ok(Converter { sw, sh, w, h, bgra, nv12, vctx, vp, input: input.unwrap(), output: output.unwrap() })
        }
    }

    fn convert(&self, gpu: &Gpu, frame: &ID3D11Texture2D, x: u32, y: u32) -> Result<(), String> {
        unsafe {
            let bx = D3D11_BOX { left: x, top: y, front: 0, right: x + self.sw, bottom: y + self.sh, back: 1 };
            gpu.context.CopySubresourceRegion(&self.bgra, 0, 0, 0, 0, frame, 0, Some(&bx));
            let stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: std::mem::ManuallyDrop::new(Some(self.input.clone())), ..Default::default() };
            let mut streams = [stream];
            let r = self.vctx.VideoProcessorBlt(&self.vp, &self.output, 0, &streams);
            std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
            r.map_err(|e| format!("convert: {e}"))
        }
    }
}

thread_local! { static STAGE_MS: std::cell::Cell<(f64, f64)> = const { std::cell::Cell::new((0.0, 0.0)) }; }

/// VPL sessions and D3D11 objects built on a worker thread and handed back once.
struct Sendable<T>(T);
unsafe impl<T> Send for Sendable<T> {}

type Key = (u32, u32, u32, u32, bool, u32, u32);

struct Pipeline {
    /// (in w, in h, out w, out h, AI upscaling, source fps, generated fps or 0)
    key: Key,
    fi: Option<Stage>,
    last: Stage,
    between: ID3D11Texture2D,
}

impl Pipeline {
    fn new(gpu: &Gpu, key: Key) -> Result<Pipeline, String> {
        let (in_w, in_h, out_w, out_h, sr, fps, target) = key;
        let fi = target > 0;
        let device = gpu.device.as_raw();
        let env = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        // Algorithm 1: on the B390 it looks the same as 2 at about half the GPU power.
        let algorithm = env("GRANDHONOR_SR_ALG", 1);
        let fi_mode = env("GRANDHONOR_FI_MODE", 3);
        let sr_mode = env("GRANDHONOR_SR_MODE", 1);
        unsafe {
            let fi_stage = if fi { Some(Stage::new(device, "fi", Filter::Interpolation { mode: fi_mode, out_fps: target }, in_w, in_h, fps, in_w, in_h, NV12)?) } else { None };
            let rate = if fi { target } else { fps };
            let last = if sr {
                Stage::new(device, "sr", Filter::SuperResolution { mode: sr_mode, algorithm }, in_w, in_h, rate, out_w, out_h, RGB4)?
            } else {
                Stage::new(device, "plain", Filter::Plain, in_w, in_h, rate, out_w, out_h, RGB4)?
            };
            let between = gpu.texture(align16(in_w), align16(in_h), DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, true).map_err(|e| e.to_string())?;
            log(&format!("pipeline {in_w}x{in_h} -> {out_w}x{out_h} sr={sr} (alg {algorithm}, mode {sr_mode}) fi {fps}->{target} (mode {fi_mode}): session {:.0}+{:.0} ms, init sr {:.0} ms fi {:.0} ms",
                last.create_ms, fi_stage.as_ref().map(|s| s.create_ms).unwrap_or(0.0), last.init_ms, fi_stage.as_ref().map(|s| s.init_ms).unwrap_or(0.0)));
            Ok(Pipeline { key, fi: fi_stage, last, between })
        }
    }

    /// One captured frame in; one or two finished frames (BGRA texture, subresource) out.
    fn run(&self, gpu: &Gpu, nv12: &ID3D11Texture2D, order: u32, ts: u64) -> Result<Vec<crate::vpl::Output>, String> {
        unsafe {
            let Some(fi) = &self.fi else { return self.last.process(nv12.as_raw(), order, ts) };
            let mut all = Vec::new();
            let t = Instant::now();
            let mids = fi.process(nv12.as_raw(), order, ts)?;
            STAGE_MS.with(|s| s.set((s.get().0 + t.elapsed().as_secs_f64() * 1000.0, s.get().1)));
            for (k, m) in mids.iter().enumerate() {
                let (tex, index) = m.texture()?;
                let tex = ID3D11Texture2D::from_raw_borrowed(&tex).unwrap();
                let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: self.key.0, bottom: self.key.1, back: 1 };
                gpu.context.CopySubresourceRegion(&self.between, 0, 0, 0, 0, tex, index, Some(&bx));
                let t = Instant::now();
                all.extend(self.last.process(self.between.as_raw(), order * 8 + k as u32, ts)?);
                STAGE_MS.with(|s| s.set((s.get().0, s.get().1 + t.elapsed().as_secs_f64() * 1000.0)));
            }
            Ok(all)
        }
    }
}

struct Layer {
    hwnd: HWND,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual,
    swap: IDXGISwapChain1,
    size: (u32, u32),
    visible: bool,
    pos: RECT,
    badge: Option<Badge>,
    sharpen: Option<crate::sharpen::Sharpen>,
    /// Sharpening level for the next frames (0 off .. 3 strong).
    sharpness: u32,
    /// Badge distance from the top edge beyond the usual margin (below the extension's badge).
    badge_offset: f32,
    /// Badge text for the next frames (None: no badge) and its font size in swap-chain pixels.
    label: Option<String>,
    badge_px: f32,
}

unsafe extern "system" fn layer_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST { return LRESULT(-1); } // HTTRANSPARENT: clicks go to the browser
    DefWindowProcW(hwnd, msg, wp, lp)
}

impl Layer {
    fn new(gpu: &Gpu) -> Result<Layer, String> {
        let e = |e: windows::core::Error| e.to_string();
        unsafe {
            let instance = GetModuleHandleW(None).map_err(e)?;
            let class = WNDCLASSW { lpfnWndProc: Some(layer_proc), hInstance: instance.into(), lpszClassName: w!("GrandHonorVideoLayer"), ..Default::default() };
            RegisterClassW(&class);
            let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_NOREDIRECTIONBITMAP;
            let hwnd = CreateWindowExW(ex, w!("GrandHonorVideoLayer"), w!("GrandHonor video"), WS_POPUP, 0, 0, 16, 16, None, None, Some(instance.into()), None).map_err(e)?;
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            let dxgi: IDXGIDevice = gpu.device.cast().map_err(e)?;
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi).map_err(e)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true).map_err(e)?;
            let visual = dcomp.CreateVisual().map_err(e)?;
            let factory: IDXGIFactory2 = CreateDXGIFactory1().map_err(e)?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: 64, Height: 64, Format: DXGI_FORMAT_B8G8R8A8_UNORM, SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT, BufferCount: 2, Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, AlphaMode: DXGI_ALPHA_MODE_IGNORE, Flags: 0, Stereo: false.into(),
            };
            let swap = factory.CreateSwapChainForComposition(&gpu.device, &desc, None).map_err(e)?;
            visual.SetContent(&swap).map_err(e)?;
            target.SetRoot(&visual).map_err(e)?;
            dcomp.Commit().map_err(e)?;
            let badge = Badge::new(gpu).map_err(|e| log(&format!("badge unavailable: {e}"))).ok();
            let sharpen = crate::sharpen::Sharpen::new(gpu).map_err(|e| log(&format!("sharpening unavailable: {e}"))).ok();
            Ok(Layer { hwnd, _dcomp: dcomp, _target: target, visual: visual.clone(), swap, size: (64, 64), visible: false, pos: RECT::default(),
                badge, sharpen, sharpness: 0, badge_offset: 0.0, label: None, badge_px: 12.0 })
        }
    }

    fn place(&mut self, r: RECT) {
        if r == self.pos && self.visible { return; }
        self.pos = r;
        self.fit();
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), r.left, r.top, r.right - r.left, r.bottom - r.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        self.visible = true;
    }

    /// Scales the swap chain (which may be larger than the player, see `out_size`) to the window.
    fn fit(&self) {
        let (w, h) = ((self.pos.right - self.pos.left) as f32, (self.pos.bottom - self.pos.top) as f32);
        let m = windows_numerics::Matrix3x2 { M11: w / self.size.0 as f32, M12: 0.0, M21: 0.0, M22: h / self.size.1 as f32, M31: 0.0, M32: 0.0 };
        unsafe {
            let _ = self.visual.SetBitmapInterpolationMode(DCOMPOSITION_BITMAP_INTERPOLATION_MODE_LINEAR);
            let _ = self.visual.SetTransform2(&m);
            let _ = self._dcomp.Commit();
        }
    }

    fn hide(&mut self) {
        if self.visible { unsafe { let _ = ShowWindow(self.hwnd, SW_HIDE); } }
        self.visible = false;
    }

    fn present(&mut self, gpu: &Gpu, tex: &ID3D11Texture2D, index: u32, w: u32, h: u32) -> Result<(), String> {
        unsafe {
            if self.size != (w, h) {
                self.swap.ResizeBuffers(2, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0)).map_err(|e| e.to_string())?;
                self.size = (w, h);
                self.fit();
            }
            let back: ID3D11Texture2D = self.swap.GetBuffer(0).map_err(|e| e.to_string())?;
            let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
            let sharpened = match (&mut self.sharpen, self.sharpness) {
                (Some(sh), level) if level > 0 && index == 0 => sh.draw(gpu, tex, &back, w, h, level).map_err(|e| log(&format!("sharpen: {e}"))).is_ok(),
                _ => false,
            };
            if !sharpened {
                gpu.context.CopySubresourceRegion(&back, 0, 0, 0, 0, tex, index, Some(&bx));
            }
            if let (Some(badge), Some(text)) = (&mut self.badge, &self.label) {
                if let Err(e) = badge.draw(&back, w, text, self.badge_px, self.badge_offset) { log(&format!("badge: {e}")); self.badge = None; }
            }
            let _ = self.swap.Present(0, DXGI_PRESENT(0));
        }
        Ok(())
    }
}

/// Shows every queued frame whose time has come (only the newest if several are late).
fn present_due(gpu: &Gpu, layer: &mut Layer, queue: &mut std::collections::VecDeque<(Instant, crate::vpl::Output)>, w: u32, h: u32) -> Result<(), String> {
    let now = Instant::now();
    let mut ready = None;
    while queue.front().is_some_and(|(due, _)| *due <= now) {
        ready = queue.pop_front().map(|(_, out)| out);
    }
    if let Some(out) = ready {
        let (tex, index) = unsafe { out.texture()? };
        let tex = unsafe { ID3D11Texture2D::from_raw_borrowed(&tex).unwrap() };
        layer.present(gpu, tex, index, w, h)?;
    }
    Ok(())
}

/// The VSR badge text drawn on the layer: what runs, and with details the sizes, rates and GPU time.
fn badge_text(s: &PageState, generating: u32, src: (u32, u32), dst: (u32, u32), live: Option<(f64, f64, f64)>) -> String {
    let frames = if s.ru { format!("кадры до {generating}") } else { format!("frames to {generating}") };
    let ai = if s.ru { "ИИ Intel" } else { "Intel AI" };
    let mode = match (s.upscale, generating > 0) {
        (true, true) => format!("{ai} · {frames}"),
        (true, false) => ai.to_string(),
        (false, _) => format!("{frames} · Intel"),
    };
    match live.filter(|_| s.stats) {
        Some((fin, fout, ms)) => {
            let (fps, msu) = if s.ru { ("к/с", "мс") } else { ("fps", "ms") };
            format!("{mode} · {}×{} → {}×{} · {:.0} → {:.0} {fps} · {:.0} {msu}", src.0, src.1, dst.0, dst.1, fin, fout, ms)
        }
        None => mode,
    }
}

pub fn run(gpu: Gpu) -> Result<(), String> {
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));
    let result = run_inner(gpu);
    if let Err(e) = &result { log(&format!("error: {e}")); }
    result
}

fn run_inner(gpu: Gpu) -> Result<(), String> {
    log(&format!("start, args {:?}", std::env::args().collect::<Vec<_>>()));
    let shared = Arc::new(Mutex::new(Shared { state: None, closed: false }));
    { let s = shared.clone(); std::thread::spawn(move || reader(s)); }
    std::thread::spawn(|| { let t = Instant::now(); crate::vpl::warm_up(); log(&format!("VPL ready in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0)); });
    let browser_pid = parent_pid();
    let mut layer = Layer::new(&gpu)?;
    let mut capture: Option<Capture> = None;
    let mut converter: Option<Converter> = None;
    let mut pipeline: Option<Pipeline> = None;
    let mut pending: Option<(Key, std::thread::JoinHandle<Sendable<Result<Pipeline, String>>>)> = None;
    let mut shown = false;
    let mut order = 0u32;
    let mut idle_since = Instant::now();
    let mut rate = (Instant::now(), 0u32, 30.0f64); // window start, captured frames, measured fps
    let clock = Instant::now();
    let mut stats = (Instant::now(), 0u32, 0u32, 0.0f64);
    let mut live = (Instant::now(), 0u32, 0u32, 0.0f64);
    let mut live_shown: Option<(f64, f64, f64)> = None;
    let mut queue: std::collections::VecDeque<(Instant, crate::vpl::Output)> = std::collections::VecDeque::new();
    let mut last_why = (Instant::now(), "");
    // Tests only: show the layer even when another window covers the browser.
    let force = std::env::var_os("GRANDHONOR_VIDEO_FORCE").is_some();
    loop {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() { DispatchMessageW(&msg); }
        }
        let (state, closed) = { let s = shared.lock().unwrap(); (s.state.clone(), s.closed) };
        if closed { log("port closed"); break; }
        let mut want = None;
        let mut why = "no page state";
        if let Some(s) = state.clone().filter(|s| s.active && s.nat_w >= 64 && s.w >= 64.0) {
            let (vw, vh) = ((s.inner_w * s.dpr).round() as i32, (s.inner_h * s.dpr).round() as i32);
            why = "browser page area not found";
            if let Some((top, origin)) = find_browser(browser_pid, vw, vh) {
                why = "player covered by another window";
                if force || uncovered(top, layer.hwnd, origin, &s) { want = Some((s, top, origin)); }
            }
        } else if state.is_some() {
            why = "page says inactive";
        }
        if want.is_none() && last_why.0.elapsed() > Duration::from_secs(1) && last_why.1 != why {
            log(&format!("waiting: {why} (browser pid {browser_pid})"));
            last_why = (Instant::now(), why);
        }
        let Some((s, top, origin)) = want else {
            if shown { queue.clear(); layer.hide(); shown = false; live_shown = None; reply(serde_json::json!({ "type": "shown", "shown": false })); }
            if idle_since.elapsed() > Duration::from_secs(2) { capture = None; }
            std::thread::sleep(Duration::from_millis(30));
            continue;
        };
        idle_since = Instant::now();
        if capture.as_ref().map(|c| c.hwnd) != Some(top) {
            capture = Some(Capture::start(&gpu, top)?);
            log("capture started");
        }
        // Screen rectangles in device pixels. The capture is the player exactly as the browser
        // shows it (controls and captions included), so the layer looks like the page.
        let dpr = s.dpr;
        let target = RECT {
            left: origin.x + (s.x * dpr).round() as i32, top: origin.y + (s.y * dpr).round() as i32,
            right: origin.x + ((s.x + s.w) * dpr).round() as i32, bottom: origin.y + ((s.y + s.h) * dpr).round() as i32,
        };
        let (box_w, box_h) = (((target.right - target.left) as u32) & !1, ((target.bottom - target.top) as u32) & !1);
        let upscale = s.upscale;
        // With AI upscaling the captured player is scaled back to the video's own size first.
        let (in_w, in_h) = if upscale { (s.nat_w & !1, s.nat_h & !1) } else { (box_w, box_h) };
        // Intel SR needs at least 1.5x; below that it renders larger and the layer scales down.
        let (out_w, out_h) = if upscale && (box_w as f64) < in_w as f64 * 1.5 {
            (((in_w as f64 * 1.5).ceil() as u32 + 7) & !7, ((in_h as f64 * 1.5).ceil() as u32 + 1) & !1)
        } else { (box_w, box_h) };
        let mut bounds = RECT::default();
        unsafe { let _ = DwmGetWindowAttribute(top, DWMWA_EXTENDED_FRAME_BOUNDS, &mut bounds as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32); }
        let (crop_x, crop_y) = ((target.left - bounds.left).max(0) as u32, (target.top - bounds.top).max(0) as u32);
        let cap = capture.as_mut().unwrap();
        if crop_x + box_w > cap.size.Width as u32 || crop_y + box_h > cap.size.Height as u32 {
            if !shown && last_why.1 != "crop" { log(&format!("crop {crop_x},{crop_y} {box_w}x{box_h} outside capture {}x{}", cap.size.Width, cap.size.Height)); last_why.1 = "crop"; }
            std::thread::sleep(Duration::from_millis(10));
            let _ = cap.latest();
            continue;
        }
        let Some(frame) = cap.latest() else {
            present_due(&gpu, &mut layer, &mut queue, out_w, out_h)?;
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        rate.1 += 1;
        if rate.0.elapsed() >= Duration::from_secs(1) {
            rate.2 = rate.1 as f64 / rate.0.elapsed().as_secs_f64();
            rate = (Instant::now(), 0, rate.2);
        }
        if converter.as_ref().map(|c| (c.sw, c.sh, c.w, c.h)) != Some((box_w, box_h, in_w, in_h)) {
            converter = Some(Converter::new(&gpu, box_w, box_h, in_w, in_h)?);
        }
        // Frame generation up to the chosen rate (60 or 120), from the video's own frame rate as
        // the page measures it (the capture also changes when the player's controls move).
        let src_fps = if s.fps >= 10.0 { s.fps.round() as u32 } else { 30 };
        let goal = if s.frame_gen && s.frame_target as f64 > src_fps as f64 * 1.15 { s.frame_target } else { 0 };
        let fi = goal > 0;
        if !upscale && !fi {
            // Nothing to do for this video (fast video without upscaling): stay out of the way.
            if shown { queue.clear(); layer.hide(); shown = false; reply(serde_json::json!({ "type": "shown", "shown": false })); }
            continue;
        }
        let key: Key = (in_w, in_h, out_w, out_h, upscale, src_fps, goal);
        // The quick pipeline starts at once; frame generation (about 1 s to set up) is built on a
        // worker and swapped in when ready.
        let quick: Key = (in_w, in_h, out_w, out_h, upscale, src_fps, 0);
        if pending.as_ref().is_some_and(|(k, _)| *k != key) { pending = None; }
        if let Some((k, handle)) = pending.take() {
            if handle.is_finished() {
                match handle.join().map_err(|_| "worker panicked".to_string()).and_then(|p| p.0) {
                    Ok(p) => { queue.clear(); drop(pipeline.take()); pipeline = Some(p); log(&format!("frame generation on {k:?}")); }
                    Err(e) => log(&format!("frame generation failed: {e}")),
                }
            } else {
                pending = Some((k, handle));
            }
        }
        let have = pipeline.as_ref().map(|p| p.key);
        if have != Some(key) && have != Some(quick) {
            // Frames still queued belong to the old sessions: release them first.
            queue.clear();
            drop(pipeline.take());
            pipeline = Some(Pipeline::new(&gpu, quick)?);
        }
        if fi && pipeline.as_ref().map(|p| p.key) != Some(key) && pending.is_none() {
            let g = Sendable(Gpu { device: gpu.device.clone(), context: gpu.context.clone() });
            pending = Some((key, std::thread::spawn(move || {
                let g = g;
                Sendable(Pipeline::new(&g.0, key))
            })));
        }
        let conv = converter.as_ref().unwrap();
        conv.convert(&gpu, &frame, crop_x, crop_y)?;
        if std::env::var("GRANDHONOR_VIDEO_DUMP_AT").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(120) == order {
            if let Some(dir) = std::env::var_os("GRANDHONOR_VIDEO_DUMP") {
                let dir = std::path::PathBuf::from(dir);
                let _ = crate::gpu::dump(&gpu, &conv.bgra, 0, box_w, box_h, &dir.join("video-captured.bmp"));
                if let Some((_, out)) = queue.back() {
                    if let Ok((tex, index)) = unsafe { out.texture() } {
                        let tex = unsafe { ID3D11Texture2D::from_raw_borrowed(&tex).unwrap() };
                        let _ = crate::gpu::dump(&gpu, tex, index, out_w, out_h, &dir.join("video-output.bmp"));
                    }
                }
                log("dumped frames");
            }
        }
        let ts = (clock.elapsed().as_secs_f64() * 90000.0) as u64;
        let work = Instant::now();
        let outputs = pipeline.as_ref().unwrap().run(&gpu, &conv.nv12, order, ts)?;
        live.1 += 1; live.2 += outputs.len() as u32; live.3 += work.elapsed().as_secs_f64() * 1000.0;
        if live.0.elapsed() >= Duration::from_secs(1) {
            let secs = live.0.elapsed().as_secs_f64();
            live_shown = Some((live.1 as f64 / secs, live.2 as f64 / secs, live.3 / live.1.max(1) as f64));
            live = (Instant::now(), 0, 0, 0.0);
        }
        stats.1 += 1; stats.2 += outputs.len() as u32; stats.3 += work.elapsed().as_secs_f64() * 1000.0;
        if stats.0.elapsed() >= Duration::from_secs(5) {
            let secs = stats.0.elapsed().as_secs_f64();
            let (fi_ms, sr_ms) = STAGE_MS.with(|s| s.replace((0.0, 0.0)));
            log(&format!("{:.1} in/s, {:.1} out/s, {:.1} ms per input (fi {:.1}, last {:.1}), {:?}", stats.1 as f64 / secs, stats.2 as f64 / secs,
                stats.3 / stats.1.max(1) as f64, fi_ms / stats.1.max(1) as f64, sr_ms / stats.1.max(1) as f64, pipeline.as_ref().unwrap().key));
            stats = (Instant::now(), 0, 0, 0.0);
        }
        order += 1;
        let n = outputs.len();
        // The first output goes on screen now; the rest are spaced evenly over the source frame
        // and shown from the loop, so capture and processing never wait for them.
        let step = (1.0 / src_fps as f64) / n as f64;
        for (k, out) in outputs.into_iter().enumerate() {
            let due = Instant::now() + Duration::from_secs_f64(step * k as f64);
            queue.push_back((due, out));
        }
        let generating = pipeline.as_ref().map_or(0, |p| p.key.6);
        let scale = out_w as f64 / box_w.max(1) as f64;
        layer.label = s.badge.then(|| badge_text(&s, generating, (in_w, in_h), (box_w, box_h), live_shown));
        layer.badge_px = (12.0 * s.dpr * scale) as f32;
        // The extension's own NPU badge may sit in the corner of the captured picture: go below it.
        layer.badge_offset = if s.stack { (26.0 * s.dpr * scale) as f32 } else { 0.0 };
        layer.sharpness = s.sharpness;
        present_due(&gpu, &mut layer, &mut queue, out_w, out_h)?;
        if !shown && layer.size == (out_w, out_h) {
            layer.place(target);
            shown = true;
            reply(serde_json::json!({ "type": "shown", "shown": true }));
            log(&format!("shown at {target:?}, crop {crop_x},{crop_y} {box_w}x{box_h} -> {in_w}x{in_h}"));
        } else if shown {
            layer.place(target);
        }    }    Ok(())
}

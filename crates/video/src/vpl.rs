//! Minimal Intel VPL (oneVPL 2.x) bindings: VPP sessions on our own D3D11 device, D3D11 texture
//! import for input, native texture access for output. Layouts follow intel/libvpl api/vpl (x64).
#![allow(non_snake_case, dead_code)]

use std::ffi::c_void;
use std::ptr::null_mut;

pub type Session = *mut c_void;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}
pub const NV12: u32 = fourcc(b"NV12");
pub const RGB4: u32 = fourcc(b"RGB4");

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FrameInfo {
    pub reserved: [u32; 4],
    pub ChannelId: u16,
    pub BitDepthLuma: u16,
    pub BitDepthChroma: u16,
    pub Shift: u16,
    pub FrameId: [u16; 4],
    pub FourCC: u32,
    pub Width: u16,
    pub Height: u16,
    pub CropX: u16,
    pub CropY: u16,
    pub CropW: u16,
    pub CropH: u16,
    pub FrameRateExtN: u32,
    pub FrameRateExtD: u32,
    pub reserved3: u16,
    pub AspectRatioW: u16,
    pub AspectRatioH: u16,
    pub PicStruct: u16,
    pub ChromaFormat: u16,
    pub reserved2: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct InfoVPP {
    pub reserved: [u32; 8],
    pub In: FrameInfo,
    pub Out: FrameInfo,
}

#[repr(C)]
pub struct VideoParam {
    pub AllocId: u32,
    pub reserved: [u32; 2],
    pub reserved3: u16,
    pub AsyncDepth: u16,
    pub vpp: InfoVPP,
    pub Protected: u16,
    pub IOPattern: u16,
    pub ExtParam: *mut *mut c_void,
    pub NumExtParam: u16,
    pub reserved2: u16,
}

#[repr(C)]
pub struct FrameData {
    pub ExtParam: *mut c_void,
    pub NumExtParam: u16,
    pub reserved: [u16; 9],
    pub MemType: u16,
    pub PitchHigh: u16,
    pub TimeStamp: u64,
    pub FrameOrder: u32,
    pub Locked: u16,
    pub Pitch: u16,
    pub Y: *mut u8,
    pub UV: *mut u8,
    pub V: *mut u8,
    pub A: *mut u8,
    pub MemId: *mut c_void,
    pub Corrupted: u16,
    pub DataFlag: u16,
}

#[repr(C)]
pub struct FrameSurface {
    pub FrameInterface: *mut FrameSurfaceInterface,
    pub Version: u16,
    pub reserved1: [u16; 3],
    pub Info: FrameInfo,
    pub Data: FrameData,
}

type Fn1 = unsafe extern "C" fn(*mut FrameSurface) -> i32;
#[repr(C)]
pub struct FrameSurfaceInterface {
    pub Context: *mut c_void,
    pub Version: u16,
    pub reserved1: [u16; 3],
    pub AddRef: Fn1,
    pub Release: Fn1,
    pub GetRefCounter: unsafe extern "C" fn(*mut FrameSurface, *mut u32) -> i32,
    pub Map: unsafe extern "C" fn(*mut FrameSurface, u32) -> i32,
    pub Unmap: Fn1,
    pub GetNativeHandle: unsafe extern "C" fn(*mut FrameSurface, *mut *mut c_void, *mut u32) -> i32,
    pub GetDeviceHandle: unsafe extern "C" fn(*mut FrameSurface, *mut *mut c_void, *mut u32) -> i32,
    pub Synchronize: unsafe extern "C" fn(*mut FrameSurface, u32) -> i32,
    pub OnComplete: usize,
    pub QueryInterface: usize,
    pub Export: usize,
    pub reserved2: [usize; 1],
}

#[repr(C)]
pub struct ExtBufferHeader {
    pub BufferId: u32,
    pub BufferSz: u32,
}

#[repr(C)]
pub struct ExtSuperResolution {
    pub Header: ExtBufferHeader,
    pub SRMode: u32,
    pub SRAlgorithm: u32,
    pub reserved1: [u32; 15],
    pub reserved2: [*mut c_void; 4],
}

#[repr(C)]
pub struct ExtFrameInterpolation {
    pub Header: ExtBufferHeader,
    pub FIMode: u32,
    pub EnableScd: u16,
    pub reserved1: [u32; 24],
    pub reserved2: [*mut c_void; 8],
}

#[repr(C)]
pub struct SurfaceHeader {
    pub SurfaceType: u32,
    pub SurfaceFlags: u32,
    pub StructSize: u32,
    pub NumExtParam: u16,
    pub ExtParam: *mut c_void,
    pub reserved: [u32; 6],
}

#[repr(C)]
pub struct SurfaceInterface {
    pub Header: SurfaceHeader,
    pub Version: u16,
    pub Context: *mut c_void,
    pub AddRef: usize,
    pub Release: usize,
    pub GetRefCounter: usize,
    pub Synchronize: usize,
    pub reserved: [usize; 11],
}

#[repr(C)]
pub struct SurfaceD3D11Tex2D {
    pub SurfaceInterface: SurfaceInterface,
    pub texture2D: *mut c_void,
    pub reserved: [*mut c_void; 7],
}

#[repr(C)]
pub struct MemoryInterface {
    pub Context: *mut c_void,
    pub Version: u16,
    pub ImportFrameSurface: unsafe extern "C" fn(*mut MemoryInterface, u32, *mut SurfaceHeader, *mut *mut FrameSurface) -> i32,
    pub GetBitstreamBuffer: usize,
    pub reserved: [*mut c_void; 15],
}

const _: () = {
    assert!(std::mem::size_of::<FrameInfo>() == 68);
    assert!(std::mem::size_of::<VideoParam>() == 208);
    assert!(std::mem::size_of::<FrameSurface>() == 184);
    assert!(std::mem::size_of::<ExtSuperResolution>() == 112);
    assert!(std::mem::size_of::<ExtFrameInterpolation>() == 176);
    assert!(std::mem::size_of::<SurfaceHeader>() == 48);
    assert!(std::mem::size_of::<SurfaceInterface>() == 184);
};

pub const ERR_MORE_DATA: i32 = -10;
pub const ERR_MORE_SURFACE: i32 = -11;
pub const WRN_DEVICE_BUSY: i32 = 2;
const HANDLE_D3D11_DEVICE: u32 = 3;
const HANDLE_MEMORY_INTERFACE: u32 = 1001;
const SURFACE_TYPE_D3D11_TEX2D: u32 = 2;
const SURFACE_FLAG_IMPORT_SHARED: u32 = 0x10;
const SURFACE_FLAG_IMPORT_COPY: u32 = 0x20;
const SURFACE_COMPONENT_VPP_INPUT: u32 = 3;
const IOPATTERN_VIDEO: u16 = 0x11;

#[link(name = "libvpl", kind = "raw-dylib")]
extern "C" {
    fn MFXLoad() -> *mut c_void;
    fn MFXUnload(loader: *mut c_void);
    fn MFXCreateConfig(loader: *mut c_void) -> *mut c_void;
    fn MFXSetConfigFilterProperty(config: *mut c_void, name: *const u8, value: Variant) -> i32;
    fn MFXCreateSession(loader: *mut c_void, index: u32, session: *mut Session) -> i32;
    fn MFXClose(session: Session) -> i32;
    fn MFXVideoCORE_SetHandle(session: Session, kind: u32, hdl: *mut c_void) -> i32;
    fn MFXVideoCORE_GetHandle(session: Session, kind: u32, hdl: *mut *mut c_void) -> i32;
    fn MFXVideoCORE_SyncOperation(session: Session, syncp: *mut c_void, wait: u32) -> i32;
    fn MFXVideoVPP_Init(session: Session, par: *mut VideoParam) -> i32;
    fn MFXVideoVPP_Close(session: Session) -> i32;
    fn MFXVideoVPP_RunFrameVPPAsync(session: Session, input: *mut FrameSurface, output: *mut FrameSurface, aux: *mut c_void, syncp: *mut *mut c_void) -> i32;
    fn MFXMemory_GetSurfaceForVPPOut(session: Session, surface: *mut *mut FrameSurface) -> i32;
}

/// One dispatcher for the whole process: it finds the runtime once (about 1.8 s), sessions after
/// that come quickly.
pub fn loader() -> *mut c_void {
    static LOADER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LOADER.get_or_init(|| unsafe {
        let loader = MFXLoad();
        // Hardware Intel runtime on D3D11, adapter 0: these filters let the dispatcher skip
        // querying every runtime (its "low latency" path).
        for (name, value) in [(&b"mfxImplDescription.Impl\0"[..], 2u32), (b"mfxImplDescription.VendorID\0", 0x8086),
                              (b"mfxImplDescription.AccelerationMode\0", 0x0300), (b"DXGIAdapterIndex\0", 0)] {
            let cfg = MFXCreateConfig(loader);
            let v = Variant { version: 0x0101, kind: 5, data: value as u64 };
            let _ = MFXSetConfigFilterProperty(cfg, name.as_ptr(), v);
        }
        loader as usize
    }) as *mut c_void
}

/// Finds the runtime ahead of time so the first real session starts at once.
pub fn warm_up() {
    unsafe {
        let mut session: Session = null_mut();
        if MFXCreateSession(loader(), 0, &mut session) == 0 { MFXClose(session); }
    }
}

/// Copies a plain struct into 8-byte aligned storage that lives as long as the session.
fn aligned<T>(value: &T) -> Box<[u64]> {
    let mut words = vec![0u64; std::mem::size_of::<T>().div_ceil(8)];
    unsafe { std::ptr::copy_nonoverlapping(value as *const T as *const u8, words.as_mut_ptr() as *mut u8, std::mem::size_of::<T>()) };
    words.into_boxed_slice()
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Variant {
    version: u16,
    kind: u32,
    data: u64,
}

pub enum Filter {
    Plain,
    SuperResolution { mode: u32, algorithm: u32 },
    /// Generated frames up to `out_fps` (the input rate is the stage's `in_fps`).
    Interpolation { mode: u32, out_fps: u32 },
}

fn frame(info: &mut FrameInfo, w: u32, h: u32, fps: u32, fourcc: u32) {
    info.FourCC = fourcc;
    info.ChromaFormat = if fourcc == RGB4 { 3 } else { 1 };
    info.PicStruct = 1;
    info.Width = ((w + 15) & !15) as u16;
    info.Height = ((h + 15) & !15) as u16;
    info.CropW = w as u16;
    info.CropH = h as u16;
    info.FrameRateExtN = fps.max(1);
    info.FrameRateExtD = 1;
}

/// One VPP stage. Input is an NV12 D3D11 texture we own; output stays inside VPL until read.
pub struct Stage {
    session: Session,
    loader: *mut c_void,
    memory: *mut MemoryInterface,
    pub name: &'static str,
    pub out_w: u32,
    pub out_h: u32,
    pub init_ms: f64,
    pub create_ms: f64,
    _ext: Box<[u64]>,
}

pub struct Output {
    pub surface: *mut FrameSurface,
}

impl Output {
    /// The D3D11 texture holding the output frame and its subresource index.
    pub unsafe fn texture(&self) -> Result<(*mut c_void, u32), String> {
        let iface = &*(*self.surface).FrameInterface;
        let mut res: *mut c_void = null_mut();
        let mut kind = 0u32;
        let st = (iface.GetNativeHandle)(self.surface, &mut res, &mut kind);
        if st != 0 {
            return Err(format!("GetNativeHandle {st}"));
        }
        Ok((res, 0))
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        unsafe { ((*(*self.surface).FrameInterface).Release)(self.surface) };
    }
}

impl Stage {
    pub unsafe fn new(device: *mut c_void, name: &'static str, filter: Filter, in_w: u32, in_h: u32, in_fps: u32,
                      out_w: u32, out_h: u32, out_fourcc: u32) -> Result<Stage, String> {
        let t0 = std::time::Instant::now();
        let loader = loader();
        let mut session: Session = null_mut();
        let st = MFXCreateSession(loader, 0, &mut session);
        let create_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if st != 0 {
            return Err(format!("{name}: MFXCreateSession {st}"));
        }
        let st = MFXVideoCORE_SetHandle(session, HANDLE_D3D11_DEVICE, device);
        if st != 0 {
            return Err(format!("{name}: SetHandle {st}"));
        }
        let mut par: VideoParam = std::mem::zeroed();
        let out_fps = match filter { Filter::Interpolation { out_fps, .. } => out_fps, _ => in_fps };
        frame(&mut par.vpp.In, in_w, in_h, in_fps, NV12);
        frame(&mut par.vpp.Out, out_w, out_h, out_fps, out_fourcc);
        par.IOPattern = IOPATTERN_VIDEO;
        par.AsyncDepth = 1;
        let mut ext: Box<[u64]> = match filter {
            Filter::Plain => Box::new([]),
            Filter::SuperResolution { mode, algorithm } => {
                let mut b: ExtSuperResolution = std::mem::zeroed();
                b.Header = ExtBufferHeader { BufferId: fourcc(b"VASR"), BufferSz: std::mem::size_of::<ExtSuperResolution>() as u32 };
                b.SRMode = mode;
                b.SRAlgorithm = algorithm;
                aligned(&b)
            }
            Filter::Interpolation { mode, .. } => {
                let mut b: ExtFrameInterpolation = std::mem::zeroed();
                b.Header = ExtBufferHeader { BufferId: fourcc(b"VAFI"), BufferSz: std::mem::size_of::<ExtFrameInterpolation>() as u32 };
                b.FIMode = mode;
                b.EnableScd = 1;
                aligned(&b)
            }
        };
        let mut list = [ext.as_mut_ptr() as *mut c_void];
        if !ext.is_empty() {
            par.ExtParam = list.as_mut_ptr();
            par.NumExtParam = 1;
        }
        let t = std::time::Instant::now();
        let st = MFXVideoVPP_Init(session, &mut par);
        let init_ms = t.elapsed().as_secs_f64() * 1000.0;
        if st < 0 {
            MFXClose(session);
            return Err(format!("{name}: VPP Init {st} ({in_w}x{in_h} -> {out_w}x{out_h})"));
        }
        let mut memory: *mut c_void = null_mut();
        let st = MFXVideoCORE_GetHandle(session, HANDLE_MEMORY_INTERFACE, &mut memory);
        if st != 0 || memory.is_null() {
            return Err(format!("{name}: no memory interface ({st})"));
        }
        Ok(Stage { session, loader, memory: memory as *mut MemoryInterface, name, out_w, out_h, init_ms, create_ms, _ext: ext })
    }

    unsafe fn import(&self, texture: *mut c_void) -> Result<*mut FrameSurface, String> {
        let mut desc: SurfaceD3D11Tex2D = std::mem::zeroed();
        desc.SurfaceInterface.Header.SurfaceType = SURFACE_TYPE_D3D11_TEX2D;
        desc.SurfaceInterface.Header.SurfaceFlags = SURFACE_FLAG_IMPORT_SHARED | SURFACE_FLAG_IMPORT_COPY;
        desc.SurfaceInterface.Header.StructSize = std::mem::size_of::<SurfaceD3D11Tex2D>() as u32;
        desc.texture2D = texture;
        let mut surface: *mut FrameSurface = null_mut();
        let st = ((*self.memory).ImportFrameSurface)(self.memory, SURFACE_COMPONENT_VPP_INPUT, &mut desc.SurfaceInterface.Header, &mut surface);
        if st != 0 {
            return Err(format!("{}: ImportFrameSurface {st}", self.name));
        }
        Ok(surface)
    }

    /// Runs one input frame; returns every output it produced (two for interpolation).
    pub unsafe fn process(&self, texture: *mut c_void, order: u32, timestamp90k: u64) -> Result<Vec<Output>, String> {
        let input = self.import(texture)?;
        (*input).Data.FrameOrder = order;
        (*input).Data.TimeStamp = timestamp90k;
        let mut outputs = Vec::new();
        let result = loop {
            let mut out: *mut FrameSurface = null_mut();
            let st = MFXMemory_GetSurfaceForVPPOut(self.session, &mut out);
            if st != 0 {
                break Err(format!("{}: GetSurfaceForVPPOut {st}", self.name));
            }
            let mut sync: *mut c_void = null_mut();
            let st = MFXVideoVPP_RunFrameVPPAsync(self.session, input, out, null_mut(), &mut sync);
            let output = Output { surface: out };
            if st == WRN_DEVICE_BUSY {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            if !sync.is_null() {
                let s = MFXVideoCORE_SyncOperation(self.session, sync, 2000);
                if s != 0 {
                    break Err(format!("{}: SyncOperation {s}", self.name));
                }
                outputs.push(output);
            }
            match st {
                ERR_MORE_SURFACE => continue,
                0 | ERR_MORE_DATA => break Ok(()),
                other => break Err(format!("{}: RunFrameVPPAsync {other}", self.name)),
            }
        };
        ((*(*input).FrameInterface).Release)(input);
        result.map(|()| outputs)
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        unsafe {
            MFXVideoVPP_Close(self.session);
            MFXClose(self.session);
        }
    }
}

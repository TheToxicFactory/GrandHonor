//! The D3D11 device shared by capture, Intel VPL and the layer, and small texture helpers.

use windows::core::{Interface, Result};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

pub fn align16(v: u32) -> u32 {
    (v + 15) & !15
}

impl Gpu {
    pub fn new() -> Result<Gpu> {
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context))?;
        }
        let device: ID3D11Device = device.unwrap();
        let context: ID3D11DeviceContext = context.unwrap();
        // VPL and frame generation set-up use this device from their own threads.
        let mt: ID3D11Multithread = device.cast()?;
        unsafe { let _ = mt.SetMultithreadProtected(true); }
        Ok(Gpu { device, context })
    }

    pub fn texture(&self, w: u32, h: u32, format: DXGI_FORMAT, bind: D3D11_BIND_FLAG, shared: bool) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind.0 as u32, CPUAccessFlags: 0,
            MiscFlags: if shared { D3D11_RESOURCE_MISC_SHARED.0 as u32 } else { 0 },
        };
        let mut tex = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
        Ok(tex.unwrap())
    }
}

/// Writes a BGRA texture to a 32-bit BMP (diagnostics only, see `GRANDHONOR_VIDEO_DUMP`).
pub fn dump(gpu: &Gpu, tex: &ID3D11Texture2D, index: u32, w: u32, h: u32, path: &std::path::Path) -> std::result::Result<(), String> {
    unsafe {
        let desc = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_STAGING, BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0 };
        let mut staging = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut staging)).map_err(|e| e.to_string())?;
        let staging = staging.unwrap();
        let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
        gpu.context.CopySubresourceRegion(&staging, 0, 0, 0, 0, tex, index, Some(&bx));
        let mut map = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map)).map_err(|e| e.to_string())?;
        let size = 54 + w * h * 4;
        let mut file = Vec::with_capacity(size as usize);
        file.extend_from_slice(b"BM"); file.extend_from_slice(&size.to_le_bytes()); file.extend_from_slice(&0u32.to_le_bytes()); file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes()); file.extend_from_slice(&(w as i32).to_le_bytes()); file.extend_from_slice(&(-(h as i32)).to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes()); file.extend_from_slice(&32u16.to_le_bytes()); file.extend_from_slice(&[0u8; 24]);
        for y in 0..h {
            file.extend_from_slice(std::slice::from_raw_parts((map.pData as *const u8).add((y * map.RowPitch) as usize), (w * 4) as usize));
        }
        gpu.context.Unmap(&staging, 0);
        std::fs::write(path, file).map_err(|e| e.to_string())
    }
}
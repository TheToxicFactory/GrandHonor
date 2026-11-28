//! Contrast-adaptive sharpening (after AMD FidelityFX CAS) drawn straight into the layer's back
//! buffer: Intel's AI upscaling is soft, and this brings edges back for a fraction of a
//! millisecond without boosting noise in flat areas.

use windows::core::{s, Result};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::*;

use crate::gpu::Gpu;

const SHADER: &str = r#"
struct V { float4 pos : SV_Position; };
V vs(uint id : SV_VertexID) {
    V o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}
Texture2D<float4> src : register(t0);
cbuffer C : register(b0) { float sharp; float3 pad; };
float4 ps(float4 pos : SV_Position) : SV_Target {
    int2 p = int2(pos.xy);
    uint w, h; src.GetDimensions(w, h);
    int2 m = int2(w - 1, h - 1);
    #define T(dx, dy) src.Load(int3(clamp(p + int2(dx, dy), int2(0, 0), m), 0)).rgb
    float3 a = T(-1, -1), b = T(0, -1), c = T(1, -1);
    float3 d = T(-1, 0), e = T(0, 0), f = T(1, 0);
    float3 g = T(-1, 1), hh = T(0, 1), i = T(1, 1);
    float3 mn = min(min(min(d, e), min(f, b)), hh);
    mn += min(mn, min(min(a, c), min(g, i)));
    float3 mx = max(max(max(d, e), max(f, b)), hh);
    mx += max(mx, max(max(a, c), max(g, i)));
    float3 amp = sqrt(saturate(min(mn, 2.0 - mx) / max(mx, 1e-5)));
    float3 wgt = amp * (-1.0 / lerp(8.0, 5.0, sharp));
    float3 o = saturate((b * wgt + d * wgt + f * wgt + hh * wgt + e) / (1.0 + 4.0 * wgt));
    return float4(o, 1);
}
"#;

pub struct Sharpen {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    constants: ID3D11Buffer,
    level: u32,
}

fn compile(entry: windows::core::PCSTR, target: windows::core::PCSTR) -> Result<ID3DBlob> {
    let mut blob = None;
    unsafe {
        D3DCompile(SHADER.as_ptr() as *const _, SHADER.len(), s!("sharpen"), None, None, entry, target, 0, 0, &mut blob, None)?;
    }
    Ok(blob.unwrap())
}

fn bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) }
}

impl Sharpen {
    pub fn new(gpu: &Gpu) -> Result<Sharpen> {
        let vsb = compile(s!("vs"), s!("vs_5_0"))?;
        let psb = compile(s!("ps"), s!("ps_5_0"))?;
        unsafe {
            let mut vs = None;
            gpu.device.CreateVertexShader(bytes(&vsb), None, Some(&mut vs))?;
            let mut ps = None;
            gpu.device.CreatePixelShader(bytes(&psb), None, Some(&mut ps))?;
            let desc = D3D11_BUFFER_DESC { ByteWidth: 16, Usage: D3D11_USAGE_DEFAULT, BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, ..Default::default() };
            let mut constants = None;
            gpu.device.CreateBuffer(&desc, None, Some(&mut constants))?;
            Ok(Sharpen { vs: vs.unwrap(), ps: ps.unwrap(), constants: constants.unwrap(), level: u32::MAX })
        }
    }

    /// Draws `source` into `target` (same size), sharpened by `level` (1 light .. 3 strong).
    pub fn draw(&mut self, gpu: &Gpu, source: &ID3D11Texture2D, target: &ID3D11Texture2D, w: u32, h: u32, level: u32) -> Result<()> {
        unsafe {
            let ctx = &gpu.context;
            if self.level != level {
                let amount = [0.0f32, 0.25, 0.55, 0.9][level.min(3) as usize];
                let data = [amount, 0.0, 0.0, 0.0];
                ctx.UpdateSubresource(&self.constants, 0, None, data.as_ptr() as *const _, 0, 0);
                self.level = level;
            }
            let mut srv = None;
            gpu.device.CreateShaderResourceView(source, None, Some(&mut srv))?;
            let mut rtv = None;
            gpu.device.CreateRenderTargetView(target, None, Some(&mut rtv))?;
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.IASetInputLayout(None);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[srv]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.constants.clone())]));
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: w as f32, Height: h as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            ctx.OMSetRenderTargets(Some(&[rtv]), None);
            ctx.Draw(3, 0);
            // Leave nothing bound that VPL or the next frame might trip over.
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
        Ok(())
    }
}

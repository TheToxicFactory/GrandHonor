//! The VSR badge drawn into the layer's frame with Direct2D: "VSR" in semibold, then what runs.
//! Same look as the extension's badge: dark plate, white text, top-right corner of the picture.

use windows::core::{w, Interface, Result};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};

use crate::gpu::Gpu;

pub struct Badge {
    ctx: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    plate: ID2D1SolidColorBrush,
    white: ID2D1SolidColorBrush,
    grey: ID2D1SolidColorBrush,
    formats: Option<(f32, IDWriteTextFormat, IDWriteTextFormat)>,
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

impl Badge {
    pub fn new(gpu: &Gpu) -> Result<Badge> {
        unsafe {
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&D2D1_FACTORY_OPTIONS::default()))?;
            let dxgi: IDXGIDevice = gpu.device.cast()?;
            let device = factory.CreateDevice(&dxgi)?;
            let ctx = device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let plate = ctx.CreateSolidColorBrush(&color(0.0, 0.0, 0.0, 0.69), None)?;
            let white = ctx.CreateSolidColorBrush(&color(1.0, 1.0, 1.0, 1.0), None)?;
            let grey = ctx.CreateSolidColorBrush(&color(0.867, 0.867, 0.867, 1.0), None)?;
            Ok(Badge { ctx, dwrite, plate, white, grey, formats: None })
        }
    }

    fn formats(&mut self, px: f32) -> Result<(IDWriteTextFormat, IDWriteTextFormat)> {
        if let Some((size, bold, regular)) = &self.formats {
            if (*size - px).abs() < 0.5 {
                return Ok((bold.clone(), regular.clone()));
            }
        }
        unsafe {
            let make = |weight| self.dwrite.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, px, w!(""));
            let bold = make(DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            let regular = make(DWRITE_FONT_WEIGHT_NORMAL)?;
            self.formats = Some((px, bold.clone(), regular.clone()));
            Ok((bold, regular))
        }
    }

    /// Draws the badge into `target` (the swap chain's back buffer, `width` pixels wide).
    pub fn draw(&mut self, target: &ID3D11Texture2D, width: u32, text: &str, px: f32, offset: f32) -> Result<()> {
        let (bold, regular) = self.formats(px.max(8.0))?;
        unsafe {
            let head: Vec<u16> = "VSR".encode_utf16().collect();
            let tail: Vec<u16> = text.encode_utf16().collect();
            let head = self.dwrite.CreateTextLayout(&head, &bold, 4000.0, 400.0)?;
            let tail = self.dwrite.CreateTextLayout(&tail, &regular, 8000.0, 400.0)?;
            let (mut hm, mut tm) = (DWRITE_TEXT_METRICS::default(), DWRITE_TEXT_METRICS::default());
            head.GetMetrics(&mut hm)?;
            tail.GetMetrics(&mut tm)?;
            let k = px / 12.0;
            let (pad_x, pad_y, gap, margin) = (7.0 * k, 4.0 * k, 5.0 * k, 8.0 * k);
            let w = pad_x * 2.0 + hm.width + gap + tm.width;
            let h = pad_y * 2.0 + hm.height.max(tm.height);
            let right = width as f32 - margin;
            let (left, top) = (right - w, margin + offset);
            let surface: IDXGISurface = target.cast()?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0, dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bitmap = self.ctx.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.ctx.SetTarget(&bitmap);
            self.ctx.BeginDraw();
            let plate = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left, top, right, bottom: top + h }, radiusX: 6.0 * k, radiusY: 6.0 * k };
            self.ctx.FillRoundedRectangle(&plate, &self.plate);
            let y = top + pad_y;
            self.ctx.DrawTextLayout(windows_numerics::Vector2 { X: left + pad_x, Y: y }, &head, &self.white, D2D1_DRAW_TEXT_OPTIONS_NONE);
            self.ctx.DrawTextLayout(windows_numerics::Vector2 { X: left + pad_x + hm.width + gap, Y: y }, &tail, &self.grey, D2D1_DRAW_TEXT_OPTIONS_NONE);
            let result = self.ctx.EndDraw(None, None);
            self.ctx.SetTarget(None);
            result
        }
    }
}

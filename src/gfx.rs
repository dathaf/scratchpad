//! Rendering surface: Direct2D draws (in software) into a 32-bit memory bitmap with real
//! per-pixel alpha, which is then copied to the window. With the DWM frame extended over the
//! whole window, that alpha lets the backdrop (Acryl/Mica) show through wherever nothing is drawn.
//!
//! Deliberately no Direct3D: a hardware device loads the GPU vendor's driver into the process
//! (~100 MB with NVIDIA), far more than a text window is worth.

use std::ffi::c_void;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

pub type Color = D2D1_COLOR_F;

pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: a as f32 / 255.0 }
}

pub fn rgb(r: u8, g: u8, b: u8) -> Color {
    rgba(r, g, b, 255)
}

/// Gray with alpha, e.g. `gray(0x99, 0xFF)` = 60 % white.
pub fn gray(a: u8, v: u8) -> Color {
    rgba(v, v, v, a)
}

pub fn fade(c: Color, opacity: f32) -> Color {
    Color { a: c.a * opacity, ..c }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

pub fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub struct Gfx {
    pub factory: ID2D1Factory,
    pub dc: ID2D1RenderTarget,
    pub brush: ID2D1SolidColorBrush,
    /// Separate brush for links in the editor; the text layout keeps a reference to it.
    pub link_brush: ID2D1SolidColorBrush,
    /// Likewise for dimmed text (Markdown syntax characters, finished tasks).
    pub muted_brush: ID2D1SolidColorBrush,
    target: ID2D1DCRenderTarget,
    hwnd: HWND,
    memory_dc: HDC,
    bitmap: HBITMAP,
    previous_bitmap: HGDIOBJ,
    width: i32,
    height: i32,
    dpi: f32,
}

/// The slow part of setting up Direct2D: creating the render target loads Windows' software
/// rasteriser (~10 ms). At startup this runs on a worker thread while the window is created.
pub struct Prepared {
    factory: ID2D1Factory,
    target: ID2D1DCRenderTarget,
}

// The factory is created multi-threaded, so its objects may be handed to and used on another thread.
unsafe impl Send for Prepared {}

impl Prepared {
    /// `factory`: reuse an existing Direct2D factory (second surface, e.g. the menu) instead of creating one.
    pub unsafe fn new(factory: Option<&ID2D1Factory>) -> Result<Self> {
        let factory: ID2D1Factory = match factory {
            Some(factory) => factory.clone(),
            None => D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?,
        };
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            ..Default::default()
        };
        let target = factory.CreateDCRenderTarget(&properties)?;
        Ok(Self { factory, target })
    }
}

impl Gfx {
    pub unsafe fn new(prepared: Prepared, hwnd: HWND, width: u32, height: u32, dpi: f32) -> Result<Self> {
        let Prepared { factory, target } = prepared;
        let dc: ID2D1RenderTarget = target.cast()?;
        // ClearType needs an opaque background; on a transparent surface only grayscale is correct.
        dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = dc.CreateSolidColorBrush(&rgb(0, 0, 0), None)?;
        let link_brush = dc.CreateSolidColorBrush(&rgb(0, 0, 0), None)?;
        let muted_brush = dc.CreateSolidColorBrush(&rgb(0, 0, 0), None)?;

        let mut gfx = Self {
            factory, dc, brush, link_brush, muted_brush, target, hwnd,
            memory_dc: CreateCompatibleDC(None),
            bitmap: HBITMAP::default(),
            previous_bitmap: HGDIOBJ::default(),
            width: 0,
            height: 0,
            dpi: 0.0,
        };
        gfx.resize(width, height, dpi)?;
        Ok(gfx)
    }

    pub unsafe fn resize(&mut self, width: u32, height: u32, dpi: f32) -> Result<()> {
        let (width, height) = (width.max(1) as i32, height.max(1) as i32);
        if width == self.width && height == self.height && dpi == self.dpi {
            return Ok(()); // e.g. restored from minimised: nothing to rebuild
        }
        self.dpi = dpi;
        if width != self.width || height != self.height {
            self.release_bitmap();
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            self.bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            self.previous_bitmap = SelectObject(self.memory_dc, self.bitmap);
            self.width = width;
            self.height = height;
        }
        self.dc.SetDpi(dpi, dpi);
        self.target.BindDC(self.memory_dc, &RECT { left: 0, top: 0, right: width, bottom: height })
    }

    unsafe fn release_bitmap(&mut self) {
        if !self.bitmap.is_invalid() {
            SelectObject(self.memory_dc, self.previous_bitmap);
            let _ = DeleteObject(self.bitmap);
            self.bitmap = HBITMAP::default();
        }
    }

    /// Copies the finished frame, including its alpha channel, to the window.
    /// `area` (pixels) limits the copy to the part that was redrawn.
    pub unsafe fn present(&self, area: Option<RECT>) -> Result<()> {
        let r = area.unwrap_or(RECT { left: 0, top: 0, right: self.width, bottom: self.height });
        let window_dc = GetDC(self.hwnd);
        let result = BitBlt(window_dc, r.left, r.top, r.right - r.left, r.bottom - r.top, self.memory_dc, r.left, r.top, SRCCOPY);
        ReleaseDC(self.hwnd, window_dc);
        result
    }

    /// For a layered popup window: shows the frame at a screen position with per-pixel alpha.
    pub unsafe fn present_layered(&self, x: i32, y: i32) -> Result<()> {
        let screen_dc = GetDC(None);
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let result = UpdateLayeredWindow(self.hwnd, screen_dc, Some(&POINT { x, y }), Some(&SIZE { cx: self.width, cy: self.height }),
            self.memory_dc, Some(&POINT { x: 0, y: 0 }), COLORREF(0), Some(&blend), ULW_ALPHA);
        ReleaseDC(None, screen_dc);
        result
    }

    /// Brush fading vertically from `top` (at y0) to `bottom` (at y1); used for glass highlights.
    unsafe fn vertical_gradient(&self, y0: f32, y1: f32, top: Color, bottom: Color) -> Result<ID2D1LinearGradientBrush> {
        let stops = [
            D2D1_GRADIENT_STOP { position: 0.0, color: top },
            D2D1_GRADIENT_STOP { position: 1.0, color: bottom },
        ];
        let collection = self.dc.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let properties = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
            startPoint: D2D_POINT_2F { x: 0.0, y: y0 },
            endPoint: D2D_POINT_2F { x: 0.0, y: y1 },
        };
        self.dc.CreateLinearGradientBrush(&properties, None, &collection)
    }

    /// A one-pixel-wide strip of white that fades out downwards, to be stretched over an area.
    /// Much cheaper per frame than filling the same area with a gradient brush.
    pub unsafe fn fade_strip(&self, top_alpha: f32) -> Result<ID2D1Bitmap> {
        const ROWS: usize = 64;
        let mut pixels = [0u8; ROWS * 4];
        for (row, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            let t = row as f32 / (ROWS - 1) as f32;
            // Premultiplied white: all four channels equal the alpha.
            let alpha = (top_alpha * (1.0 - t).powf(1.4) * 255.0).round() as u8;
            pixel.fill(alpha);
        }
        let properties = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        self.dc.CreateBitmap(D2D_SIZE_U { width: 1, height: ROWS as u32 }, Some(pixels.as_ptr() as *const c_void), 4, &properties)
    }

    pub unsafe fn draw_stretched(&self, bitmap: &ID2D1Bitmap, r: D2D_RECT_F) {
        self.dc.DrawBitmap(bitmap, Some(&r), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
    }

    pub unsafe fn fill_round_gradient(&self, r: D2D_RECT_F, radius: f32, top: Color, bottom: Color) {
        if let Ok(brush) = self.vertical_gradient(r.top, r.bottom, top, bottom) {
            self.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &brush);
        }
    }

    /// Outline that is bright at the top and faint at the bottom: light catching a glass edge.
    pub unsafe fn stroke_round_gradient(&self, r: D2D_RECT_F, radius: f32, top: Color, bottom: Color, width: f32) {
        if let Ok(brush) = self.vertical_gradient(r.top, r.bottom, top, bottom) {
            self.dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &brush, width, None);
        }
    }

    pub unsafe fn fill_rect(&self, r: D2D_RECT_F, c: Color) {
        self.brush.SetColor(&c);
        self.dc.FillRectangle(&r, &self.brush);
    }

    pub unsafe fn fill_round(&self, r: D2D_RECT_F, radius: f32, c: Color) {
        self.brush.SetColor(&c);
        self.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &self.brush);
    }

    pub unsafe fn stroke_round(&self, r: D2D_RECT_F, radius: f32, c: Color, width: f32) {
        self.brush.SetColor(&c);
        self.dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &self.brush, width, None);
    }

    pub unsafe fn fill_circle(&self, cx: f32, cy: f32, radius: f32, c: Color) {
        self.brush.SetColor(&c);
        self.dc.FillEllipse(&D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: radius, radiusY: radius }, &self.brush);
    }

    pub unsafe fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, width: f32) {
        self.brush.SetColor(&c);
        self.dc.DrawLine(D2D_POINT_2F { x: x0, y: y0 }, D2D_POINT_2F { x: x1, y: y1 }, &self.brush, width, None);
    }

    /// Single- or multi-line text, vertically centred in `r`.
    pub unsafe fn text(&self, s: &str, format: &IDWriteTextFormat, r: D2D_RECT_F, c: Color, align: DWRITE_TEXT_ALIGNMENT) {
        let wide: Vec<u16> = s.encode_utf16().collect();
        let _ = format.SetTextAlignment(align);
        let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        self.brush.SetColor(&c);
        self.dc.DrawText(&wide, format, &r, &self.brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, DWRITE_MEASURING_MODE_NATURAL);
    }
}

impl Drop for Gfx {
    fn drop(&mut self) {
        unsafe {
            self.release_bitmap();
            let _ = DeleteDC(self.memory_dc);
        }
    }
}

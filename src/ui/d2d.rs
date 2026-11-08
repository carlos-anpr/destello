//! Shared native drawing primitives. Every target uses physical pixels (96 DPI).
use crate::geom::{Point, Rect};
use std::sync::{Arc, Mutex};
static SHARED: Mutex<Option<Arc<Graphics>>> = Mutex::new(None);
type FormatCache = Vec<((u32, u8), IDWriteTextFormat)>;
use windows::{
    core::*,
    Win32::{
        Foundation::{E_INVALIDARG, HWND},
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
            Dxgi::Common::*,
            Imaging::*,
        },
        System::Com::*,
    },
};
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn rect(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}
pub fn color(rgb: u32, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 255) as f32 / 255.,
        g: ((rgb >> 8) & 255) as f32 / 255.,
        b: (rgb & 255) as f32 / 255.,
        a: alpha,
    }
}
pub struct Graphics {
    pub factory: ID2D1Factory,
    pub text: IDWriteFactory,
    pub wic: AgileReference<IWICImagingFactory>,
    pub(super) icons: Vec<ID2D1PathGeometry>,
    formats: Mutex<FormatCache>,
    tabular: IDWriteTypography,
    stroke: ID2D1StrokeStyle,
}
impl Graphics {
    pub fn new() -> Result<Arc<Self>> {
        let mut shared = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(gfx) = shared.as_ref() {
            return Ok(gfx.clone());
        }
        let gfx = Arc::new(Self::create()?);
        *shared = Some(gfx.clone());
        Ok(gfx)
    }
    fn create() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)?;
            let stroke = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.,
                },
                None,
            )?;
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let text: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let tabular = text.CreateTypography()?;
            tabular.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            let icons = super::icons::build(&factory)?;
            Ok(Self {
                factory,
                stroke,
                text,
                wic: AgileReference::new(&wic)?,
                icons,
                formats: Mutex::new(Vec::new()),
                tabular,
            })
        }
    }
    fn format(&self, size: f32, style: Option<TextStyle>) -> Result<IDWriteTextFormat> {
        let kind = match style {
            None => 0,
            Some(TextStyle::Body) => 1,
            Some(TextStyle::Semibold) => 2,
            Some(TextStyle::Mono) => 3,
        };
        let key = (size.to_bits(), kind);
        let mut cache = self.formats.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, fmt)) = cache.iter().find(|(k, _)| *k == key) {
            return Ok(fmt.clone());
        }
        unsafe {
            let family = match kind {
                0 => w!("Segoe UI Variable"),
                3 => w!("Consolas"),
                _ => w!("Segoe UI Variable Text"),
            };
            let fmt = self.text.CreateTextFormat(
                family,
                None,
                if kind == 2 {
                    DWRITE_FONT_WEIGHT_SEMI_BOLD
                } else {
                    DWRITE_FONT_WEIGHT_NORMAL
                },
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("es-ES"),
            )?;
            if style.is_some() {
                fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            }
            if cache.len() >= 128 {
                cache.remove(0);
            }
            cache.push((key, fmt.clone()));
            Ok(fmt)
        }
    }
    pub fn overlay_target(
        &self,
        hwnd: HWND,
        w: u32,
        h: u32,
        software: bool,
    ) -> Result<ID2D1HwndRenderTarget> {
        let mut props = Self::properties();
        if software {
            props.r#type = D2D1_RENDER_TARGET_TYPE_SOFTWARE;
        }
        self.target(
            hwnd,
            w,
            h,
            &props,
            D2D1_PRESENT_OPTIONS(
                D2D1_PRESENT_OPTIONS_IMMEDIATELY.0 | D2D1_PRESENT_OPTIONS_RETAIN_CONTENTS.0,
            ),
        )
    }
    fn target(
        &self,
        hwnd: HWND,
        w: u32,
        h: u32,
        props: &D2D1_RENDER_TARGET_PROPERTIES,
        present: D2D1_PRESENT_OPTIONS,
    ) -> Result<ID2D1HwndRenderTarget> {
        unsafe {
            let rt = self.factory.CreateHwndRenderTarget(
                props,
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd,
                    pixelSize: D2D_SIZE_U {
                        width: w,
                        height: h,
                    },
                    presentOptions: present,
                },
            )?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(rt)
        }
    }
    pub fn properties() -> D2D1_RENDER_TARGET_PROPERTIES {
        D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.,
            dpiY: 96.,
            ..Default::default()
        }
    }
    pub fn window_target(&self, hwnd: HWND, w: u32, h: u32) -> Result<ID2D1HwndRenderTarget> {
        unsafe {
            let rt = self.factory.CreateHwndRenderTarget(
                &Self::properties(),
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd,
                    pixelSize: D2D_SIZE_U {
                        width: w,
                        height: h,
                    },
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                },
            )?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(rt)
        }
    }
}
pub struct Canvas<'a> {
    pub rt: &'a ID2D1RenderTarget,
    pub gfx: &'a Graphics,
}
impl Canvas<'_> {
    pub fn fill(&self, r: Rect, c: u32, a: f32) -> Result<()> {
        unsafe {
            let b = self.rt.CreateSolidColorBrush(&color(c, a), None)?;
            self.rt.FillRectangle(&rect(r), &b);
            Ok(())
        }
    }
    pub fn border(&self, r: Rect, c: u32, w: f32) -> Result<()> {
        unsafe {
            let b = self.rt.CreateSolidColorBrush(&color(c, 1.), None)?;
            self.rt.DrawRectangle(&rect(r), &b, w, None);
            Ok(())
        }
    }
    pub fn rounded(&self, r: Rect, radius: f32, c: u32, a: f32) -> Result<()> {
        unsafe {
            let b = self.rt.CreateSolidColorBrush(&color(c, a), None)?;
            self.rt.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rect(r),
                    radiusX: radius,
                    radiusY: radius,
                },
                &b,
            );
            Ok(())
        }
    }
    pub fn shadow(&self, r: Rect, radius: f32) -> Result<()> {
        let scale = (radius / theme::RADIUS).max(0.1);
        // Two concentric Gaussian approximations: y=8/blur=32 and y=2/blur=6.
        for (spread, offset, strength, steps) in [(32., 8., 0.45, 20), (6., 2., 0.35, 8)] {
            for n in (1..=steps).rev() {
                let t = n as f32 / steps as f32;
                let grow = (spread * scale * t * 0.5).round() as i32;
                let y = (offset * scale).round() as i32;
                let alpha = strength / steps as f32 * (1. - t * 0.7);
                self.rounded(
                    Rect::new(
                        r.left - grow,
                        r.top - grow + y,
                        r.right + grow,
                        r.bottom + grow + y,
                    ),
                    radius + grow as f32,
                    0,
                    alpha,
                )?;
            }
        }
        Ok(())
    }
    pub fn line(&self, a: Point, b: Point, c: u32, alpha: f32, w: f32) -> Result<()> {
        unsafe {
            let brush = self.rt.CreateSolidColorBrush(&color(c, alpha), None)?;
            let mut e = D2D1_ELLIPSE::default();
            e.point.X = a.x as f32;
            e.point.Y = a.y as f32;
            let mut f = e;
            f.point.X = b.x as f32;
            f.point.Y = b.y as f32;
            self.rt
                .DrawLine(e.point, f.point, &brush, w, &self.gfx.stroke);
            Ok(())
        }
    }
    pub fn path(&self, points: &[Point], color_value: u32, alpha: f32, width: f32) -> Result<()> {
        if points.len() < 2 {
            return Ok(());
        }
        unsafe {
            let path = self.gfx.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            let vectors: Vec<_> = points
                .iter()
                .map(|p| {
                    let mut e = D2D1_ELLIPSE::default();
                    e.point.X = p.x as f32;
                    e.point.Y = p.y as f32;
                    e.point
                })
                .collect();
            sink.BeginFigure(vectors[0], D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLines(&vectors[1..]);
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            let brush = self
                .rt
                .CreateSolidColorBrush(&color(color_value, alpha), None)?;
            self.rt.DrawGeometry(&path, &brush, width, &self.gfx.stroke);
            Ok(())
        }
    }
    pub fn ellipse(&self, r: Rect, c: u32, alpha: f32, w: f32, fill: bool) -> Result<()> {
        unsafe {
            let brush = self.rt.CreateSolidColorBrush(&color(c, alpha), None)?;
            let mut e = D2D1_ELLIPSE {
                radiusX: r.width() as f32 / 2.,
                radiusY: r.height() as f32 / 2.,
                ..Default::default()
            };
            e.point.X = (r.left + r.right) as f32 / 2.;
            e.point.Y = (r.top + r.bottom) as f32 / 2.;
            if fill {
                self.rt.FillEllipse(&e, &brush)
            } else {
                self.rt.DrawEllipse(&e, &brush, w, None)
            }
            Ok(())
        }
    }
    fn text_format(&self, size: f32) -> Result<IDWriteTextFormat> {
        self.gfx.format(size, None)
    }
    pub fn icon(&self, index: usize, r: Rect, col: u32) -> Result<()> {
        unsafe {
            let brush = self.rt.CreateSolidColorBrush(&color(col, 1.), None)?;
            let mut transform = Default::default();
            self.rt.GetTransform(&mut transform);
            let original = transform;
            transform.M11 = r.width() as f32 / 24.;
            transform.M12 = 0.;
            transform.M21 = 0.;
            transform.M22 = r.height() as f32 / 24.;
            transform.M31 = r.left as f32;
            transform.M32 = r.top as f32;
            self.rt.SetTransform(&transform);
            if let Some(geometry) = self.gfx.icons.get(index) {
                self.rt
                    .DrawGeometry(geometry, &brush, 1.6 * 24. / 18., &self.gfx.stroke);
            }
            self.rt.SetTransform(&original);
            Ok(())
        }
    }
    pub fn caret(&self, s: &str, r: Rect, size: f32, utf16_position: u32) -> Result<(Point, i32)> {
        unsafe {
            let fmt = self.text_format(size)?;
            let layout = self.gfx.text.CreateTextLayout(
                &s.encode_utf16().collect::<Vec<_>>(),
                &fmt,
                r.width().max(1) as f32,
                r.height().max(1) as f32,
            )?;
            let (mut x, mut y) = (0., 0.);
            let mut metrics = DWRITE_HIT_TEST_METRICS::default();
            layout.HitTestTextPosition(utf16_position, false, &mut x, &mut y, &mut metrics)?;
            Ok((
                Point {
                    x: r.left + x.round() as i32,
                    y: r.top + y.round() as i32,
                },
                metrics.height.max(size).round() as i32,
            ))
        }
    }
    pub fn text(&self, s: &str, r: Rect, size: f32, c: u32) -> Result<()> {
        unsafe {
            let fmt = self.text_format(size)?;
            let b = self.rt.CreateSolidColorBrush(&color(c, 1.), None)?;
            self.rt.DrawText(
                &s.encode_utf16().collect::<Vec<_>>(),
                &fmt,
                &rect(r),
                &b,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            Ok(())
        }
    }
    pub fn reserve_bitmap(&self, w: u32, h: u32) -> Result<ID2D1Bitmap> {
        if w == 0 || h == 0 {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        unsafe {
            self.rt.CreateBitmap(
                D2D_SIZE_U {
                    width: w,
                    height: h,
                },
                None,
                0,
                &D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.,
                    dpiY: 96.,
                },
            )
        }
    }
    pub fn bitmap(&self, w: u32, h: u32, bgra: &[u8]) -> Result<ID2D1Bitmap> {
        let expected = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(4));
        if w == 0 || h == 0 || w.checked_mul(4).is_none() || expected.is_none_or(|n| n > bgra.len())
        {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        unsafe {
            self.rt.CreateBitmap(
                D2D_SIZE_U {
                    width: w,
                    height: h,
                },
                Some(bgra.as_ptr().cast()),
                w * 4,
                &D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.,
                    dpiY: 96.,
                },
            )
        }
    }
    pub fn image(&self, b: &ID2D1Bitmap, dst: Rect, src: Option<Rect>, nearest: bool) {
        unsafe {
            self.rt.DrawBitmap(
                b,
                Some(&rect(dst)),
                1.,
                if nearest {
                    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR
                } else {
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR
                },
                src.map(rect).as_ref().map(|r| r as *const _),
            );
        }
    }
}
use windows::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::*,
};
pub trait NativeWindow: std::any::Any {
    fn message(&mut self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT>;
}
struct Host {
    alive: std::rc::Rc<std::cell::Cell<bool>>,
    state: Option<Box<dyn NativeWindow>>,
}
unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Host;
    if !ptr.is_null() {
        let alive = unsafe { (*ptr).alive.clone() };
        if msg == WM_NCDESTROY {
            alive.set(false);
        }
        // Temporarily detach the handler: nested modal loops must never alias its mutable state.
        let state = unsafe { (*ptr).state.take() };
        let mut answer = None;
        if let Some(mut state) = state {
            answer = state.message(hwnd, msg, w, l);
            if msg != WM_NCDESTROY
                && alive.get()
                && unsafe {
                    IsWindow(Some(hwnd)).as_bool()
                        && GetWindowLongPtrW(hwnd, GWLP_USERDATA) == ptr as isize
                }
            {
                unsafe {
                    (*ptr).state = Some(state);
                }
            }
        }
        if msg == WM_NCDESTROY {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ptr));
            }
        }
        if let Some(result) = answer {
            return result;
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}
/// Detaches the handler while direct session operations may dispatch nested Win32 messages.
pub fn with_window<T: NativeWindow, R>(hwnd: HWND, action: impl FnOnce(&mut T) -> R) -> Option<R> {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Host;
        if ptr.is_null() {
            return None;
        }
        let alive = (*ptr).alive.clone();
        let mut state = (*ptr).state.take()?;
        let any: &mut dyn std::any::Any = &mut *state;
        let value = any.downcast_mut::<T>().map(action);
        if alive.get() {
            (*ptr).state = Some(state);
        }
        value
    }
}
pub fn window(
    title: &str,
    area: Rect,
    style: WINDOW_STYLE,
    ex: WINDOW_EX_STYLE,
    state: Box<dyn NativeWindow>,
) -> Result<HWND> {
    unsafe {
        let module = GetModuleHandleW(None)?;
        let class = w!("Destello.Native.Surface");
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            hInstance: module.into(),
            lpszClassName: class,
            lpfnWndProc: Some(procedure),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let title = wide(title);
        let hwnd = CreateWindowExW(
            ex,
            class,
            PCWSTR(title.as_ptr()),
            style,
            area.left,
            area.top,
            area.width(),
            area.height(),
            None,
            None,
            Some(module.into()),
            None,
        )?;
        let host = Box::new(Host {
            state: Some(state),
            alive: std::rc::Rc::new(std::cell::Cell::new(true)),
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(host) as isize);
        Ok(hwnd)
    }
}
pub fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
    }
}
pub fn local_point(l: LPARAM) -> Point {
    Point {
        x: (l.0 as u16 as i16) as i32,
        y: ((l.0 >> 16) as u16 as i16) as i32,
    }
}
pub fn close(hwnd: HWND) {
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// Let the Windows 11 compositor round auxiliary surfaces; failures on older Windows are harmless.
pub fn round_window(hwnd: HWND) {
    unsafe {
        let preference = windows::Win32::Graphics::Dwm::DWMWCP_ROUND;
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_WINDOW_CORNER_PREFERENCE,
            (&preference as *const windows::Win32::Graphics::Dwm::DWM_WINDOW_CORNER_PREFERENCE)
                .cast(),
            std::mem::size_of_val(&preference) as u32,
        );
    }
}
/// Visual tokens shared by every floating surface (all dimensions are logical until scaled).
pub mod theme {
    pub const SURFACE: u32 = 0x1b1c22;
    pub const BASE: u32 = 0x16171c;
    pub const ACCENT: u32 = 0x7c5cff;
    pub const TEXT: u32 = 0xf3f3f7;
    pub const MUTED: u32 = 0xa6a6b3;
    pub const DANGER: u32 = 0xff647c;
    pub const SURFACE_OPACITY: f32 = 0.94;
    pub const BORDER_OPACITY: f32 = 0.09;
    pub const RADIUS: f32 = 14.;
    pub const MOTION_SECONDS: f32 = 0.12;
}
#[derive(Clone, Copy)]
pub enum TextStyle {
    Body,
    Semibold,
    Mono,
}
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |shift: u32| {
        let x = ((a >> shift) & 255u32) as f32;
        let y = ((b >> shift) & 255u32) as f32;
        (x + (y - x) * t.clamp(0., 1.)).round() as u32
    };
    ch(16) << 16 | ch(8) << 8 | ch(0)
}
impl Canvas<'_> {
    pub fn rounded_border(
        &self,
        r: Rect,
        radius: f32,
        c: u32,
        alpha: f32,
        width: f32,
    ) -> Result<()> {
        unsafe {
            let brush = self.rt.CreateSolidColorBrush(&color(c, alpha), None)?;
            self.rt.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rect(r),
                    radiusX: radius,
                    radiusY: radius,
                },
                &brush,
                width,
                None,
            );
            Ok(())
        }
    }
    pub fn panel(&self, r: Rect, scale: f32, opacity: f32) -> Result<()> {
        self.shadow(r, theme::RADIUS * scale)?;
        self.rounded(
            r,
            theme::RADIUS * scale,
            theme::SURFACE,
            theme::SURFACE_OPACITY * opacity,
        )?;
        self.rounded_border(
            Rect::new(r.left + 1, r.top + 1, r.right - 1, r.bottom - 1),
            theme::RADIUS * scale - 1.,
            0xffffff,
            theme::BORDER_OPACITY * opacity,
            scale.max(1.),
        )
    }
    fn styled_format(&self, size: f32, style: TextStyle) -> Result<IDWriteTextFormat> {
        self.gfx.format(size, Some(style))
    }
    fn styled_layout(
        &self,
        s: &str,
        size: f32,
        style: TextStyle,
        width: f32,
        height: f32,
    ) -> Result<IDWriteTextLayout> {
        unsafe {
            let fmt = self.styled_format(size, style)?;
            let text: Vec<u16> = s.encode_utf16().collect();
            let layout =
                self.gfx
                    .text
                    .CreateTextLayout(&text, &fmt, width.max(1.), height.max(1.))?;
            if matches!(style, TextStyle::Semibold) {
                layout.SetTypography(
                    &self.gfx.tabular,
                    DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length: text.len() as u32,
                    },
                )?;
            }
            Ok(layout)
        }
    }
    pub fn text_size(&self, s: &str, size: f32, style: TextStyle) -> Result<(i32, i32)> {
        unsafe {
            let layout = self.styled_layout(s, size, style, 100000., 100000.)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics)?;
            Ok((
                metrics.widthIncludingTrailingWhitespace.ceil() as i32,
                metrics.height.ceil() as i32,
            ))
        }
    }
    pub fn label(&self, s: &str, r: Rect, size: f32, c: u32, style: TextStyle) -> Result<()> {
        unsafe {
            let layout = self.styled_layout(s, size, style, r.width() as f32, r.height() as f32)?;
            let brush = self.rt.CreateSolidColorBrush(&color(c, 1.), None)?;
            let mut e = D2D1_ELLIPSE::default();
            e.point.X = r.left as f32;
            e.point.Y = r.top as f32;
            self.rt.DrawTextLayout(
                e.point,
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
            Ok(())
        }
    }
    pub fn centered(&self, s: &str, r: Rect, size: f32, c: u32, style: TextStyle) -> Result<()> {
        let (width, _) = self.text_size(s, size, style)?;
        self.label(
            s,
            Rect::new(
                r.center().x - width / 2,
                r.top,
                r.center().x + (width + 1) / 2,
                r.bottom,
            ),
            size,
            c,
            style,
        )
    }
    pub fn rounded_image(
        &self,
        bitmap: &ID2D1Bitmap,
        dst: Rect,
        src: Option<Rect>,
        radius: f32,
        nearest: bool,
    ) -> Result<()> {
        unsafe {
            let geometry = self
                .gfx
                .factory
                .CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                    rect: rect(dst),
                    radiusX: radius,
                    radiusY: radius,
                })?;
            let mut params = D2D1_LAYER_PARAMETERS {
                contentBounds: rect(dst),
                geometricMask: std::mem::ManuallyDrop::new(Some(geometry.cast()?)),
                opacity: 1.,
                ..Default::default()
            };
            params.maskTransform.M11 = 1.;
            params.maskTransform.M22 = 1.;
            self.rt.PushLayer(&params, None);
            self.image(bitmap, dst, src, nearest);
            self.rt.PopLayer();
            std::mem::ManuallyDrop::drop(&mut params.geometricMask);
            Ok(())
        }
    }
    pub fn opacity(&self, r: Rect, opacity: f32, draw: impl FnOnce() -> Result<()>) -> Result<()> {
        if opacity >= 1. {
            return draw();
        }
        if opacity <= 0. {
            return Ok(());
        }
        unsafe {
            let mut params = D2D1_LAYER_PARAMETERS {
                contentBounds: rect(r),
                opacity: opacity.clamp(0., 1.),
                ..Default::default()
            };
            params.maskTransform.M11 = 1.;
            params.maskTransform.M22 = 1.;
            self.rt.PushLayer(&params, None);
            let result = draw();
            self.rt.PopLayer();
            result
        }
    }
}
pub fn dark_backdrop(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::*;
    unsafe {
        let dark = 1i32;
        let backdrop = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const i32).cast(),
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&backdrop as *const DWM_SYSTEMBACKDROP_TYPE).cast(),
            std::mem::size_of_val(&backdrop) as u32,
        );
    }
}

/// Called once by the control thread after COM initialization. No HWND is created.
pub fn warm_up() {
    let started = std::time::Instant::now();
    let result = (|| -> Result<()> {
        let gfx = Graphics::new()?;
        for size in [10., 11., 12., 14., 18.] {
            for style in [TextStyle::Body, TextStyle::Semibold, TextStyle::Mono] {
                let fmt = gfx.format(size, Some(style))?;
                unsafe {
                    let layout = gfx.text.CreateTextLayout(
                        &"Destello 0123456789".encode_utf16().collect::<Vec<_>>(),
                        &fmt,
                        500.,
                        80.,
                    )?;
                    let mut metrics = DWRITE_TEXT_METRICS::default();
                    layout.GetMetrics(&mut metrics)?;
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => crate::logging::info(&format!(
            "ui warm_up factories/fonts/icons: {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.
        )),
        Err(e) => crate::logging::warn(&format!("ui warm_up: {e}")),
    }
}
#[cfg(test)]
mod cache_tests {
    use super::*;
    // The eviction stress test must not evict a format during the identity assertion.
    static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());
    struct Com;
    impl Com {
        fn init() -> Self {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .expect("COM");
            }
            Self
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    #[test]
    fn factories_formats_and_icons_are_shared_across_threads() {
        let _guard = CACHE_TEST_LOCK.lock().expect("cache test lock");
        let _com = Com::init();
        let first = Graphics::new().expect("graphics");
        warm_up();
        let fmt = first.format(12., Some(TextStyle::Body)).expect("format");
        let expected = first.clone();
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let _com = Com::init();
                    let second = Graphics::new().expect("graphics");
                    assert!(Arc::ptr_eq(&expected, &second));
                    let again = second.format(12., Some(TextStyle::Body)).expect("format");
                    assert_eq!(fmt.as_raw(), again.as_raw());
                    assert_eq!(second.icons.len(), 18);
                    unsafe {
                        second
                            .wic
                            .resolve()
                            .expect("agile WIC")
                            .CreateBitmap(
                                2,
                                2,
                                &GUID_WICPixelFormat32bppPBGRA,
                                WICBitmapCacheOnLoad,
                            )
                            .expect("bitmap");
                    }
                })
                .join()
                .expect("worker");
        });
    }
    #[test]
    fn format_cache_has_a_fixed_memory_bound() {
        let _guard = CACHE_TEST_LOCK.lock().expect("cache test lock");
        let _com = Com::init();
        let gfx = Graphics::new().expect("graphics");
        for size in 1..=160 {
            gfx.format(size as f32, Some(TextStyle::Mono))
                .expect("format");
        }
        assert!(gfx.formats.lock().expect("cache").len() <= 128);
    }
}

//! Captura GDI del escritorio virtual, monitores y geometría de ventanas.
//!
//! Todo en píxeles físicos. El buffer resultante es B,G,R,A con alfa opaco.
use anyhow::{anyhow, Result};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use windows::core::{w, BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors,
    GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
    SRCCOPY,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement,
    TreeScope_Children, UIA_WindowControlTypeId,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_ESCAPE,
};
use windows::Win32::UI::Shell::{AppVisibility, IAppVisibility};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, EnumWindows, FindWindowExW, GetAncestor, GetClassNameW, GetCursorInfo,
    GetDesktopWindow, GetForegroundWindow, GetSystemMetrics, GetTopWindow, GetWindow,
    GetWindowLongW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, RealChildWindowFromPoint, WindowFromPoint, CURSORINFO, CURSOR_SHOWING,
    DI_NORMAL, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GW_HWNDNEXT, MONITORINFOF_PRIMARY,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::geom::{Point, Rect};

#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub rect: Rect,
    pub work: Rect,
    pub dpi: u32,
    pub primary: bool,
}

/// Tipo de superficie interactiva congelada en la captura.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapKind {
    Window,
    Menu,
    ShellFlyout,
    Taskbar,
}

/// Rectángulo (coordenadas de escritorio virtual) de una superficie capturada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapTarget {
    pub rect: Rect,
    pub kind: SnapKind,
}

/// Captura en coordenadas del escritorio virtual. bgra: B,G,R,A con A=255, stride = width*4.
#[derive(Clone, Debug)]
pub struct Screenshot {
    pub origin_x: i32,
    pub origin_y: i32,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub monitors: Vec<MonitorInfo>,
    /// Superficies visibles en orden Z en el instante inmediatamente posterior
    /// al blit: ventanas, menús, flyouts del shell y barra de tareas.
    pub targets: Vec<SnapTarget>,
}

/// Rectángulo del escritorio virtual completo (puede empezar en negativo).
pub fn virtual_screen_rect() -> Rect {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            Rect::new(0, 0, 1920, 1080)
        } else {
            Rect::new(x, y, x + w, y + h)
        }
    }
}

/// Captura el escritorio virtual completo.
pub fn capture_virtual_screen(include_cursor: bool) -> Result<Screenshot> {
    capture_rect(virtual_screen_rect(), include_cursor)
}

/// DIB section 32 bpp top-down reutilizada: BitBlt escribe directamente en `bits`.
struct CachedDib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl CachedDib {
    unsafe fn new(screen: HDC, width: i32, height: i32) -> Result<Self> {
        let dc = CreateCompatibleDC(Some(screen));
        if dc.0.is_null() {
            return Err(anyhow!("CreateCompatibleDC falló"));
        }

        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        // Altura negativa: DIB top-down, igual que el buffer RGBA.
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        info.bmiHeader.biSizeImage = (width as u32) * (height as u32) * 4;

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        {
            Ok(bitmap) => bitmap,
            Err(e) => {
                let _ = DeleteDC(dc);
                return Err(anyhow!("CreateDIBSection falló: {e}"));
            }
        };
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(dc);
            return Err(anyhow!("CreateDIBSection devolvió bits nulos"));
        }

        let old = SelectObject(dc, HGDIOBJ(bitmap.0));
        if old.0.is_null() {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(dc);
            return Err(anyhow!("SelectObject del DIB falló"));
        }

        Ok(Self {
            dc,
            bitmap,
            old,
            bits: bits.cast::<u8>(),
            width,
            height,
        })
    }
}

impl Drop for CachedDib {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

thread_local! {
    static DIB_CACHE: RefCell<Option<CachedDib>> = const { RefCell::new(None) };
}

/// DC del escritorio con liberación garantizada.
struct ScreenDc(HDC);

impl ScreenDc {
    fn acquire() -> Result<Self> {
        let hdc = unsafe { GetDC(None) };
        if hdc.0.is_null() {
            Err(anyhow!("GetDC del escritorio falló"))
        } else {
            Ok(Self(hdc))
        }
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

/// Captura una región física del escritorio virtual (recortada a la pantalla).
pub fn capture_rect(r: Rect, include_cursor: bool) -> Result<Screenshot> {
    let total_start = Instant::now();
    let rect = normalize(r).intersect(virtual_screen_rect());
    let w = rect.width();
    let h = rect.height();
    if w <= 0 || h <= 0 {
        return Err(anyhow!("área de captura vacía"));
    }

    let screen = ScreenDc::acquire()?;
    let mut buffer: Vec<u8> = Vec::new();
    let mut targets: Vec<SnapTarget> = Vec::new();
    let mut blit_ms = 0.0f64;
    let mut copy_ms = 0.0f64;
    let mut alpha_ms = 0.0f64;

    DIB_CACHE.with(|cache| -> Result<()> {
        let mut cache = cache.borrow_mut();
        let needs_new = cache
            .as_ref()
            .is_none_or(|dib| dib.width != w || dib.height != h);
        if needs_new {
            let dib = unsafe { CachedDib::new(screen.0, w, h)? };
            *cache = Some(dib);
        }
        let dib = cache
            .as_mut()
            .ok_or_else(|| anyhow!("caché DIB vacía"))?;

        // BitBlt directo al DIB (sin GetDIBits y sin CAPTUREBLT: DWM ya compone).
        let blit_start = Instant::now();
        let blit = unsafe {
            BitBlt(
                dib.dc,
                0,
                0,
                w,
                h,
                Some(screen.0),
                rect.left,
                rect.top,
                SRCCOPY,
            )
        };
        if include_cursor && blit.is_ok() {
            unsafe {
                draw_cursor(dib.dc, rect.left, rect.top);
            }
        }
        blit_ms = blit_start.elapsed().as_secs_f64() * 1000.0;
        if blit.is_err() {
            return Err(anyhow!("BitBlt falló al capturar la pantalla"));
        }

        // Justo después del blit y antes de tocar el buffer: la lista de
        // superficies debe describir exactamente la imagen congelada.
        targets = snapshot_targets();

        let copy_start = Instant::now();
        let len = (w as usize) * (h as usize) * 4;
        buffer = vec![0u8; len];
        unsafe {
            std::ptr::copy_nonoverlapping(dib.bits, buffer.as_mut_ptr(), len);
        }
        copy_ms = copy_start.elapsed().as_secs_f64() * 1000.0;

        let alpha_start = Instant::now();
        set_alpha_opaque(&mut buffer);
        alpha_ms = alpha_start.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    })?;

    let monitors_start = Instant::now();
    let monitors = monitors();
    let monitors_ms = monitors_start.elapsed().as_secs_f64() * 1000.0;

    let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;
    let message = format!(
        "capture_rect {w}x{h}: blit {blit_ms:.2} ms, copy {copy_ms:.2} ms, alpha {alpha_ms:.2} ms, monitores {monitors_ms:.2} ms, total {total_ms:.2} ms"
    );
    if total_ms > 40.0 {
        crate::logging::warn(&message);
    } else {
        crate::logging::info(&message);
    }

    Ok(Screenshot {
        origin_x: rect.left,
        origin_y: rect.top,
        width: w as u32,
        height: h as u32,
        bgra: buffer,
        monitors,
        targets,
    })
}

/// Marca el canal alfa como opaco sin tocar BGR.
fn set_alpha_opaque(buffer: &mut [u8]) {
    // En Windows (little endian) el byte alto de cada u32 es el alfa.
    {
        let (prefix, words, suffix) = unsafe { buffer.align_to_mut::<u32>() };
        for word in words.iter_mut() {
            *word |= 0xFF00_0000;
        }
        if prefix.is_empty() && suffix.is_empty() {
            return;
        }
    }
    // Fallback si el buffer no estaba alineado a 4 (no ocurre con Vec<u8> normal).
    for px in buffer.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
}

unsafe fn draw_cursor(hdc: HDC, origin_x: i32, origin_y: i32) {
    let mut cursor = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    if GetCursorInfo(&mut cursor).is_ok() && (cursor.flags.0 & CURSOR_SHOWING.0) != 0 {
        let x = cursor.ptScreenPos.x - origin_x;
        let y = cursor.ptScreenPos.y - origin_y;
        let _ = DrawIconEx(
            hdc,
            x,
            y,
            windows::Win32::UI::WindowsAndMessaging::HICON(cursor.hCursor.0),
            0,
            0,
            0,
            None,
            DI_NORMAL,
        );
    }
}

/// Monitores conectados, en píxeles físicos.
pub fn monitors() -> Vec<MonitorInfo> {
    let mut out: Vec<MonitorInfo> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_cb),
            LPARAM(&mut out as *mut Vec<MonitorInfo> as isize),
        );
    }
    if out.is_empty() {
        let r = virtual_screen_rect();
        out.push(MonitorInfo {
            rect: r,
            work: r,
            dpi: 96,
            primary: true,
        });
    }
    out
}

unsafe extern "system" fn monitor_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<MonitorInfo>);
    if let Some(info) = monitor_info(hmon) {
        out.push(info);
    }
    BOOL(1)
}

unsafe fn monitor_info(hmon: HMONITOR) -> Option<MonitorInfo> {
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !GetMonitorInfoW(hmon, &mut mi.monitorInfo).as_bool() {
        return None;
    }
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    Some(MonitorInfo {
        rect: rect_from(mi.monitorInfo.rcMonitor),
        work: rect_from(mi.monitorInfo.rcWork),
        dpi: dpi_x.max(48),
        primary: (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0,
    })
}

/// Monitor más cercano al punto (siempre devuelve uno).
pub fn monitor_at(pt: Point) -> MonitorInfo {
    unsafe {
        let hmon = MonitorFromPoint(
            POINT { x: pt.x, y: pt.y },
            MONITOR_DEFAULTTONEAREST,
        );
        if !hmon.0.is_null() {
            if let Some(info) = monitor_info(hmon) {
                return info;
            }
        }
    }
    monitors()
        .into_iter()
        .find(|m| m.primary)
        .or_else(|| monitors().into_iter().next())
        .unwrap_or(MonitorInfo {
            rect: virtual_screen_rect(),
            work: virtual_screen_rect(),
            dpi: 96,
            primary: true,
        })
}

/// Rectángulo visible (DWM extended frame bounds) o el clásico si DWM falla.
pub fn window_rect(hwnd: HWND) -> Option<Rect> {
    unsafe {
        let mut rect = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut rect as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_ok()
        {
            let dwm = rect_from(rect);
            if !dwm.is_empty() {
                return Some(dwm);
            }
        }
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            return Some(rect_from(rect));
        }
    }
    None
}

/// Tamaño mínimo de una ventana de aplicación capturable.
const MIN_WINDOW_SIZE: i32 = 40;

/// Datos objetivos de una ventana. El filtro no toca Win32: es testeable.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WindowSnapshot {
    class: String,
    title: String,
    /// Ejecutable del proceso (solo se consulta para CoreWindow del shell).
    process: Option<String>,
    visible: bool,
    minimized: bool,
    cloaked: bool,
    tool_window: bool,
    no_activate: bool,
    transparent: bool,
    popup_style: bool,
    rect: Rect,
}

fn is_shell_class(class: &str) -> bool {
    matches!(
        class,
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "Progman"
            | "WorkerW"
            | "Windows.UI.Core.CoreWindow"
    )
}

/// Filtro puro: ventana de aplicación real, visible y con tamaño suficiente.
fn is_capturable(snapshot: &WindowSnapshot) -> bool {
    snapshot.visible
        && !snapshot.minimized
        && !snapshot.cloaked
        && !snapshot.tool_window
        && !snapshot.no_activate
        && !snapshot.transparent
        && !snapshot.class.is_empty()
        && !is_shell_class(&snapshot.class)
        && !snapshot.class.starts_with("Destello")
        && snapshot.rect.width() >= MIN_WINDOW_SIZE
        && snapshot.rect.height() >= MIN_WINDOW_SIZE
}

const MIN_TARGET_SIZE: i32 = 8;

fn is_taskbar_class(class: &str) -> bool {
    matches!(class, "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

fn is_core_window(class: &str) -> bool {
    class == "Windows.UI.Core.CoreWindow"
}

fn shell_flyout_name_for_process(process: Option<&str>) -> Option<&'static str> {
    let process = process?;
    if process.eq_ignore_ascii_case("StartMenuExperienceHost.exe") {
        Some("Inicio")
    } else if process.eq_ignore_ascii_case("SearchHost.exe") {
        Some("Búsqueda")
    } else if process.eq_ignore_ascii_case("ShellExperienceHost.exe") {
        Some("Configuración rápida/notificaciones")
    } else {
        None
    }
}

fn is_shell_flyout_process(process: Option<&str>) -> bool {
    shell_flyout_name_for_process(process).is_some()
}

fn is_menu_class(class: &str) -> bool {
    matches!(
        class,
        "#32768" | "Xaml_WindowedPopupClass" | "Microsoft.UI.Content.PopupWindowSiteBridge"
    )
}

/// Resultado puro del filtro, con motivo de descarte para el diagnóstico.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapVerdict {
    Target(SnapKind),
    Reject(&'static str),
}

fn snap_verdict(snapshot: &WindowSnapshot, desktop: Rect) -> SnapVerdict {
    if !snapshot.visible {
        return SnapVerdict::Reject("invisible");
    }
    if snapshot.minimized {
        return SnapVerdict::Reject("minimizada");
    }
    if snapshot.cloaked {
        return SnapVerdict::Reject("cloaked");
    }
    if snapshot.transparent {
        return SnapVerdict::Reject("WS_EX_TRANSPARENT");
    }
    if snapshot.class.is_empty() {
        return SnapVerdict::Reject("clase vacía");
    }
    if snapshot.class.starts_with("Destello") {
        return SnapVerdict::Reject("ventana propia de Destello");
    }
    if snapshot.class == "SysShadow" {
        return SnapVerdict::Reject("SysShadow");
    }
    if matches!(snapshot.class.as_str(), "Progman" | "WorkerW") {
        return SnapVerdict::Reject("escritorio del shell");
    }

    let rect = snapshot.rect.intersect(desktop);
    if rect.width() < MIN_TARGET_SIZE || rect.height() < MIN_TARGET_SIZE {
        return SnapVerdict::Reject("recorte <8x8 o fuera de pantalla");
    }

    if is_taskbar_class(&snapshot.class) {
        return SnapVerdict::Target(SnapKind::Taskbar);
    }
    if is_core_window(&snapshot.class) {
        return if is_shell_flyout_process(snapshot.process.as_deref()) {
            SnapVerdict::Target(SnapKind::ShellFlyout)
        } else {
            // CoreWindow de apps UWP: la ventana de aplicación es su host.
            SnapVerdict::Reject("CoreWindow no pertenece al shell")
        };
    }
    if is_menu_class(&snapshot.class)
        || snapshot.tool_window
        || snapshot.no_activate
        || snapshot.popup_style
    {
        return SnapVerdict::Target(SnapKind::Menu);
    }
    if snapshot.rect.width() < MIN_WINDOW_SIZE || snapshot.rect.height() < MIN_WINDOW_SIZE {
        return SnapVerdict::Reject("ventana <40x40");
    }
    if is_capturable(snapshot) {
        SnapVerdict::Target(SnapKind::Window)
    } else {
        SnapVerdict::Reject("no supera el filtro de ventana de aplicación")
    }
}

fn verdict_text(verdict: SnapVerdict) -> String {
    match verdict {
        SnapVerdict::Target(kind) => format!("TARGET({kind:?})"),
        SnapVerdict::Reject(reason) => format!("RECHAZADA({reason})"),
    }
}

/// Filtro + clasificación puros de una superficie capturada.
fn classify_snap(snapshot: &WindowSnapshot, desktop: Rect) -> Option<SnapTarget> {
    match snap_verdict(snapshot, desktop) {
        SnapVerdict::Target(kind) => Some(SnapTarget {
            rect: snapshot.rect.intersect(desktop),
            kind,
        }),
        SnapVerdict::Reject(_) => None,
    }
}

fn is_xaml_class(class: &str) -> bool {
    class.eq_ignore_ascii_case("Xaml_WindowedPopupClass")
        || class.eq_ignore_ascii_case("Microsoft.UI.Content.PopupWindowSiteBridge")
        || class.to_ascii_lowercase().contains("xaml")
}

/// Regla para raíces que WindowFromPoint ve pero EnumWindows no: banda Z
/// superior del shell, por encima del overlay.
fn upper_band_verdict(snapshot: &WindowSnapshot, desktop: Rect) -> SnapVerdict {
    if !snapshot.visible {
        return SnapVerdict::Reject("invisible");
    }
    if snapshot.minimized {
        return SnapVerdict::Reject("minimizada");
    }
    if snapshot.cloaked {
        return SnapVerdict::Reject("cloaked");
    }
    if snapshot.transparent {
        return SnapVerdict::Reject("WS_EX_TRANSPARENT");
    }
    if snapshot.class.is_empty() {
        return SnapVerdict::Reject("clase vacía");
    }
    if snapshot.class.starts_with("Destello") {
        return SnapVerdict::Reject("ventana propia de Destello");
    }
    if matches!(snapshot.class.as_str(), "SysShadow" | "Progman" | "WorkerW") {
        return SnapVerdict::Reject("shell no interactiva");
    }

    let rect = snapshot.rect.intersect(desktop);
    if rect.width() < MIN_TARGET_SIZE || rect.height() < MIN_TARGET_SIZE {
        return SnapVerdict::Reject("recorte <8x8 o fuera de pantalla");
    }

    if is_taskbar_class(&snapshot.class) {
        SnapVerdict::Target(SnapKind::Taskbar)
    } else if is_core_window(&snapshot.class)
        || is_xaml_class(&snapshot.class)
        || is_shell_flyout_process(snapshot.process.as_deref())
    {
        SnapVerdict::Target(SnapKind::ShellFlyout)
    } else {
        SnapVerdict::Target(SnapKind::Menu)
    }
}

fn rect_area(rect: Rect) -> i64 {
    rect.width().max(0) as i64 * rect.height().max(0) as i64
}

/// El CoreWindow del Inicio es transparente y ocupa toda el área de trabajo;
/// solo se sustituye por paneles UIA cuando es un flyout a pantalla completa.
fn covers_almost_all(rect: Rect, desktop: Rect, work_areas: &[Rect]) -> bool {
    let screens = std::iter::once(desktop).chain(work_areas.iter().copied());
    screens.into_iter().any(|screen| {
        let screen_area = rect_area(screen);
        if screen_area <= 0 {
            return false;
        }
        let visible = rect_area(rect.intersect(screen));
        visible * 10 >= screen_area * 9
    })
}

fn segment_rect_candidate(rect: Rect, window_rect: Rect) -> Option<Rect> {
    let clipped = rect.intersect(window_rect);
    if clipped.width() < 8 || clipped.height() < 8 {
        return None;
    }
    if rect.width() >= window_rect.width().saturating_sub(8)
        && rect.height() >= window_rect.height().saturating_sub(8)
    {
        return None;
    }
    Some(clipped)
}

fn panel_rect_candidate(rect: Rect, window_rect: Rect, window_area: i64) -> Option<Rect> {
    let clipped = rect.intersect(window_rect);
    if clipped.width() < 40 || clipped.height() < 40 {
        return None;
    }
    // Un hijo que ocupa casi toda la ventana es el contenedor transparente,
    // no un panel real.
    if rect.width() >= window_rect.width().saturating_sub(8)
        && rect.height() >= window_rect.height().saturating_sub(8)
    {
        return None;
    }
    // Al menos un 2% de la ventana: descarta barras y botones internos.
    if rect_area(clipped) * 50 < window_area {
        return None;
    }
    Some(clipped)
}

fn rect_contains(outer: Rect, inner: Rect) -> bool {
    inner.left >= outer.left
        && inner.top >= outer.top
        && inner.right <= outer.right
        && inner.bottom <= outer.bottom
}

fn dedupe_panels(mut panels: Vec<Rect>) -> Vec<Rect> {
    panels.sort_by_key(|rect| std::cmp::Reverse(rect_area(*rect)));
    let mut kept: Vec<Rect> = Vec::new();
    for rect in panels {
        if kept.iter().any(|kept| rect_contains(*kept, rect)) {
            continue;
        }
        kept.push(rect);
    }
    kept.sort_by_key(|rect| rect.left);
    kept
}

fn panels_from_segments(
    segments: Vec<Rect>,
    window_rect: Rect,
    window_area: i64,
) -> Vec<Rect> {
    let anchors: Vec<Rect> = segments
        .iter()
        .copied()
        .filter_map(|rect| panel_rect_candidate(rect, window_rect, window_area))
        .collect();
    if anchors.is_empty() {
        return Vec::new();
    }

    // Cada panel se compone de su cuerpo grande + controles de su columna
    // (buscador, barra de navegación, botón de móvil…). Se reconstruye el
    // rectángulo visible del panel, no solo el listado interior.
    let mut groups: Vec<Rect> = Vec::new();
    for anchor in &anchors {
        let mut group = *anchor;
        for segment in &segments {
            let center_x = segment.left + segment.width() / 2;
            if center_x >= anchor.left && center_x <= anchor.right {
                group = union_rect(group, *segment);
            }
        }
        groups.push(group);
    }

    const MARGIN: i32 = 16;
    let top = groups
        .iter()
        .map(|rect| rect.top)
        .min()
        .unwrap_or(window_rect.top)
        .saturating_sub(MARGIN)
        .max(window_rect.top);
    let bottom = groups
        .iter()
        .map(|rect| rect.bottom)
        .max()
        .unwrap_or(window_rect.bottom)
        .saturating_add(MARGIN)
        .min(window_rect.bottom);

    let panels: Vec<Rect> = groups
        .into_iter()
        .map(|rect| Rect::new(rect.left, top, rect.right, bottom))
        .filter(|rect| {
            rect.width() < window_rect.width().saturating_sub(8)
                || rect.height() < window_rect.height().saturating_sub(8)
        })
        .collect();
    dedupe_panels(panels)
}

fn uia_panels(
    hwnd: HWND,
    window_rect: Rect,
    label: &str,
    deadline: Instant,
) -> Vec<Rect> {
    let started = Instant::now();
    let result = (|| -> Option<Vec<Rect>> {
        let automation: IUIAutomation = unsafe {
            CoCreateInstance(
                &CUIAutomation,
                None::<&windows::core::IUnknown>,
                CLSCTX_ALL,
            )
            .ok()?
        };
        let root = unsafe { automation.ElementFromHandle(hwnd) }.ok()?;
        let condition = unsafe { automation.CreateTrueCondition() }.ok()?;

        let window_area = rect_area(window_rect);
        let mut direct = Vec::new();
        collect_segments(&root, &condition, window_rect, deadline, &mut direct);
        let panels = panels_from_segments(direct, window_rect, window_area);
        if !panels.is_empty() {
            return Some(panels);
        }
        if Instant::now() >= deadline {
            return Some(Vec::new());
        }

        // Un nivel más por si el primer hijo es un contenedor a pantalla
        // completa (el panel real queda dentro).
        let mut nested = Vec::new();
        let Ok(children) = (unsafe { root.FindAll(TreeScope_Children, &condition) }) else {
            return Some(Vec::new());
        };
        let Ok(count) = (unsafe { children.Length() }) else {
            return Some(Vec::new());
        };
        for index in 0..count.min(32) {
            if Instant::now() >= deadline {
                break;
            }
            let Ok(child) = (unsafe { children.GetElement(index) }) else {
                continue;
            };
            collect_segments(
                &child,
                &condition,
                window_rect,
                deadline,
                &mut nested,
            );
        }
        Some(panels_from_segments(nested, window_rect, window_area))
    })();

    let panels = result.unwrap_or_default();
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    if panels.is_empty() {
        crate::logging::warn(&format!(
            "uia flyout {label}: sin paneles tras {elapsed:.1} ms; se descarta el rect de pantalla completa"
        ));
    } else {
        crate::logging::info(&format!(
            "uia flyout {label}: {} paneles en {elapsed:.1} ms: {:?}",
            panels.len(),
            panels
        ));
    }
    panels
}

fn collect_segments(
    parent: &IUIAutomationElement,
    condition: &IUIAutomationCondition,
    window_rect: Rect,
    deadline: Instant,
    out: &mut Vec<Rect>,
) {
    if Instant::now() >= deadline {
        return;
    }
    let Ok(array) = (unsafe { parent.FindAll(TreeScope_Children, condition) }) else {
        return;
    };
    let Ok(count) = (unsafe { array.Length() }) else {
        return;
    };
    for index in 0..count.min(64) {
        if Instant::now() >= deadline {
            return;
        }
        let Ok(element) = (unsafe { array.GetElement(index) }) else {
            continue;
        };
        let kind = unsafe { element.CurrentControlType() }.ok();
        // Un hijo de tipo Window es otra ventana/flyout anidado (p. ej. el
        // buscador), no un panel del Inicio.
        if kind.is_some_and(|kind| kind == UIA_WindowControlTypeId) {
            continue;
        }
        if unsafe { element.CurrentIsOffscreen() }
            .map(|offscreen| offscreen.as_bool())
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(rect) = (unsafe { element.CurrentBoundingRectangle() }) else {
            continue;
        };
        let rect = Rect::new(rect.left, rect.top, rect.right, rect.bottom);
        if let Some(segment) = segment_rect_candidate(rect, window_rect) {
            out.push(segment);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ClassifiedTarget {
    target: SnapTarget,
    process: Option<String>,
}

fn same_process(a: &Option<String>, b: &Option<String>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

fn same_vertical_edges(a: Rect, b: Rect) -> bool {
    (a.left - b.left).abs() <= 2 && (a.right - b.right).abs() <= 2
}

fn vertical_gap(a: Rect, b: Rect) -> i32 {
    if a.bottom <= b.top {
        b.top - a.bottom
    } else if b.bottom <= a.top {
        a.top - b.bottom
    } else {
        0
    }
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.min(b.left),
        a.top.min(b.top),
        a.right.max(b.right),
        a.bottom.max(b.bottom),
    )
}

/// Los menús contextuales de Win11 pueden ser dos popups apilados del mismo
/// proceso (tira superior + cuerpo). Se fusionan en un único target vertical.
/// Los submenús en cascada quedan al lado y no cumplen el mismo ancho.
fn merge_split_menus(mut items: Vec<ClassifiedTarget>) -> Vec<ClassifiedTarget> {
    let mut index = 0;
    while index < items.len() {
        if items[index].target.kind != SnapKind::Menu {
            index += 1;
            continue;
        }

        let mut merged_any = true;
        while merged_any {
            merged_any = false;
            let mut other = index + 1;
            while other < items.len() {
                if items[other].target.kind == SnapKind::Menu
                    && same_process(&items[index].process, &items[other].process)
                    && same_vertical_edges(items[index].target.rect, items[other].target.rect)
                    && vertical_gap(items[index].target.rect, items[other].target.rect) <= 2
                {
                    items[index].target.rect =
                        union_rect(items[index].target.rect, items[other].target.rect);
                    items.remove(other);
                    merged_any = true;
                    break;
                }
                other += 1;
            }
        }
        index += 1;
    }
    items
}

struct UpperBandRoot {
    hwnd: HWND,
    snapshot: WindowSnapshot,
    verdict: SnapVerdict,
    origin: &'static str,
}

fn sample_window_from_point(desktop: Rect) -> Vec<isize> {
    let columns = (desktop.width().max(0) / 32 + 1) as usize;
    let rows = (desktop.height().max(0) / 32 + 1) as usize;
    let mut raw: Vec<isize> = Vec::with_capacity(columns * rows);
    let desktop_hwnd = unsafe { GetDesktopWindow() };
    let desktop_rect = {
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(desktop_hwnd, &mut rect) }.is_ok() {
            Some(rect_from(rect))
        } else {
            None
        }
    };

    let mut y = desktop.top;
    while y < desktop.bottom {
        let mut x = desktop.left;
        while x < desktop.right {
            let point = POINT { x, y };
            // RealChildWindowFromPoint(desktop) resuelve la ventana top-level
            // (incluida la banda superior del shell) sin bajar a hijos. Para
            // puntos fuera del rect del escritorio (multi-monitor) se usa
            // WindowFromPoint.
            let mut hwnd = if desktop_rect.is_none_or(|rect| rect.contains(Point { x, y })) {
                unsafe { RealChildWindowFromPoint(desktop_hwnd, point) }
            } else {
                HWND::default()
            };
            if hwnd.0.is_null() {
                hwnd = unsafe { WindowFromPoint(point) };
            }
            if !hwnd.0.is_null() {
                raw.push(hwnd.0 as isize);
            }
            x = x.saturating_add(32);
        }
        y = y.saturating_add(32);
    }
    raw
}

/// Muestrea una rejilla de 32 px. Usa `RealChildWindowFromPoint` sobre el
/// escritorio como sonda rápida de la raíz top-level (también ve la banda Z
/// superior) y `WindowFromPoint` como fallback. Las raíces que EnumWindows no
/// ve (Inicio, flyouts, taskbar) se añaden arriba del todo.
fn upper_band_roots(known: &HashSet<isize>, desktop: Rect) -> (Vec<UpperBandRoot>, f64, f64) {
    let sampling_start = Instant::now();
    let raw = sample_window_from_point(desktop);
    let sampling_ms = sampling_start.elapsed().as_secs_f64() * 1000.0;

    let classify_start = Instant::now();
    let mut root_cache: HashMap<isize, Option<isize>> = HashMap::with_capacity(64);
    let mut seen: HashSet<isize> = HashSet::with_capacity(16);
    let mut candidate_keys: Vec<isize> = Vec::new();
    for direct in raw {
        let candidate = if let Some(cached) = root_cache.get(&direct) {
            *cached
        } else {
            let root = unsafe {
                GetAncestor(
                    HWND(direct as *mut core::ffi::c_void),
                    GA_ROOT,
                )
            };
            let key = (!root.0.is_null()).then_some(root.0 as isize);
            let candidate = key.filter(|key| !known.contains(key));
            root_cache.insert(direct, candidate);
            candidate
        };
        if let Some(key) = candidate {
            if seen.insert(key) {
                candidate_keys.push(key);
            }
        }
    }

    let mut roots: Vec<UpperBandRoot> = Vec::new();
    for key in candidate_keys {
        let root = HWND(key as *mut core::ffi::c_void);
        if let Some(snapshot) = unsafe { window_snapshot_all(root) } {
            let verdict = upper_band_verdict(&snapshot, desktop);
            roots.push(UpperBandRoot {
                hwnd: root,
                snapshot,
                verdict,
                origin: "WindowFromPoint",
            });
        }
    }

    // La barra de tareas puede no aparecer ni en EnumWindows ni en la rejilla.
    for class in [w!("Shell_TrayWnd"), w!("Shell_SecondaryTrayWnd")] {
        let Ok(found) = (unsafe { FindWindowExW(None, None, class, PCWSTR::null()) }) else {
            continue;
        };
        if found.0.is_null() {
            continue;
        }
        let key = found.0 as isize;
        if known.contains(&key) || !seen.insert(key) {
            continue;
        }
        if let Some(snapshot) = unsafe { window_snapshot_all(found) } {
            let verdict = upper_band_verdict(&snapshot, desktop);
            roots.push(UpperBandRoot {
                hwnd: found,
                snapshot,
                verdict,
                origin: "FindWindowEx",
            });
        }
    }
    let classify_ms = classify_start.elapsed().as_secs_f64() * 1000.0;

    let work_areas: Vec<Rect> = monitors().into_iter().map(|monitor| monitor.work).collect();
    let mut processed: Vec<UpperBandRoot> = Vec::new();
    for root in roots {
        let full_screen_flyout = matches!(
            root.verdict,
            SnapVerdict::Target(SnapKind::ShellFlyout)
        ) && covers_almost_all(root.snapshot.rect, desktop, &work_areas);
        if !full_screen_flyout {
            processed.push(root);
            continue;
        }

        // UIA solo se consulta para flyouts a pantalla completa (Inicio).
        let deadline = Instant::now() + Duration::from_millis(60);
        let panels = uia_panels(
            root.hwnd,
            root.snapshot.rect,
            &root.snapshot.class,
            deadline,
        );
        if panels.is_empty() {
            let mut rejected = root;
            rejected.origin = "UIA";
            rejected.verdict = SnapVerdict::Reject("UIA sin paneles");
            processed.push(rejected);
        } else {
            for panel in panels {
                let mut snapshot = root.snapshot.clone();
                snapshot.rect = panel;
                processed.push(UpperBandRoot {
                    hwnd: root.hwnd,
                    snapshot,
                    verdict: SnapVerdict::Target(SnapKind::ShellFlyout),
                    origin: "UIA",
                });
            }
        }
    }
    let roots = processed;

    let message = format!(
        "targets banda superior: muestreo32={sampling_ms:.2} ms, clasificacion={classify_ms:.2} ms, raices={}",
        roots.len()
    );
    if sampling_ms > 5.0 {
        crate::logging::warn(&message);
    } else {
        crate::logging::info(&message);
    }
    (roots, sampling_ms, classify_ms)
}

/// Superficies visibles en orden Z (EnumWindows va de arriba a abajo).
pub fn snapshot_targets() -> Vec<SnapTarget> {
    let desktop = virtual_screen_rect();
    let mut known: HashSet<isize> = HashSet::new();
    let mut out: Vec<ClassifiedTarget> = Vec::new();
    {
        let mut ctx = TargetsCtx {
            desktop,
            out: &mut out,
            known: &mut known,
        };
        unsafe {
            let _ = EnumWindows(
                Some(enum_target_cb),
                LPARAM(&mut ctx as *mut TargetsCtx<'_> as isize),
            );
        }
    }

    let (roots, _sampling_ms, _classify_ms) = upper_band_roots(&known, desktop);
    let upper: Vec<ClassifiedTarget> = roots
        .into_iter()
        .filter_map(|root| match root.verdict {
            SnapVerdict::Target(kind) => Some(ClassifiedTarget {
                target: SnapTarget {
                    rect: root.snapshot.rect.intersect(desktop),
                    kind,
                },
                process: root.snapshot.process,
            }),
            SnapVerdict::Reject(_) => None,
        })
        .collect();

    let lower = merge_split_menus(out);
    let upper = merge_split_menus(upper);
    let mut merged = upper;
    merged.extend(lower);
    merged.into_iter().map(|item| item.target).collect()
}

struct TargetsCtx<'a> {
    desktop: Rect,
    out: &'a mut Vec<ClassifiedTarget>,
    known: &'a mut HashSet<isize>,
}

unsafe extern "system" fn enum_target_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut TargetsCtx<'_>);
    ctx.known.insert(hwnd.0 as isize);
    if let Some(snapshot) = window_snapshot(hwnd) {
        if let Some(target) = classify_snap(&snapshot, ctx.desktop) {
            ctx.out.push(ClassifiedTarget {
                target,
                process: snapshot.process,
            });
        }
    }
    BOOL(1)
}

fn format_rect(rect: Option<Rect>) -> String {
    match rect {
        Some(rect) => format!(
            "{},{},{},{}",
            rect.left, rect.top, rect.right, rect.bottom
        ),
        None => "?".to_string(),
    }
}

fn debug_window_line(
    hwnd: HWND,
    index: usize,
    desktop: Rect,
    origin: &str,
    verdict: SnapVerdict,
    rect_override: Option<Rect>,
) -> String {
    let class = unsafe { window_class(hwnd) };
    let title = unsafe { window_title(hwnd) };
    let process = unsafe { process_exe_name(hwnd) };
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 };
    let cloaked_raw = unsafe { cloaked_value(hwnd) };
    let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    let minimized = unsafe { IsIconic(hwnd).as_bool() };
    let rect = rect_override.or_else(|| window_rect(hwnd));
    let clip = rect.map(|rect| rect.intersect(desktop));

    format!(
        "[{index:03}] origen={origin} hwnd=0x{:08X} clase={:?} proceso={:?} titulo={:?} ex=0x{:08X} cloaked={}(0x{:X}) visible={} min={} rect={} clip={} veredicto={}",
        hwnd.0 as usize,
        class,
        process.as_deref().unwrap_or("?"),
        title,
        ex,
        cloaked_raw != 0,
        cloaked_raw,
        visible,
        minimized,
        format_rect(rect),
        format_rect(clip),
        verdict_text(verdict)
    )
}

/// Informe de diagnóstico para `destello debug-targets`: todas las ventanas
/// top-level en orden Z, sus datos y el veredicto del filtro con motivo.
pub fn debug_targets_report() -> String {
    struct Ctx {
        desktop: Rect,
        lines: Vec<String>,
        known: HashSet<isize>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        ctx.known.insert(hwnd.0 as isize);
        let verdict = unsafe { window_snapshot(hwnd) }
            .map(|snapshot| snap_verdict(&snapshot, ctx.desktop))
            .unwrap_or(SnapVerdict::Reject("sin rect DWM"));
        let index = ctx.lines.len();
        ctx.lines.push(debug_window_line(
            hwnd,
            index,
            ctx.desktop,
            "EnumWindows",
            verdict,
            None,
        ));
        BOOL(1)
    }

    let desktop = virtual_screen_rect();
    let mut ctx = Ctx {
        desktop,
        lines: Vec::new(),
        known: HashSet::new(),
    };
    unsafe {
        let _ = EnumWindows(
            Some(cb),
            LPARAM(&mut ctx as *mut Ctx as isize),
        );
    }

    let enum_count = ctx.lines.len();
    let (roots, sampling_ms, classify_ms) = upper_band_roots(&ctx.known, desktop);
    let upper_count = roots.len();
    for root in roots {
        let index = ctx.lines.len();
        ctx.lines.push(debug_window_line(
            root.hwnd,
            index,
            desktop,
            root.origin,
            root.verdict,
            Some(root.snapshot.rect),
        ));
    }

    let mut out = String::new();
    out.push_str("Destello debug-targets\n");
    out.push_str(&format!(
        "escritorio={} (izquierda,arriba,derecha,abajo)\n",
        format_rect(Some(desktop))
    ));
    out.push_str(&format!(
        "ventanas EnumWindows={enum_count}; raices banda superior={upper_count}; muestreo32={sampling_ms:.2} ms; clasificacion={classify_ms:.2} ms\n"
    ));
    out.push_str("orden: EnumWindows (arriba -> abajo) y después banda superior\n");
    for line in ctx.lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Cierra el flyout del shell visible (Inicio, búsqueda, configuración rápida)
/// enviando Esc y esperando hasta 400 ms a que desaparezca. La captura ya está
/// congelada, así que el flyout sigue en la imagen.
pub fn dismiss_shell_flyouts() {
    let Some(name) = visible_shell_flyout_name() else {
        return;
    };

    let started = Instant::now();
    if !send_escape() {
        crate::logging::warn(&format!("no se pudo enviar Esc para cerrar el flyout {name}"));
        return;
    }

    let deadline = Duration::from_millis(400);
    loop {
        if visible_shell_flyout_name().is_none() {
            crate::logging::info(&format!(
                "flyout del shell cerrado: {name} en {} ms",
                started.elapsed().as_millis()
            ));
            return;
        }
        if started.elapsed() >= deadline {
            crate::logging::warn(&format!(
                "el flyout del shell {name} no se cerró tras {} ms",
                deadline.as_millis()
            ));
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn launcher_visible() -> bool {
    unsafe {
        let Ok(visibility) = CoCreateInstance::<_, IAppVisibility>(
            &AppVisibility,
            None::<&windows::core::IUnknown>,
            CLSCTX_ALL,
        ) else {
            return false;
        };
        visibility
            .IsLauncherVisible()
            .map(|visible| visible.as_bool())
            .unwrap_or(false)
    }
}

fn visible_shell_flyout_name() -> Option<&'static str> {
    if launcher_visible() {
        return Some("Inicio");
    }

    struct Ctx {
        desktop: Rect,
        found: Option<&'static str>,
    }
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        if ctx.found.is_some() {
            return BOOL(0);
        }
        if !IsWindowVisible(hwnd).as_bool()
            || IsIconic(hwnd).as_bool()
            || is_cloaked(hwnd)
            || window_class(hwnd) != "Windows.UI.Core.CoreWindow"
        {
            return BOOL(1);
        }
        let Some(name) = shell_flyout_name_for_process(process_exe_name(hwnd).as_deref()) else {
            return BOOL(1);
        };
        let Some(rect) = window_rect(hwnd) else {
            return BOOL(1);
        };
        let rect = rect.intersect(ctx.desktop);
        if rect.width() < MIN_TARGET_SIZE || rect.height() < MIN_TARGET_SIZE {
            return BOOL(1);
        }
        ctx.found = Some(name);
        BOOL(0)
    }

    let mut ctx = Ctx {
        desktop: virtual_screen_rect(),
        found: None,
    };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize));
    }
    ctx.found
}

fn send_escape() -> bool {
    let press = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_ESCAPE,
                ..Default::default()
            },
        },
    };
    let release = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_ESCAPE,
                dwFlags: KEYEVENTF_KEYUP,
                ..Default::default()
            },
        },
    };
    let inputs = [press, release];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    sent == inputs.len() as u32
}

/// Recorta al escritorio virtual; una maximizada no debe incluir bordes fuera.
fn clipped_capture_rect(rect: Rect, desktop: Rect) -> Option<Rect> {
    let clipped = rect.intersect(desktop);
    (clipped.width() >= MIN_WINDOW_SIZE && clipped.height() >= MIN_WINDOW_SIZE).then_some(clipped)
}

fn root_window(hwnd: HWND) -> HWND {
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    if root.0.is_null() {
        hwnd
    } else {
        root
    }
}

unsafe fn window_class(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let len = GetClassNameW(hwnd, &mut buffer);
    if len > 0 {
        String::from_utf16_lossy(&buffer[..len as usize])
    } else {
        String::new()
    }
}

unsafe fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 512];
    let len = GetWindowTextW(hwnd, &mut buffer);
    if len > 0 {
        String::from_utf16_lossy(&buffer[..len as usize])
    } else {
        String::new()
    }
}

unsafe fn cloaked_value(hwnd: HWND) -> u32 {
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut core::ffi::c_void,
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok()
    {
        cloaked
    } else {
        0
    }
}

unsafe fn is_cloaked(hwnd: HWND) -> bool {
    cloaked_value(hwnd) != 0
}

unsafe fn process_exe_name(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return None;
    }
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buffer = [0u16; 512];
    let mut size = buffer.len() as u32;
    let result = QueryFullProcessImageNameW(
        process,
        PROCESS_NAME_WIN32,
        PWSTR(buffer.as_mut_ptr()),
        &mut size,
    );
    let _ = CloseHandle(process);
    result.ok()?;

    let len = (size as usize).min(buffer.len());
    let path = String::from_utf16_lossy(&buffer[..len]);
    path.rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_ascii_lowercase())
}

unsafe fn window_snapshot(hwnd: HWND) -> Option<WindowSnapshot> {
    window_snapshot_impl(hwnd, false)
}

/// Igual que `window_snapshot`, pero consulta siempre el ejecutable (debug y
/// muestreo de la banda superior Z).
unsafe fn window_snapshot_all(hwnd: HWND) -> Option<WindowSnapshot> {
    window_snapshot_impl(hwnd, true)
}

unsafe fn window_snapshot_impl(hwnd: HWND, all_processes: bool) -> Option<WindowSnapshot> {
    let rect = window_rect(hwnd)?;
    let class = window_class(hwnd);
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    let tool_window = ex & WS_EX_TOOLWINDOW.0 != 0;
    let no_activate = ex & WS_EX_NOACTIVATE.0 != 0;
    let popup_style = style & WS_POPUP.0 != 0;
    // El proceso solo hace falta para flyouts del shell, para fusionar
    // popups/menús del mismo proceso y para el muestreo de banda superior.
    let needs_process = all_processes
        || class == "Windows.UI.Core.CoreWindow"
        || is_menu_class(&class)
        || tool_window
        || no_activate
        || popup_style;
    let process = if needs_process {
        process_exe_name(hwnd)
    } else {
        None
    };

    Some(WindowSnapshot {
        title: window_title(hwnd),
        process,
        visible: IsWindowVisible(hwnd).as_bool(),
        minimized: IsIconic(hwnd).as_bool(),
        cloaked: is_cloaked(hwnd),
        tool_window,
        no_activate,
        transparent: ex & WS_EX_TRANSPARENT.0 != 0,
        popup_style,
        class,
        rect,
    })
}

fn log_chosen_window(snapshot: &WindowSnapshot, rect: Rect) {
    crate::logging::info(&format!(
        "ventana capturada: clase={:?} titulo={:?} rect={}x{} @ {},{}",
        snapshot.class,
        snapshot.title,
        rect.width(),
        rect.height(),
        rect.left,
        rect.top
    ));
}

unsafe fn capture_rect_for_window(hwnd: HWND, desktop: Rect) -> Option<(Rect, WindowSnapshot)> {
    let snapshot = window_snapshot(hwnd)?;
    if !is_capturable(&snapshot) {
        return None;
    }
    let rect = clipped_capture_rect(snapshot.rect, desktop)?;
    Some((rect, snapshot))
}

/// Ventana en primer plano (para el atajo de ventana activa).
///
/// Si el primer plano es una ventana auxiliar (herramientas, ventanas propias,
/// 2x16 px…) se recorre el orden Z y se captura la primera ventana de
/// aplicación válida. El rect se recorta al escritorio virtual.
pub fn foreground_window_rect() -> Option<Rect> {
    let desktop = virtual_screen_rect();
    let foreground = unsafe { GetForegroundWindow() };
    if !foreground.0.is_null() {
        let root = root_window(foreground);
        unsafe {
            if let Some(snapshot) = window_snapshot(root) {
                if is_capturable(&snapshot) {
                    if let Some(rect) = clipped_capture_rect(snapshot.rect, desktop) {
                        log_chosen_window(&snapshot, rect);
                        return Some(rect);
                    }
                }
                crate::logging::info(&format!(
                    "ventana en primer plano descartada: clase={:?} titulo={:?} rect={}x{}",
                    snapshot.class,
                    snapshot.title,
                    snapshot.rect.width(),
                    snapshot.rect.height()
                ));
            }
        }
    }

    let mut hwnd = unsafe { GetTopWindow(None) }.ok()?;
    while !hwnd.0.is_null() {
        let root = root_window(hwnd);
        if let Some((rect, snapshot)) = unsafe { capture_rect_for_window(root, desktop) } {
            log_chosen_window(&snapshot, rect);
            return Some(rect);
        }
        hwnd = match unsafe { GetWindow(hwnd, GW_HWNDNEXT) } {
            Ok(next) => next,
            Err(_) => break,
        };
    }

    crate::logging::warn("no se encontró una ventana de aplicación válida para capturar");
    None
}

pub(crate) fn rect_from(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

pub(crate) fn normalize(r: Rect) -> Rect {
    Rect::new(
        r.left.min(r.right),
        r.top.min(r.bottom),
        r.left.max(r.right),
        r.top.max(r.bottom),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alfa_opaco_sin_tocar_bgr() {
        let mut bytes = vec![1, 2, 3, 0, 4, 5, 6, 7, 8, 9, 10, 11];
        set_alpha_opaque(&mut bytes);
        assert_eq!(
            bytes,
            vec![1, 2, 3, 255, 4, 5, 6, 255, 8, 9, 10, 255]
        );
    }

    fn snapshot(class: &str) -> WindowSnapshot {
        WindowSnapshot {
            class: class.to_string(),
            title: "Documento".to_string(),
            process: None,
            visible: true,
            minimized: false,
            cloaked: false,
            tool_window: false,
            no_activate: false,
            transparent: false,
            popup_style: false,
            rect: Rect::new(100, 100, 900, 700),
        }
    }

    fn target(kind: SnapKind) -> Option<SnapTarget> {
        Some(SnapTarget {
            rect: Rect::new(100, 100, 900, 700),
            kind,
        })
    }

    #[test]
    fn filtro_acepta_una_ventana_de_aplicacion_normal() {
        assert!(is_capturable(&snapshot("Notepad")));
        assert!(is_capturable(&snapshot("Chrome_WidgetWin_1")));
    }

    #[test]
    fn filtro_descarta_estados_y_ventanas_auxiliares() {
        let base = snapshot("Notepad");
        let mutations: [fn(&mut WindowSnapshot); 7] = [
            |s| s.visible = false,
            |s| s.minimized = true,
            |s| s.cloaked = true,
            |s| s.tool_window = true,
            |s| s.no_activate = true,
            |s| s.transparent = true,
            |s| s.class.clear(),
        ];
        for mutate in mutations {
            let mut candidate = base.clone();
            mutate(&mut candidate);
            assert!(!is_capturable(&candidate), "{candidate:?}");
        }
    }

    #[test]
    fn ac5_filtro_descarta_shell_y_ventanas_propias() {
        for class in [
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
            "Progman",
            "WorkerW",
            "Destello.Control",
            "Destello.Native.Surface",
        ] {
            assert!(!is_capturable(&snapshot(class)), "{class}");
        }
    }

    #[test]
    fn ac5_filtro_descarta_ventanas_menores_de_40x40() {
        let mut small = snapshot("Ayuda");
        small.rect = Rect::new(0, 0, 39, 500);
        assert!(!is_capturable(&small));

        small.rect = Rect::new(0, 0, 500, 39);
        assert!(!is_capturable(&small));

        small.rect = Rect::new(0, 0, 40, 40);
        assert!(is_capturable(&small));
    }

    #[test]
    fn recorte_de_ventana_maximizada_al_escritorio_virtual() {
        let desktop = Rect::new(0, 0, 1920, 1040);
        // DWMWA_EXTENDED_FRAME_BOUNDS de una maximizada incluye el borde fuera.
        let maximized = Rect::new(-8, -8, 1928, 1048);
        assert_eq!(
            clipped_capture_rect(maximized, desktop),
            Some(desktop)
        );

        // Un monitor a la izquierda con coordenadas negativas.
        let desktop = Rect::new(-1920, 0, 0, 1080);
        assert_eq!(
            clipped_capture_rect(Rect::new(-1928, -8, 8, 1088), desktop),
            Some(desktop)
        );

        // Si al recortar queda un sliver de menos de 40 px, se descarta.
        assert!(clipped_capture_rect(Rect::new(1910, 0, 1950, 50), Rect::new(0, 0, 1920, 1040)).is_none());
    }

    #[test]
    fn clasifica_ventanas_menus_flyouts_y_taskbar() {
        let desktop = Rect::new(0, 0, 1920, 1080);
        assert_eq!(
            classify_snap(&snapshot("Notepad"), desktop),
            target(SnapKind::Window)
        );

        let mut classic = snapshot("#32768");
        classic.tool_window = true;
        assert_eq!(
            classify_snap(&classic, desktop),
            target(SnapKind::Menu)
        );
        assert_eq!(
            classify_snap(&snapshot("Xaml_WindowedPopupClass"), desktop),
            target(SnapKind::Menu)
        );
        assert_eq!(
            classify_snap(
                &snapshot("Microsoft.UI.Content.PopupWindowSiteBridge"),
                desktop
            ),
            target(SnapKind::Menu)
        );

        let mut popup = snapshot("VentanaPopUp");
        popup.popup_style = true;
        assert_eq!(classify_snap(&popup, desktop), target(SnapKind::Menu));

        for process in [
            "StartMenuExperienceHost.exe",
            "SearchHost.exe",
            "ShellExperienceHost.exe",
        ] {
            let mut flyout = snapshot("Windows.UI.Core.CoreWindow");
            flyout.process = Some(process.to_string());
            assert_eq!(
                classify_snap(&flyout, desktop),
                target(SnapKind::ShellFlyout),
                "{process}"
            );
        }

        let mut other_core = snapshot("Windows.UI.Core.CoreWindow");
        other_core.process = Some("Notepad.exe".to_string());
        assert_eq!(classify_snap(&other_core, desktop), None);

        assert_eq!(
            classify_snap(&snapshot("Shell_TrayWnd"), desktop),
            target(SnapKind::Taskbar)
        );
        assert_eq!(
            classify_snap(&snapshot("Shell_SecondaryTrayWnd"), desktop),
            target(SnapKind::Taskbar)
        );
    }

    #[test]
    fn clasificacion_descarta_sombras_fuera_de_pantalla_y_tamanos_minimos() {
        let desktop = Rect::new(0, 0, 1920, 1080);
        for class in ["SysShadow", "Progman", "WorkerW", "Destello.Native.Surface"] {
            assert_eq!(classify_snap(&snapshot(class), desktop), None, "{class}");
        }

        let base = snapshot("#32768");
        let mutations: [fn(&mut WindowSnapshot); 5] = [
            |s| s.visible = false,
            |s| s.minimized = true,
            |s| s.cloaked = true,
            |s| s.transparent = true,
            |s| s.class.clear(),
        ];
        for mutate in mutations {
            let mut candidate = base.clone();
            mutate(&mut candidate);
            assert_eq!(classify_snap(&candidate, desktop), None, "{candidate:?}");
        }

        let mut tiny = snapshot("#32768");
        tiny.rect = Rect::new(0, 0, 7, 100);
        assert_eq!(classify_snap(&tiny, desktop), None);
        tiny.rect = Rect::new(0, 0, 8, 100);
        assert_eq!(
            classify_snap(&tiny, desktop).map(|snap| snap.kind),
            Some(SnapKind::Menu)
        );

        let mut offscreen = snapshot("#32768");
        offscreen.rect = Rect::new(2000, 0, 2100, 100);
        assert_eq!(classify_snap(&offscreen, desktop), None);
    }

    fn candidate(kind: SnapKind, process: &str, rect: Rect) -> ClassifiedTarget {
        ClassifiedTarget {
            target: SnapTarget { rect, kind },
            process: Some(process.to_string()),
        }
    }

    #[test]
    fn ac4_fusiona_menus_apilados_del_mismo_proceso() {
        let strip = Rect::new(100, 100, 496, 180);
        let body = Rect::new(100, 182, 496, 635);
        let merged = merge_split_menus(vec![
            candidate(SnapKind::Menu, "explorer.exe", strip),
            candidate(SnapKind::Menu, "explorer.exe", body),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].target.kind, SnapKind::Menu);
        assert_eq!(merged[0].target.rect, Rect::new(100, 100, 496, 635));

        // Tres trozos apilados también terminan en uno.
        let top = Rect::new(100, 100, 496, 180);
        let mid = Rect::new(100, 182, 496, 260);
        let bottom = Rect::new(100, 262, 496, 635);
        let merged = merge_split_menus(vec![
            candidate(SnapKind::Menu, "explorer.exe", top),
            candidate(SnapKind::Menu, "explorer.exe", mid),
            candidate(SnapKind::Menu, "explorer.exe", bottom),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].target.rect, Rect::new(100, 100, 496, 635));
    }

    #[test]
    fn no_fusiona_cascadas_laterales_ni_procesos_distintos() {
        let body = Rect::new(100, 100, 496, 553);
        let side = Rect::new(498, 100, 700, 400);
        let merged = merge_split_menus(vec![
            candidate(SnapKind::Menu, "explorer.exe", body),
            candidate(SnapKind::Menu, "explorer.exe", side),
        ]);
        assert_eq!(merged.len(), 2, "el submenú en cascada va al lado");

        let below = Rect::new(100, 555, 496, 700);
        let merged = merge_split_menus(vec![
            candidate(SnapKind::Menu, "explorer.exe", body),
            candidate(SnapKind::Menu, "otra.exe", below),
        ]);
        assert_eq!(merged.len(), 2, "procesos distintos no se fusionan");

        let shifted = Rect::new(103, 555, 496, 700);
        let merged = merge_split_menus(vec![
            candidate(SnapKind::Menu, "explorer.exe", body),
            candidate(SnapKind::Menu, "explorer.exe", shifted),
        ]);
        assert_eq!(merged.len(), 2, "left/right deben coincidir ±2 px");
    }

    #[test]
    fn fusiona_con_hueco_de_dos_px_pero_no_de_tres() {
        let strip = Rect::new(100, 100, 496, 180);
        let body = Rect::new(100, 182, 496, 400);
        assert_eq!(
            merge_split_menus(vec![
                candidate(SnapKind::Menu, "explorer.exe", strip),
                candidate(SnapKind::Menu, "explorer.exe", body),
            ])
            .len(),
            1
        );

        let body = Rect::new(100, 183, 496, 400);
        assert_eq!(
            merge_split_menus(vec![
                candidate(SnapKind::Menu, "explorer.exe", strip),
                candidate(SnapKind::Menu, "explorer.exe", body),
            ])
            .len(),
            2
        );
    }

    #[test]
    fn no_fusiona_ventanas_normales_aunque_esten_apiladas() {
        let a = Rect::new(100, 100, 496, 180);
        let b = Rect::new(100, 182, 496, 400);
        let merged = merge_split_menus(vec![
            candidate(SnapKind::Window, "explorer.exe", a),
            candidate(SnapKind::Window, "explorer.exe", b),
        ]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn banda_superior_clasifica_corewindow_xaml_shell_taskbar_y_menu() {
        let desktop = Rect::new(0, 0, 1920, 1080);

        let mut start = snapshot("Windows.UI.Core.CoreWindow");
        start.process = Some("StartMenuExperienceHost.exe".to_string());
        assert_eq!(
            upper_band_verdict(&start, desktop),
            SnapVerdict::Target(SnapKind::ShellFlyout)
        );

        // La regla de banda superior trata cualquier CoreWindow como flyout.
        let mut core = snapshot("Windows.UI.Core.CoreWindow");
        core.process = Some("notepad.exe".to_string());
        assert_eq!(
            upper_band_verdict(&core, desktop),
            SnapVerdict::Target(SnapKind::ShellFlyout)
        );

        assert_eq!(
            upper_band_verdict(&snapshot("TopLevelWindowForOverflowXamlIsland"), desktop),
            SnapVerdict::Target(SnapKind::ShellFlyout)
        );
        assert_eq!(
            upper_band_verdict(&snapshot("Xaml_WindowedPopupClass"), desktop),
            SnapVerdict::Target(SnapKind::ShellFlyout)
        );

        let mut search = snapshot("VentanaShell");
        search.process = Some("SearchHost.exe".to_string());
        assert_eq!(
            upper_band_verdict(&search, desktop),
            SnapVerdict::Target(SnapKind::ShellFlyout)
        );

        assert_eq!(
            upper_band_verdict(&snapshot("Shell_TrayWnd"), desktop),
            SnapVerdict::Target(SnapKind::Taskbar)
        );
        assert_eq!(
            upper_band_verdict(&snapshot("OtraVentana"), desktop),
            SnapVerdict::Target(SnapKind::Menu)
        );
    }

    #[test]
    fn banda_superior_rechaza_invisibles_cloaked_propias_y_fuera() {
        let desktop = Rect::new(0, 0, 1920, 1080);
        let base = snapshot("VentanaSuperior");

        let mut invisible = base.clone();
        invisible.visible = false;
        assert_eq!(
            upper_band_verdict(&invisible, desktop),
            SnapVerdict::Reject("invisible")
        );

        let mut cloaked = base.clone();
        cloaked.cloaked = true;
        assert_eq!(
            upper_band_verdict(&cloaked, desktop),
            SnapVerdict::Reject("cloaked")
        );

        let own = snapshot("Destello.Native.Surface");
        assert_eq!(
            upper_band_verdict(&own, desktop),
            SnapVerdict::Reject("ventana propia de Destello")
        );

        let mut tiny = base.clone();
        tiny.rect = Rect::new(0, 0, 7, 7);
        assert_eq!(
            upper_band_verdict(&tiny, desktop),
            SnapVerdict::Reject("recorte <8x8 o fuera de pantalla")
        );

        let mut outside = base;
        outside.rect = Rect::new(2000, 0, 2100, 100);
        assert_eq!(
            upper_band_verdict(&outside, desktop),
            SnapVerdict::Reject("recorte <8x8 o fuera de pantalla")
        );
    }

    #[test]
    fn detecta_flyout_casi_a_pantalla_completa() {
        let desktop = Rect::new(0, 0, 1920, 1080);
        let work = [Rect::new(0, 0, 1920, 1032)];
        assert!(covers_almost_all(work[0], desktop, &work));
        assert!(covers_almost_all(
            Rect::new(-8, -8, 1928, 1048),
            desktop,
            &work
        ));
        assert!(!covers_almost_all(
            Rect::new(100, 100, 1700, 1000),
            desktop,
            &work
        ));

        let left_monitor = [Rect::new(-1920, 0, 0, 1040)];
        assert!(covers_almost_all(
            Rect::new(-1920, 0, 0, 1040),
            desktop,
            &left_monitor
        ));
    }

    #[test]
    fn paneles_uia_filtran_barras_y_contenedores_pantalla_completa() {
        let window = Rect::new(0, 0, 1920, 1032);
        let window_area = rect_area(window);

        let main = Rect::new(403, 219, 1235, 955);
        let mobile = Rect::new(1241, 183, 1517, 955);
        assert_eq!(panel_rect_candidate(main, window, window_area), Some(main));
        assert_eq!(
            panel_rect_candidate(mobile, window, window_area),
            Some(mobile)
        );

        // Barra de navegación y botón de búsqueda: área insuficiente.
        assert_eq!(
            panel_rect_candidate(Rect::new(455, 967, 1183, 1007), window, window_area),
            None
        );
        assert_eq!(
            panel_rect_candidate(Rect::new(435, 171, 1171, 203), window, window_area),
            None
        );
        // El contenedor transparente ocupa toda la ventana.
        assert_eq!(panel_rect_candidate(window, window, window_area), None);
    }

    #[test]
    fn reconstruye_los_dos_paneles_del_inicio_desde_segmentos_uia() {
        let window = Rect::new(0, 0, 1920, 1032);
        let segments = vec![
            Rect::new(435, 171, 1171, 203),  // buscador
            Rect::new(1175, 171, 1207, 203), // botón móvil
            Rect::new(403, 219, 1235, 955),  // cuerpo principal
            Rect::new(455, 967, 1183, 1007), // navegación
            Rect::new(1241, 183, 1517, 955), // grupo móvil
            Rect::new(1461, 967, 1501, 1007), // botón Más
        ];
        let panels = panels_from_segments(segments, window, rect_area(window));
        assert_eq!(
            panels,
            vec![
                Rect::new(403, 155, 1235, 1023),
                Rect::new(1241, 155, 1517, 1023),
            ]
        );

        let small = vec![Rect::new(435, 171, 1171, 203)];
        assert!(panels_from_segments(small, window, rect_area(window)).is_empty());
    }

    #[test]
    fn paneles_uia_eliminan_hijos_contenidos_y_conservan_columnas() {
        let main = Rect::new(403, 219, 1235, 955);
        let inner = Rect::new(500, 300, 900, 700);
        let mobile = Rect::new(1241, 183, 1517, 955);
        let panels = dedupe_panels(vec![inner, mobile, main]);
        assert_eq!(panels, vec![main, mobile]);
    }

    #[test]
    fn clasificacion_recorta_menus_maximizados_y_negativos() {
        let desktop = Rect::new(-1920, 0, 0, 1080);
        let mut menu = snapshot("#32768");
        menu.rect = Rect::new(-1928, -8, 8, 1088);
        assert_eq!(
            classify_snap(&menu, desktop),
            Some(SnapTarget {
                rect: desktop,
                kind: SnapKind::Menu,
            })
        );
    }
}


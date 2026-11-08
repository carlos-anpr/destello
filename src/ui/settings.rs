use super::{
    annotate::TextEdit,
    d2d::{self, theme, Canvas, Graphics, NativeWindow},
};
use crate::{
    config::{Config, ImageFormat},
    geom::{Point, Rect},
};
use std::{cell::RefCell, rc::Rc};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{Direct2D::*, Gdi::*},
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
};
struct Settings {
    cfg: Config,
    gfx: std::sync::Arc<Graphics>,
    rt: Option<ID2D1HwndRenderTarget>,
    result: Rc<RefCell<Option<Config>>>,
    scale: f32,
    editing: Option<TextEdit>,
    hover: Option<usize>,
    capturing: Option<usize>,
    hotkey_errors: [Option<String>; 5],
}
impl Settings {
    fn r(&self, x: i32, y: i32, w: i32, h: i32) -> Rect {
        let s = self.scale;
        Rect::new(
            (x as f32 * s) as i32,
            (y as f32 * s) as i32,
            ((x + w) as f32 * s) as i32,
            ((y + h) as f32 * s) as i32,
        )
    }
    fn hit(&self, p: Point) -> Option<usize> {
        let p = Point {
            x: (p.x as f32 / self.scale) as i32,
            y: (p.y as f32 / self.scale) as i32,
        };
        if Rect::new(28, 115, 612, 153).contains(p) {
            return Some(1);
        }
        if Rect::new(400, 190, 612, 225).contains(p) {
            return Some(2);
        }
        if Rect::new(400, 236, 612, 271).contains(p) {
            return Some(3);
        }
        for i in 0..4 {
            if Rect::new(28, 288 + i * 34, 612, 320 + i * 34).contains(p) {
                return Some(7 + i as usize);
            }
        }
        if Rect::new(28, 756, 180, 798).contains(p) {
            return Some(11);
        }
        if Rect::new(430, 756, 612, 798).contains(p) {
            return Some(12);
        }
        for i in 0..5 {
            if Rect::new(320, 468 + i * 52, 612, 498 + i * 52).contains(p) {
                return Some(13 + i as usize);
            }
        }
        None
    }
    fn hotkey(&self, index: usize) -> &str {
        match index {
            0 => &self.cfg.hotkeys.area,
            1 => &self.cfg.hotkeys.full_monitor,
            2 => &self.cfg.hotkeys.all_monitors,
            3 => &self.cfg.hotkeys.active_window,
            _ => &self.cfg.hotkeys.repeat_area,
        }
    }
    fn set_hotkey(&mut self, index: usize, value: String) {
        match index {
            0 => self.cfg.hotkeys.area = value,
            1 => self.cfg.hotkeys.full_monitor = value,
            2 => self.cfg.hotkeys.all_monitors = value,
            3 => self.cfg.hotkeys.active_window = value,
            _ => self.cfg.hotkeys.repeat_area = value,
        }
    }
    fn capture_key(&mut self, k: u32) {
        let Some(index) = self.capturing else { return };
        self.hotkey_errors[index] = None;
        if k == 27 {
            self.capturing = None;
            return;
        }
        if k == 8 || k == 46 {
            self.set_hotkey(index, String::new());
            self.capturing = None;
            return;
        }
        if matches!(k,16..=18|91|92|160..=165) {
            return;
        }
        let down = |key| unsafe { GetKeyState(key) < 0 };
        let mods = if down(17) { 2 } else { 0 }
            | if down(18) { 1 } else { 0 }
            | if down(16) { 4 } else { 0 }
            | if down(91) || down(92) { 8 } else { 0 };
        let value = crate::hotkeys::format_hotkey(mods, k);
        if crate::hotkeys::parse_hotkey(&value).is_none() {
            self.hotkey_errors[index] = Some("Esta tecla no se puede usar como atajo".into());
            return;
        }
        if mods == 1 && k == 44 {
            self.hotkey_errors[index] = Some("Alt+ImpPnt está reservado para Windows".into());
            return;
        }
        if (0..5).any(|i| i != index && !value.is_empty() && self.hotkey(i) == value) {
            self.hotkey_errors[index] = Some("Ese atajo ya está asignado a otra captura".into());
            return;
        }
        self.set_hotkey(index, value);
        self.capturing = None;
    }
    fn commit(&mut self) {
        if let Some(e) = self.editing.take() {
            if !e.text.trim().is_empty() {
                self.cfg.filename_pattern = e.text
            }
        }
    }
    fn click(&mut self, hwnd: HWND, id: usize) {
        if id != 1 {
            self.commit()
        }
        match id {
            1 => {
                if self.editing.is_none() {
                    let mut e = TextEdit::default();
                    for unit in self.cfg.filename_pattern.encode_utf16() {
                        e.input(unit)
                    }
                    self.editing = Some(e)
                }
            }
            2 => {
                self.cfg.format = if self.cfg.format == ImageFormat::Png {
                    ImageFormat::Jpg
                } else {
                    ImageFormat::Png
                }
            }
            3 => {
                self.cfg.jpeg_quality = if self.cfg.jpeg_quality >= 100 {
                    60
                } else {
                    (self.cfg.jpeg_quality + 5).min(100)
                }
            }
            7 => self.cfg.include_cursor = !self.cfg.include_cursor,
            8 => self.cfg.show_magnifier = !self.cfg.show_magnifier,
            9 => self.cfg.launch_at_startup = !self.cfg.launch_at_startup,
            10 => {
                self.cfg.last_color = 0x8b5cf6;
                self.cfg.last_thickness = 3.;
                self.cfg.last_tool = "move".into()
            }
            11 => {
                self.cfg = Config::default();
                self.editing = None;
                self.capturing = None;
                self.hotkey_errors.fill(None);
            }
            12 => {
                *self.result.borrow_mut() = Some(self.cfg.clone());
                d2d::close(hwnd)
            }
            13..=17 => {
                self.hotkey_errors.fill(None);
                self.capturing = Some(id - 13);
            }
            _ => {}
        }
        d2d::invalidate(hwnd)
    }
    fn paint(&mut self, hwnd: HWND) -> Result<()> {
        let mut cr = RECT::default();
        unsafe {
            GetClientRect(hwnd, &mut cr)?;
        }
        if self.rt.is_none() {
            self.rt = Some(self.gfx.window_target(
                hwnd,
                cr.right.max(1) as u32,
                cr.bottom.max(1) as u32,
            )?)
        }
        let Some(rt) = &self.rt else { return Ok(()) };
        let c = Canvas { rt, gfx: &self.gfx };
        unsafe {
            rt.BeginDraw();
        }
        let result: Result<()> = (|| {
            c.fill(Rect::new(0, 0, cr.right, cr.bottom), theme::BASE, 1.)?;
            c.rounded(self.r(28, 24, 38, 38), 11. * self.scale, theme::ACCENT, 1.)?;
            c.text("D", self.r(38, 27, 26, 30), 25. * self.scale, theme::TEXT)?;
            c.text(
                "Destello",
                self.r(80, 22, 300, 40),
                26. * self.scale,
                theme::TEXT,
            )?;
            c.text(
                "Tu captura, a tu manera",
                self.r(80, 60, 480, 24),
                13. * self.scale,
                theme::MUTED,
            )?;
            c.text(
                "NOMBRE DEL ARCHIVO",
                self.r(28, 88, 550, 22),
                10. * self.scale,
                theme::MUTED,
            )?;
            c.rounded(
                self.r(28, 115, 584, 38),
                7. * self.scale,
                if self.editing.is_some() {
                    0x303052
                } else {
                    theme::SURFACE
                },
                1.,
            )?;
            let pattern = self
                .editing
                .as_ref()
                .map_or(self.cfg.filename_pattern.as_str(), |e| e.text.as_str());
            c.text(
                pattern,
                self.r(39, 124, 560, 26),
                14. * self.scale,
                theme::TEXT,
            )?;
            if let Some(edit) = &self.editing {
                let (p, h) = c.caret(
                    pattern,
                    self.r(39, 124, 560, 26),
                    14. * self.scale,
                    edit.text[..edit.caret].encode_utf16().count() as u32,
                )?;
                c.line(p, Point { x: p.x, y: p.y + h }, 0xd3c5ff, 1., 1.)?;
            }
            let preview = pattern
                .replace(
                    "{fecha}",
                    &chrono::Local::now().format("%Y-%m-%d").to_string(),
                )
                .replace(
                    "{hora}",
                    &chrono::Local::now().format("%H.%M.%S").to_string(),
                );
            c.text(
                &format!(
                    "Vista previa: {preview}.{}",
                    if self.cfg.format == ImageFormat::Png {
                        "png"
                    } else {
                        "jpg"
                    }
                ),
                self.r(28, 156, 584, 20),
                11. * self.scale,
                theme::MUTED,
            )?;
            for (y, label, value) in [
                (
                    190,
                    "Formato",
                    if self.cfg.format == ImageFormat::Png {
                        "PNG · sin pérdida".into()
                    } else {
                        "JPG · compacto".into()
                    },
                ),
                (
                    236,
                    "Calidad JPG",
                    format!("{} %   − / +", self.cfg.jpeg_quality),
                ),
            ] {
                c.text(
                    label,
                    self.r(28, y + 8, 350, 26),
                    14. * self.scale,
                    0xe1e7f5,
                )?;
                c.rounded(self.r(400, y, 212, 35), 7. * self.scale, theme::SURFACE, 1.)?;
                c.text(
                    &value,
                    self.r(416, y + 8, 190, 28),
                    13. * self.scale,
                    theme::ACCENT,
                )?;
            }
            for (i, (label, on)) in [
                ("Incluir cursor en la captura", self.cfg.include_cursor),
                ("Lupa de precisión", self.cfg.show_magnifier),
                ("Iniciar con Windows", self.cfg.launch_at_startup),
                ("Restablecer estilo de las herramientas", false),
            ]
            .into_iter()
            .enumerate()
            {
                let y = 288 + i as i32 * 34;
                if self.hover == Some(i + 7) {
                    c.rounded(self.r(20, y, 600, 32), 6. * self.scale, theme::TEXT, 0.035)?
                }
                c.text(
                    label,
                    self.r(28, y + 7, 505, 27),
                    13. * self.scale,
                    0xdde4f3,
                )?;
                if i < 3 {
                    c.rounded(
                        self.r(566, y + 6, 42, 22),
                        11. * self.scale,
                        if on { theme::ACCENT } else { 0x374056 },
                        1.,
                    )?;
                    c.ellipse(
                        self.r(if on { 589 } else { 569 }, y + 9, 16, 16),
                        theme::TEXT,
                        1.,
                        1.,
                        true,
                    )?
                } else {
                    c.text(
                        "↺",
                        self.r(574, y + 1, 40, 30),
                        23. * self.scale,
                        theme::ACCENT,
                    )?
                }
            }
            c.text(
                "ATAJOS · clic para cambiar; Supr para desactivar",
                self.r(28, 444, 584, 18),
                10. * self.scale,
                theme::MUTED,
            )?;
            for (i, label) in [
                "Capturar área",
                "Monitor completo",
                "Todos los monitores",
                "Ventana activa",
                "Repetir última área",
            ]
            .iter()
            .enumerate()
            {
                let y = 468 + i as i32 * 52;
                c.text(
                    label,
                    self.r(28, y + 6, 280, 24),
                    12. * self.scale,
                    0xc5cde0,
                )?;
                c.rounded(
                    self.r(320, y, 292, 30),
                    6. * self.scale,
                    if self.capturing == Some(i) {
                        0x493a77
                    } else {
                        theme::SURFACE
                    },
                    1.,
                )?;
                let value = if self.capturing == Some(i) {
                    "Pulsa el nuevo atajo…"
                } else if self.hotkey(i).is_empty() {
                    "Desactivado"
                } else {
                    self.hotkey(i)
                };
                c.text(
                    value,
                    self.r(332, y + 6, 270, 24),
                    12. * self.scale,
                    0xd1c5ff,
                )?;
                if let Some(error) = &self.hotkey_errors[i] {
                    c.text(
                        error,
                        self.r(320, y + 32, 292, 18),
                        10. * self.scale,
                        0xf0a3ad,
                    )?;
                }
            }
            c.rounded(
                self.r(28, 756, 152, 42),
                8. * self.scale,
                theme::SURFACE,
                1.,
            )?;
            c.text(
                "Restablecer",
                self.r(48, 769, 142, 25),
                13. * self.scale,
                0xd9e1f2,
            )?;
            c.rounded(
                self.r(430, 756, 182, 42),
                8. * self.scale,
                theme::ACCENT,
                1.,
            )?;
            c.text(
                "Guardar ajustes",
                self.r(456, 769, 164, 25),
                13. * self.scale,
                theme::TEXT,
            )?;
            Ok(())
        })();
        let end = unsafe { rt.EndDraw(None, None) };
        if end
            .as_ref()
            .is_err_and(|e| e.code() == D2DERR_RECREATE_TARGET)
        {
            self.rt = None;
            d2d::invalidate(hwnd);
            return Ok(());
        }
        result?;
        end
    }
}
impl NativeWindow for Settings {
    fn message(&mut self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                unsafe {
                    let _ = BeginPaint(hwnd, &mut ps);
                }
                let result = self.paint(hwnd);
                unsafe {
                    let _ = EndPaint(hwnd, &ps);
                }
                if let Err(e) = result {
                    crate::notify::error(&e.to_string());
                    d2d::close(hwnd)
                }
            }
            WM_LBUTTONUP => {
                if let Some(id) = self.hit(d2d::local_point(l)) {
                    self.click(hwnd, id)
                } else {
                    self.commit()
                }
                d2d::invalidate(hwnd)
            }
            WM_MOUSEMOVE => {
                let hover = self.hit(d2d::local_point(l));
                if hover != self.hover {
                    self.hover = hover;
                    d2d::invalidate(hwnd)
                }
            }
            WM_KEYDOWN | WM_SYSKEYDOWN if self.capturing.is_some() => {
                self.capture_key(w.0 as u32);
                d2d::invalidate(hwnd)
            }
            WM_KEYDOWN => {
                if let Some(e) = &mut self.editing {
                    match w.0 {
                        8 => e.backspace(),
                        46 => e.delete(),
                        37 => e.left(),
                        39 => e.right(),
                        36 => e.caret = 0,
                        35 => e.caret = e.text.len(),
                        13 => self.commit(),
                        27 => self.editing = None,
                        _ => {}
                    }
                    d2d::invalidate(hwnd)
                } else if w.0 == 27 {
                    d2d::close(hwnd)
                }
            }
            WM_CHAR => {
                if let Some(e) = &mut self.editing {
                    if w.0 >= 32 {
                        e.input(w.0 as u16);
                        d2d::invalidate(hwnd)
                    }
                }
            }
            WM_MOUSEWHEEL => {
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                    let _ = ScreenToClient(hwnd, &mut p);
                }
                if self.r(400, 236, 212, 35).contains(Point { x: p.x, y: p.y }) {
                    let delta = if ((w.0 >> 16) as u16 as i16) > 0 {
                        5i16
                    } else {
                        -5
                    };
                    self.cfg.jpeg_quality =
                        (self.cfg.jpeg_quality as i16 + delta).clamp(1, 100) as u8;
                    d2d::invalidate(hwnd)
                }
            }
            WM_DPICHANGED => {
                self.scale = ((w.0 >> 16) as u16) as f32 / 96.;
                if l.0 != 0 {
                    let r = unsafe { *(l.0 as *const RECT) };
                    let m = crate::capture::monitor_at(Point {
                        x: r.left,
                        y: r.top,
                    });
                    self.scale = self
                        .scale
                        .min(m.work.height() as f32 / 876.)
                        .min(m.work.width() as f32 / 680.);
                    let mut frame = RECT {
                        left: 0,
                        top: 0,
                        right: (640. * self.scale) as i32,
                        bottom: (830. * self.scale) as i32,
                    };
                    unsafe {
                        let _ = AdjustWindowRectEx(
                            &mut frame,
                            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                            false,
                            WS_EX_TOOLWINDOW,
                        );
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            r.left,
                            r.top,
                            frame.right - frame.left,
                            frame.bottom - frame.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
                self.rt = None;
                d2d::invalidate(hwnd)
            }
            WM_SIZE => {
                self.rt = None;
                d2d::invalidate(hwnd)
            }
            WM_CLOSE => unsafe {
                let _ = DestroyWindow(hwnd);
            },
            WM_ERASEBKGND => return Some(LRESULT(1)),
            _ => return None,
        }
        Some(LRESULT(0))
    }
}
pub fn open(cfg: &mut Config) -> bool {
    let result = Rc::new(RefCell::new(None));
    let run = (|| -> Result<()> {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        let m = crate::capture::monitor_at(Point { x: p.x, y: p.y });
        let scale = (m.dpi as f32 / 96.)
            .min(m.work.height() as f32 / 876.)
            .min(m.work.width() as f32 / 680.);
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: (640. * scale) as i32,
            bottom: (830. * scale) as i32,
        };
        unsafe {
            AdjustWindowRectEx(
                &mut frame,
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                false,
                WS_EX_TOOLWINDOW,
            )?;
        }
        let w = frame.right - frame.left;
        let h = frame.bottom - frame.top;
        let area = Rect::new(
            m.work.center().x - w / 2,
            m.work.center().y - h / 2,
            m.work.center().x + w / 2,
            m.work.center().y + h / 2,
        );
        let settings = Settings {
            cfg: cfg.clone(),
            gfx: Graphics::new()?,
            rt: None,
            result: result.clone(),
            scale,
            editing: None,
            hover: None,
            capturing: None,
            hotkey_errors: std::array::from_fn(|_| None),
        };
        let hwnd = d2d::window(
            "Ajustes · Destello",
            area,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            WS_EX_TOOLWINDOW,
            Box::new(settings),
        )?;
        d2d::round_window(hwnd);
        d2d::dark_backdrop(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let mut msg = MSG::default();
            while IsWindow(Some(hwnd)).as_bool() {
                let code = GetMessageW(&mut msg, None, 0, 0).0;
                if code <= 0 {
                    if code == 0 {
                        PostQuitMessage(msg.wParam.0 as i32)
                    }
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    })();
    if let Err(e) = run {
        crate::notify::error(&format!("No se pudieron abrir los ajustes: {e}"))
    }
    if let Some(value) = result.borrow_mut().take() {
        *cfg = value;
        return true;
    }
    false
}

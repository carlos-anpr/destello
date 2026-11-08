use super::d2d::{self, Canvas, Graphics, NativeWindow};
use crate::{geom::Rect, image_ops::RgbaImage};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{Direct2D::*, Dwm::*, Gdi::*},
        UI::{Input::KeyboardAndMouse::ReleaseCapture, WindowsAndMessaging::*},
    },
};
struct Pin {
    image: RgbaImage,
    bgra: Vec<u8>,
    gfx: std::sync::Arc<Graphics>,
    rt: Option<ID2D1HwndRenderTarget>,
    zoom: f32,
}
pub fn show(image: RgbaImage, area: Rect) {
    let run = (|| -> Result<()> {
        if image.width == 0 || image.height == 0 {
            return Ok(());
        }
        let mut bgra = image.data.clone();
        for p in bgra.as_chunks_mut::<4>().0 {
            p.swap(0, 2)
        }
        let monitor = crate::capture::monitor_at(area.center());
        let zoom = (monitor.work.width() as f32 / image.width as f32)
            .min(monitor.work.height() as f32 / image.height as f32)
            .min(1.);
        let area = Rect::new(
            area.left,
            area.top,
            area.left + (image.width as f32 * zoom) as i32,
            area.top + (image.height as f32 * zoom) as i32,
        )
        .clamp_to(monitor.work);
        let pin = Pin {
            image,
            bgra,
            gfx: Graphics::new()?,
            rt: None,
            zoom,
        };
        let hwnd = d2d::window(
            "Destello · Fijado",
            area,
            WS_POPUP | WS_THICKFRAME,
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            Box::new(pin),
        )?;
        d2d::round_window(hwnd);
        unsafe {
            let margins = windows::Win32::UI::Controls::MARGINS {
                cxLeftWidth: 1,
                cxRightWidth: 1,
                cyTopHeight: 1,
                cyBottomHeight: 1,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        Ok(())
    })();
    if let Err(e) = run {
        crate::notify::error(&format!("No se pudo fijar la imagen: {e}"))
    }
}
impl Pin {
    fn paint(&mut self, hwnd: HWND) -> Result<()> {
        let mut r = RECT::default();
        unsafe {
            GetClientRect(hwnd, &mut r)?;
        }
        if self.rt.is_none() {
            self.rt = Some(self.gfx.window_target(
                hwnd,
                r.right.max(1) as u32,
                r.bottom.max(1) as u32,
            )?)
        }
        let Some(rt) = &self.rt else { return Ok(()) };
        let c = Canvas { rt, gfx: &self.gfx };
        let bitmap = c.bitmap(self.image.width, self.image.height, &self.bgra)?;
        unsafe {
            rt.BeginDraw();
        }
        c.image(&bitmap, Rect::new(0, 0, r.right, r.bottom), None, false);
        let result = unsafe { rt.EndDraw(None, None) };
        if result
            .as_ref()
            .is_err_and(|e| e.code() == D2DERR_RECREATE_TARGET)
        {
            self.rt = None;
            d2d::invalidate(hwnd);
            return Ok(());
        }
        result
    }
    fn menu(&mut self, hwnd: HWND) {
        unsafe {
            if let Ok(menu) = CreatePopupMenu() {
                let _ = AppendMenuW(menu, MF_STRING, 1, w!("Copiar"));
                let _ = AppendMenuW(menu, MF_STRING, 2, w!("Guardar como…"));
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                let _ = AppendMenuW(menu, MF_STRING, 3, w!("Cerrar"));
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                let cmd = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_RIGHTBUTTON,
                    p.x,
                    p.y,
                    None,
                    hwnd,
                    None,
                )
                .0;
                let _ = DestroyMenu(menu);
                match cmd {
                    1 | 2 => {
                        let mut cfg = crate::config::Config::load().config;
                        let result = if cmd == 1 {
                            crate::output::deliver(
                                self.image.clone(),
                                crate::output::Action::Copy,
                                &cfg,
                                hwnd,
                            )
                            .map(|_| ())
                        } else {
                            crate::output::save_as_dialog(&self.image, &mut cfg, hwnd).and_then(
                                |path| {
                                    if path.is_some() {
                                        cfg.save()?;
                                    }
                                    Ok(())
                                },
                            )
                        };
                        if let Err(e) = result {
                            crate::notify::error(&e.to_string())
                        }
                    }
                    3 => d2d::close(hwnd),
                    _ => {}
                }
            }
        }
    }
}
impl NativeWindow for Pin {
    fn message(&mut self, hwnd: HWND, msg: u32, w: WPARAM, _l: LPARAM) -> Option<LRESULT> {
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
                    crate::notify::error(&format!("No se pudo dibujar la imagen fijada: {e}"));
                    d2d::close(hwnd)
                }
            }
            WM_SIZE => {
                self.rt = None;
                d2d::invalidate(hwnd)
            }
            WM_NCCALCSIZE => return Some(LRESULT(0)),
            WM_LBUTTONDOWN => unsafe {
                let _ = ReleaseCapture();
                let _ = SendMessageW(
                    hwnd,
                    WM_NCLBUTTONDOWN,
                    Some(WPARAM(HTCAPTION as usize)),
                    Some(LPARAM(0)),
                );
            },
            WM_LBUTTONDBLCLK => d2d::close(hwnd),
            WM_KEYDOWN if w.0 == 27 => d2d::close(hwnd),
            WM_RBUTTONUP => self.menu(hwnd),
            WM_MOUSEWHEEL => {
                let step = if ((w.0 >> 16) as u16 as i16) > 0 {
                    1.1
                } else {
                    1. / 1.1
                };
                self.zoom = (self.zoom * step).clamp(0.05, 8.);
                let mut r = RECT::default();
                unsafe {
                    if GetWindowRect(hwnd, &mut r).is_ok() {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            r.left,
                            r.top,
                            (self.image.width as f32 * self.zoom).max(16.) as i32,
                            (self.image.height as f32 * self.zoom).max(16.) as i32,
                            SWP_NOACTIVATE,
                        );
                    }
                }
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

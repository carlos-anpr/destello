use super::{
    annotate::{self, Annotation, History, TextEdit, Tool},
    d2d::{self, theme, Canvas, Graphics, NativeWindow, TextStyle},
    export,
    toolbar::{Button, Toolbar, ToolbarState},
};
use crate::{
    capture::{self, Screenshot},
    config::Config,
    geom::{self, Point, Rect},
    image_ops::RgbaImage,
    output::Action,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};
const REFRESH_SURFACE: u32 = WM_APP + 0x52;
const FOREGROUND_TIMER: usize = 2;
struct OverlayTimings {
    start: Instant,
    copy_ms: Cell<f64>,
    paint_ms: Cell<f64>,
    show_ms: Cell<f64>,
}
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{Direct2D::*, Gdi::*},
        System::Threading::{AttachThreadInput, GetCurrentThreadId},
        UI::{Controls::Dialogs::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
};
pub enum OverlayResult {
    Cancelled,
    Done {
        image: RgbaImage,
        action: Action,
        area: Rect,
    },
    Pin {
        image: RgbaImage,
        area: Rect,
    },
}
#[derive(Clone, Copy)]
enum Drag {
    Select(Point),
    MoveArea(Rect, Point),
    Resize(Rect, usize, Point),
    Draw,
    Annotation(usize, Point),
}
struct Editing {
    annotation: Annotation,
    edit: TextEdit,
}
struct Overlay {
    shot: Screenshot,
    cfg: Config,
    updated_config: Rc<RefCell<Option<Config>>>,
    timings: Rc<OverlayTimings>,
    session: Rc<Cell<bool>>,
    ready: bool,
    persistent: bool,
    stale: bool,
    backend: Backend,
    key: SurfaceKey,
    marker_chosen: bool,
    gfx: std::sync::Arc<Graphics>,
    target: Option<ID2D1HwndRenderTarget>,
    background: Option<ID2D1Bitmap>,
    area: Option<Rect>,
    hover_window: Option<Rect>,
    previous_window: Option<Rect>,
    window_changed: Instant,
    cursor: Point,
    tool: Tool,
    color: u32,
    thickness: f32,
    fill: bool,
    history: History,
    draft: Option<Annotation>,
    editing: Option<Editing>,
    selected: Option<usize>,
    drag: Option<Drag>,
    toolbar: Option<Toolbar>,
    hover: Option<Button>,
    previous_hover: Option<Button>,
    pressed: Option<Button>,
    hover_since: Instant,
    shown: Instant,
    foreground_retry: Option<Instant>,
    result: Rc<RefCell<Option<OverlayResult>>>,
}
impl Overlay {
    fn model(
        shot: Screenshot,
        cfg: &Config,
        gfx: std::sync::Arc<Graphics>,
        surface_key: SurfaceKey,
        persistent: bool,
        backend: Backend,
    ) -> Self {
        let timings = Rc::new(OverlayTimings {
            start: Instant::now(),
            copy_ms: Cell::new(0.),
            paint_ms: Cell::new(0.),
            show_ms: Cell::new(0.),
        });
        let result = Rc::new(RefCell::new(None));
        let mut point = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut point);
        }
        let cursor = Point {
            x: point.x,
            y: point.y,
        };
        let hover_window = snap_target_at(&shot.targets, cursor);
        let mut state = Self {
            shot,
            cfg: cfg.clone(),
            updated_config: Rc::new(RefCell::new(None)),
            timings: timings.clone(),
            session: Rc::new(Cell::new(false)),
            ready: false,
            persistent,
            stale: false,
            backend,
            key: surface_key,
            marker_chosen: false,
            gfx,
            target: None,
            background: None,
            area: None,
            hover_window,
            previous_window: None,
            window_changed: Instant::now(),
            cursor,
            tool: match cfg.last_tool.to_ascii_lowercase().as_str() {
                "rectangle" | "rect" => Tool::Rectangle,
                "ellipse" => Tool::Ellipse,
                "arrow" => Tool::Arrow,
                "line" => Tool::Line,
                "pen" => Tool::Pen,
                "marker" => Tool::Marker,
                "text" => Tool::Text,
                "counter" => Tool::Counter,
                "pixelate" => Tool::Pixelate,
                "redact" => Tool::Redact,
                _ => Tool::Move,
            },
            color: cfg.last_color,
            thickness: cfg.last_thickness.clamp(1., 32.),
            fill: false,
            history: History::default(),
            draft: None,
            editing: None,
            selected: None,
            drag: None,
            toolbar: None,
            hover: None,
            previous_hover: None,
            pressed: None,
            hover_since: Instant::now(),
            shown: Instant::now(),
            foreground_retry: None,
            result: result.clone(),
        };
        if state.tool == Tool::Marker {
            state.select_tool(Tool::Marker);
        }
        state
    }
    fn reset(
        &mut self,
        shot: Screenshot,
        cfg: &Config,
        preselected: bool,
        timings: Rc<OverlayTimings>,
    ) {
        let target = self.target.take();
        let background = self.background.take();
        let key = self.key.clone();
        let persistent = self.persistent;
        let backend = self.backend;
        let gfx = self.gfx.clone();
        let session = self.session.clone();
        session.set(false);
        *self = Self::model(shot, cfg, gfx, key, persistent, backend);
        self.session = session;
        self.target = target;
        self.background = background;
        self.timings = timings;
        if preselected {
            self.area = Some(self.bounds());
        }
        self.update_toolbar();
    }
    fn finish(&mut self, hwnd: HWND) {
        self.session.set(false);
        self.ready = false;
        self.drag = None;
        self.draft = None;
        self.editing = None;
        self.history = History::default();
        self.toolbar = None;
        self.area = None;
        self.foreground_retry = None;
        self.shot.targets.clear();
        self.shot.bgra = Vec::new();
        unsafe {
            let _ = ReleaseCapture();
            let _ = KillTimer(Some(hwnd), 1);
            let _ = KillTimer(Some(hwnd), FOREGROUND_TIMER);
            if self.persistent {
                let _ = ShowWindow(hwnd, SW_HIDE);
            } else {
                let _ = DestroyWindow(hwnd);
            }
        }
    }
    fn resources(&mut self, hwnd: HWND) -> Result<()> {
        if self.target.is_none() {
            self.target = Some(self.gfx.overlay_target(
                hwnd,
                self.shot.width,
                self.shot.height,
                self.backend == Backend::Software,
            )?);
            self.background = None;
        }
        if self.background.is_none() {
            let rt = self
                .target
                .as_ref()
                .ok_or_else(|| Error::from_hresult(E_FAIL))?;
            self.background = Some(
                Canvas { rt, gfx: &self.gfx }.reserve_bitmap(self.shot.width, self.shot.height)?,
            );
        }
        Ok(())
    }
    fn upload(&mut self, hwnd: HWND) -> Result<()> {
        self.resources(hwnd)?;
        let pitch = self
            .shot
            .width
            .checked_mul(4)
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        let size = (pitch as usize)
            .checked_mul(self.shot.height as usize)
            .ok_or_else(|| Error::from_hresult(E_INVALIDARG))?;
        if size > self.shot.bgra.len() {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let started = Instant::now();
        let result = if let Some(bitmap) = &self.background {
            unsafe { bitmap.CopyFromMemory(None, self.shot.bgra.as_ptr().cast(), pitch) }
        } else {
            Err(Error::from_hresult(E_FAIL))
        };
        if result
            .as_ref()
            .is_err_and(|e| e.code() == D2DERR_RECREATE_TARGET)
        {
            self.target = None;
            self.background = None;
        }
        result?;
        self.timings
            .copy_ms
            .set(started.elapsed().as_secs_f64() * 1000.);
        Ok(())
    }
    fn prepare(&mut self, hwnd: HWND) -> Result<()> {
        self.ready = false;
        for attempt in 0..2 {
            match self.upload(hwnd) {
                Ok(()) => {}
                Err(e) if e.code() == D2DERR_RECREATE_TARGET && attempt == 0 => continue,
                Err(e) => return Err(e),
            }
            let started = Instant::now();
            match self.paint(hwnd) {
                Ok(()) => {
                    self.timings
                        .paint_ms
                        .set(started.elapsed().as_secs_f64() * 1000.);
                    self.ready = true;
                    return Ok(());
                }
                Err(e) if e.code() == D2DERR_RECREATE_TARGET && attempt == 0 => {}
                Err(e) => return Err(e),
            }
        }
        Err(Error::from_hresult(E_FAIL))
    }
    fn show(&mut self, hwnd: HWND) -> Result<()> {
        self.session.set(true);
        let started = Instant::now();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let present = if let Some(rt) = &self.target {
            unsafe {
                rt.BeginDraw();
                rt.EndDraw(None, None)
            }
        } else {
            Err(Error::from_hresult(E_FAIL))
        };
        if present
            .as_ref()
            .is_err_and(|e| e.code() == D2DERR_RECREATE_TARGET)
        {
            self.target = None;
            self.background = None;
        }
        present?;
        self.timings
            .show_ms
            .set(started.elapsed().as_secs_f64() * 1000.);
        let total = self.timings.start.elapsed().as_secs_f64() * 1000.;
        crate::logging::info(&format!("ui overlay backend={} copy={:.2}ms paint={:.2}ms show={:.2}ms first_present={:.2}ms persistent={} (since run_overlay; excludes capture)",self.backend.name(),self.timings.copy_ms.get(),self.timings.paint_ms.get(),self.timings.show_ms.get(),total,self.persistent));
        crate::logging::info(&format!(
            "ui overlay snapshot_targets={} initial_target={:?}",
            self.shot.targets.len(),
            self.shot
                .targets
                .iter()
                .find(|target| target.rect.contains(self.cursor))
                .map(|target| (target.kind, target.rect))
        ));
        if !Self::activate_foreground(hwnd) {
            self.foreground_retry = Some(Instant::now());
            unsafe {
                if SetTimer(Some(hwnd), FOREGROUND_TIMER, 50, None) == 0 {
                    self.foreground_retry = None;
                    crate::logging::warn(
                        "ui overlay foreground failed; could not start retry timer",
                    );
                }
            }
        }
        unsafe {
            SetTimer(Some(hwnd), 1, 16, None);
        }
        Ok(())
    }
    fn activate_foreground(hwnd: HWND) -> bool {
        unsafe {
            if GetForegroundWindow() != hwnd {
                let _ = SetForegroundWindow(hwnd);
            }
            let foreground = GetForegroundWindow();
            if foreground != hwnd {
                let other = GetWindowThreadProcessId(foreground, None);
                let current = GetCurrentThreadId();
                if other != 0 && other != current {
                    let attached = AttachThreadInput(current, other, true).as_bool();
                    let _ = SetForegroundWindow(hwnd);
                    if attached {
                        let _ = AttachThreadInput(current, other, false);
                    }
                }
            }
            if GetForegroundWindow() == hwnd {
                let _ = SetFocus(Some(hwnd));
                true
            } else {
                false
            }
        }
    }
    fn retry_foreground(&mut self, hwnd: HWND) {
        let Some(started) = self.foreground_retry else {
            return;
        };
        let expired = started.elapsed().as_millis() >= 500;
        let active = unsafe { GetForegroundWindow() == hwnd }
            || (!expired && Self::activate_foreground(hwnd));
        if active || expired {
            self.foreground_retry = None;
            unsafe {
                let _ = KillTimer(Some(hwnd), FOREGROUND_TIMER);
            }
            if active {
                crate::logging::info(&format!(
                    "ui overlay foreground acquired after {:.0}ms",
                    started.elapsed().as_secs_f64() * 1000.
                ));
            } else {
                crate::logging::warn(&format!(
                    "ui overlay foreground failed after 500ms; foreground={:?}",
                    unsafe { GetForegroundWindow() }
                ));
            }
        }
    }
    fn bounds(&self) -> Rect {
        Rect::new(
            self.shot.origin_x,
            self.shot.origin_y,
            self.shot.origin_x + self.shot.width as i32,
            self.shot.origin_y + self.shot.height as i32,
        )
    }
    fn origin(&self) -> Point {
        Point {
            x: self.shot.origin_x,
            y: self.shot.origin_y,
        }
    }
    fn local(&self, r: Rect) -> Rect {
        r.translate(-self.shot.origin_x, -self.shot.origin_y)
    }
    fn monitor(&self) -> (Rect, f32) {
        let p = self.area.map_or(self.cursor, |r| r.center());
        self.shot
            .monitors
            .iter()
            .find(|m| m.rect.contains(p))
            .or_else(|| self.shot.monitors.first())
            .map_or((self.bounds(), 1.), |m| (m.rect, m.dpi as f32 / 96.))
    }
    fn update_toolbar(&mut self) {
        let (work, scale) = self.monitor();
        self.toolbar = self
            .area
            .filter(|a| !a.is_empty())
            .map(|a| Toolbar::new(self.local(a), self.local(work), scale));
    }
    fn commit_text(&mut self) {
        if let Some(mut e) = self.editing.take() {
            if !e.edit.text.is_empty() {
                e.annotation.text = e.edit.text;
                self.history.push(e.annotation)
            }
        }
    }
    fn escape(&mut self, hwnd: HWND) {
        if self.editing.take().is_some() {
            return;
        }
        let draft = self.draft.take().is_some();
        let drag = self.drag.take().is_some();
        if draft || drag {
            unsafe {
                let _ = ReleaseCapture();
            }
            return;
        }
        self.finish(hwnd)
    }
    fn complete(&mut self, hwnd: HWND, action: Option<Action>) {
        self.commit_text();
        if let Some(area) = self.area.filter(|a| !a.is_empty()) {
            match export::render(&self.shot, area, &self.history.items) {
                Ok(image) => {
                    if action == Some(Action::SaveAs) {
                        match crate::output::save_as_dialog(&image, &mut self.cfg, hwnd) {
                            Ok(Some(_)) => {
                                *self.updated_config.borrow_mut() = Some(self.cfg.clone());
                            }
                            Ok(None) => return,
                            Err(e) => {
                                crate::notify::error(&format!("No se pudo guardar: {e}"));
                                return;
                            }
                        }
                    }
                    *self.result.borrow_mut() = Some(match action {
                        Some(action) => OverlayResult::Done {
                            image,
                            action,
                            area,
                        },
                        None => OverlayResult::Pin { image, area },
                    });
                    self.finish(hwnd)
                }
                Err(e) => crate::notify::error(&format!("No se pudo exportar: {e}")),
            }
        }
    }
    fn new_annotation(&self, p: Point) -> Annotation {
        Annotation {
            tool: self.tool,
            points: vec![p],
            color: self.color,
            thickness: self.thickness,
            fill: self.fill,
            text: String::new(),
            number: self
                .history
                .items
                .iter()
                .filter(|a| a.tool == Tool::Counter)
                .count()
                + 1,
        }
    }
    fn select_tool(&mut self, t: Tool) {
        if t == Tool::Marker && !self.marker_chosen {
            self.color = 0xffd60a;
            self.marker_chosen = true;
        }
        self.tool = t;
        self.selected = None;
    }
    fn button(&mut self, hwnd: HWND, b: Button) {
        self.commit_text();
        match b {
            Button::Tool(t) => self.select_tool(t),
            Button::Undo => self.history.undo(),
            Button::Redo => self.history.redo(),
            Button::Pin => self.complete(hwnd, None),
            Button::SaveAs => self.complete(hwnd, Some(Action::SaveAs)),
            Button::Copy => self.complete(hwnd, Some(Action::Copy)),
            Button::Close => self.finish(hwnd),
            Button::Color(c) => self.color = c,
            Button::Fill => self.fill = !self.fill,
            Button::Thickness(v) => self.thickness = v as f32,
            Button::CustomColor => {
                let mut custom = [COLORREF(0); 16];
                let mut cc = CHOOSECOLORW {
                    lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
                    hwndOwner: hwnd,
                    rgbResult: COLORREF(
                        ((self.color & 255) << 16)
                            | (self.color & 0xff00)
                            | ((self.color >> 16) & 255),
                    ),
                    lpCustColors: custom.as_mut_ptr(),
                    Flags: CC_FULLOPEN | CC_RGBINIT,
                    ..Default::default()
                };
                if unsafe { ChooseColorW(&mut cc) }.as_bool() {
                    let c = cc.rgbResult.0;
                    self.color = ((c & 255) << 16) | (c & 0xff00) | ((c >> 16) & 255)
                }
            }
        }
    }
    fn mouse_down(&mut self, hwnd: HWND, p: Point) {
        self.cursor = p;
        let local = Point {
            x: p.x - self.shot.origin_x,
            y: p.y - self.shot.origin_y,
        };
        if let Some(b) = self.toolbar.as_ref().and_then(|t| t.hit(local)) {
            self.pressed = Some(b);
            return;
        }

        if self
            .toolbar
            .as_ref()
            .is_some_and(|t| t.bounds.contains(local))
        {
            return;
        }
        self.commit_text();
        if let Some(a) = self.area {
            if self.tool != Tool::Move {
                if let Some(h) = a.hit_handle(p, (7. * self.monitor().1) as i32) {
                    self.drag = Some(Drag::Resize(a, h, p));
                    unsafe {
                        SetCapture(hwnd);
                    }
                    return;
                }
            }
            if self.tool == Tool::Move {
                if let Some(h) = a.hit_handle(p, (6. * self.monitor().1) as i32) {
                    self.drag = Some(Drag::Resize(a, h, p))
                } else if let Some(i) = self.hit_annotation(p) {
                    self.history.checkpoint();
                    self.selected = Some(i);
                    self.drag = Some(Drag::Annotation(i, p))
                } else if a.contains(p) {
                    self.selected = None;
                    self.drag = Some(Drag::MoveArea(a, p))
                } else {
                    self.area = None;
                    self.drag = Some(Drag::Select(p))
                }
            } else if self.tool == Tool::Text {
                self.editing = Some(Editing {
                    annotation: self.new_annotation(p),
                    edit: TextEdit::default(),
                })
            } else if self.tool == Tool::Counter {
                self.history.push(self.new_annotation(p))
            } else {
                self.draft = Some(self.new_annotation(p));
                self.drag = Some(Drag::Draw)
            }
        } else {
            self.drag = Some(Drag::Select(p))
        }
        if self.drag.is_some() {
            unsafe {
                SetCapture(hwnd);
            }
        }
    }
    fn mouse_move(&mut self, p: Point) {
        self.cursor = p;
        let p = Point {
            x: p.x.clamp(self.bounds().left, self.bounds().right),
            y: p.y.clamp(self.bounds().top, self.bounds().bottom),
        };
        match self.drag {
            Some(Drag::Select(a)) => {
                self.area = Some(Rect::from_points(a, p).intersect(self.bounds()))
            }
            Some(Drag::MoveArea(r, a)) => {
                self.area = Some(r.translate(p.x - a.x, p.y - a.y).clamp_to(self.bounds()))
            }
            Some(Drag::Resize(r, h, a)) => {
                self.area = Some(
                    geom::resize(
                        r,
                        h,
                        Point {
                            x: p.x - a.x,
                            y: p.y - a.y,
                        },
                    )
                    .intersect(self.bounds()),
                )
            }
            Some(Drag::Annotation(i, a)) => {
                if let Some(item) = self.history.items.get_mut(i) {
                    item.translate(p.x - a.x, p.y - a.y)
                }
                self.drag = Some(Drag::Annotation(i, p))
            }
            Some(Drag::Draw) => {
                if let Some(a) = &mut self.draft {
                    let start = a.points[0];
                    let mut end = p;
                    if key(VK_SHIFT) {
                        if matches!(a.tool, Tool::Line | Tool::Arrow) {
                            end = geom::snap_45(start, p)
                        } else if matches!(a.tool, Tool::Rectangle | Tool::Ellipse) {
                            let size = (p.x - start.x).abs().max((p.y - start.y).abs());
                            end = Point {
                                x: start.x + size * if p.x < start.x { -1 } else { 1 },
                                y: start.y + size * if p.y < start.y { -1 } else { 1 },
                            }
                        }
                    }
                    if matches!(a.tool, Tool::Pen | Tool::Marker) {
                        a.points.push(end)
                    } else {
                        a.points.truncate(1);
                        a.points.push(end)
                    }
                }
            }
            None => {
                let window = if self.area.is_none() {
                    snap_target_at(&self.shot.targets, p)
                } else {
                    None
                };
                if window != self.hover_window {
                    self.previous_window = self.hover_window;
                    self.hover_window = window;
                    self.window_changed = Instant::now();
                }
            }
        }
        let local = Point {
            x: p.x - self.shot.origin_x,
            y: p.y - self.shot.origin_y,
        };
        let hover = self.toolbar.as_ref().and_then(|t| t.hit(local));
        if hover != self.hover {
            self.previous_hover = self.hover;
            self.hover = hover;
            self.hover_since = Instant::now();
        }
    }
    fn mouse_up(&mut self, hwnd: HWND) {
        if let Some(button) = self.pressed.take() {
            if (button == Button::Undo && !self.history.can_undo())
                || (button == Button::Redo && !self.history.can_redo())
            {
                return;
            }
            let p = Point {
                x: self.cursor.x - self.shot.origin_x,
                y: self.cursor.y - self.shot.origin_y,
            };
            if self.toolbar.as_ref().and_then(|t| t.hit(p)) == Some(button) {
                self.button(hwnd, button)
            }
            return;
        }
        if let Some(Drag::Select(a)) = self.drag {
            if self.area.is_none_or(|r| r.width() < 3 && r.height() < 3) {
                self.area =
                    snap_target_at(&self.shot.targets, a).map(|r| r.intersect(self.bounds()))
            }
        }
        if let Some(a) = self.draft.take() {
            if !a.bounds().is_empty()
                || matches!(a.tool, Tool::Line | Tool::Arrow | Tool::Pen | Tool::Marker)
            {
                self.history.push(a)
            }
        }
        self.drag = None;
        unsafe {
            let _ = ReleaseCapture();
        }
        self.area = self.area.filter(|r| !r.is_empty());
        self.update_toolbar();
        self.shown = Instant::now();
    }
    fn keyboard(&mut self, hwnd: HWND, k: u32) {
        let ctrl = key(VK_CONTROL);
        let shift = key(VK_SHIFT);
        if k == VK_ESCAPE.0 as u32 {
            self.escape(hwnd);
            return;
        }
        if let Some(e) = &mut self.editing {
            match k {
                13 if ctrl => self.commit_text(),
                37 => e.edit.left(),
                39 => e.edit.right(),
                38 => e.edit.vertical(-1),
                40 => e.edit.vertical(1),
                36 => e.edit.caret = 0,
                35 => e.edit.caret = e.edit.text.len(),
                8 => e.edit.backspace(),
                46 => e.edit.delete(),
                _ => {}
            }
            return;
        }
        if ctrl {
            match k {
                65 => {
                    self.area = Some(self.bounds());
                    self.update_toolbar()
                }
                67 => self.complete(hwnd, Some(Action::Copy)),
                83 => self.complete(hwnd, Some(Action::SaveAs)),
                80 => self.complete(hwnd, None),
                90 => {
                    if shift {
                        self.history.redo()
                    } else {
                        self.history.undo()
                    }
                }
                89 => self.history.redo(),
                _ => {}
            }
        } else {
            match k {
                13 => self.complete(hwnd, Some(Action::Copy)),
                70 => {
                    self.area = Some(self.monitor_under_cursor());
                    self.update_toolbar()
                }
                67 => {
                    let hex = self.pixel_hex();
                    if let Err(e) = crate::clipboard::copy_text(&hex) {
                        crate::notify::error(&e.to_string());
                    }
                }
                46 => {
                    if let Some(i) = self.selected.take() {
                        if i < self.history.items.len() {
                            self.history.checkpoint();
                            self.history.items.remove(i);
                        }
                    }
                }
                _ => {
                    if let Some(t) = Tool::key(k) {
                        self.select_tool(t);
                    }
                }
            }
        }
        if matches!(k, 37..=40) {
            if let Some(r) = self.area {
                let step = if shift { 10 } else { 1 };
                let (dx, dy) = match k {
                    37 => (-step, 0),
                    38 => (0, -step),
                    39 => (step, 0),
                    _ => (0, step),
                };
                self.area = Some(if ctrl {
                    Rect::new(
                        r.left,
                        r.top,
                        (r.right + dx).max(r.left + 1),
                        (r.bottom + dy).max(r.top + 1),
                    )
                    .intersect(self.bounds())
                } else {
                    r.translate(dx, dy).clamp_to(self.bounds())
                });
                self.update_toolbar()
            }
        }
    }
    fn monitor_under_cursor(&self) -> Rect {
        self.shot
            .monitors
            .iter()
            .find(|m| m.rect.contains(self.cursor))
            .map_or(self.bounds(), |m| m.rect)
    }
    fn pixel_hex(&self) -> String {
        let x = (self.cursor.x - self.shot.origin_x)
            .clamp(0, self.shot.width.saturating_sub(1) as i32) as usize;
        let y = (self.cursor.y - self.shot.origin_y)
            .clamp(0, self.shot.height.saturating_sub(1) as i32) as usize;
        let i = (y * self.shot.width as usize + x) * 4;
        self.shot.bgra.get(i..i + 4).map_or_else(
            || "#000000".into(),
            |v| format!("#{:02X}{:02X}{:02X}", v[2], v[1], v[0]),
        )
    }
    fn paint_annotations(&self, c: &Canvas<'_>) -> Result<()> {
        let Some(area) = self.area else { return Ok(()) };
        unsafe {
            c.rt.PushAxisAlignedClip(&d2d::rect(self.local(area)), D2D1_ANTIALIAS_MODE_ALIASED);
        }
        let result = (|| -> Result<()> {
            annotate::render(c, &self.history.items, &self.shot, self.origin())?;
            if let Some(a) = &self.draft {
                annotate::draw(c, a, &self.shot, self.origin())?
            }
            if let Some(e) = &self.editing {
                let mut a = e.annotation.clone();
                a.text = e.edit.text.clone();
                annotate::draw(c, &a, &self.shot, self.origin())?;
                let position = e.edit.text[..e.edit.caret].encode_utf16().count() as u32;
                let (p, height) = c.caret(
                    &e.edit.text,
                    self.local(a.bounds()),
                    a.thickness * 6.,
                    position,
                )?;
                c.line(
                    p,
                    Point {
                        x: p.x,
                        y: p.y + height,
                    },
                    0xffffff,
                    1.,
                    1.,
                )?;
            }
            Ok(())
        })();
        // Always restore the clip before painting selection controls and other UI,
        // including when an annotation failed to render.
        unsafe {
            c.rt.PopAxisAlignedClip();
        }
        result
    }
    fn hit_annotation(&self, p: Point) -> Option<usize> {
        self.area
            .filter(|area| area.contains(p))
            .and_then(|_| self.history.hit(p))
    }
    fn paint(&mut self, hwnd: HWND) -> Result<()> {
        if self.target.is_none() || self.background.is_none() {
            self.upload(hwnd)?;
        }
        let rt = self
            .target
            .as_ref()
            .ok_or_else(|| Error::from_hresult(E_FAIL))?;
        let c = Canvas { rt, gfx: &self.gfx };
        unsafe {
            rt.BeginDraw();
        }
        let draw: Result<()> = (|| {
            let b = Rect::new(0, 0, self.shot.width as i32, self.shot.height as i32);
            if let Some(bg) = &self.background {
                c.image(bg, b, None, false);
            }
            if let Some(a) = self.area {
                let r = self.local(a);
                for outside in [
                    Rect::new(0, 0, b.right, r.top),
                    Rect::new(0, r.bottom, b.right, b.bottom),
                    Rect::new(0, r.top, r.left, r.bottom),
                    Rect::new(r.right, r.top, b.right, r.bottom),
                ] {
                    c.fill(outside, 0, 0.55)?
                }
            } else {
                c.fill(b, 0, 0.55)?
            }
            self.paint_annotations(&c)?;
            if let Some(i) = self.selected.and_then(|i| self.history.items.get(i)) {
                let r = self.local(i.bounds());
                for x in (r.left..r.right).step_by(8) {
                    c.line(
                        Point { x, y: r.top },
                        Point {
                            x: (x + 4).min(r.right),
                            y: r.top,
                        },
                        0xffffff,
                        0.9,
                        1.,
                    )?;
                    c.line(
                        Point { x, y: r.bottom },
                        Point {
                            x: (x + 4).min(r.right),
                            y: r.bottom,
                        },
                        0xffffff,
                        0.9,
                        1.,
                    )?
                }
                for y in (r.top..r.bottom).step_by(8) {
                    c.line(
                        Point { x: r.left, y },
                        Point {
                            x: r.left,
                            y: (y + 4).min(r.bottom),
                        },
                        0xffffff,
                        0.9,
                        1.,
                    )?;
                    c.line(
                        Point { x: r.right, y },
                        Point {
                            x: r.right,
                            y: (y + 4).min(r.bottom),
                        },
                        0xffffff,
                        0.9,
                        1.,
                    )?
                }
            }
            let (_, scale) = self.monitor();
            if let Some(a) = self.area {
                let r = self.local(a);
                c.rounded_border(r, 0., 0, 0.4, 3.5 * scale)?;
                c.border(r, theme::ACCENT, 1.5 * scale)?;
                let hovered = a.hit_handle(self.cursor, (7. * scale) as i32);
                for (i, p) in r.handles().into_iter().enumerate() {
                    let n = (if hovered == Some(i) { 5.5 } else { 4.5 } * scale).round() as i32;
                    let circle = Rect::new(p.x - n, p.y - n, p.x + n, p.y + n);
                    c.ellipse(
                        circle.translate(0, scale.round() as i32),
                        0,
                        0.28,
                        3. * scale,
                        true,
                    )?;
                    c.ellipse(circle, 0xffffff, 1., 1., true)?;
                    c.ellipse(circle, theme::ACCENT, 1., 1.5 * scale, false)?;
                }
                let label = format!("{} \u{00d7} {}", a.width(), a.height());
                let (width, height) = c.text_size(&label, 12. * scale, TextStyle::Semibold)?;
                let tag = geom::place_size_label(
                    r,
                    self.local(self.monitor().0),
                    width + (16. * scale) as i32,
                    height + (8. * scale) as i32,
                    (8. * scale) as i32,
                );
                c.panel(tag, scale, 1.)?;
                c.centered(&label, tag, 12. * scale, theme::TEXT, TextStyle::Semibold)?;
            } else {
                let progress =
                    d2d::ease(self.window_changed.elapsed().as_secs_f32() / theme::MOTION_SECONDS);
                for (window, alpha) in [
                    (self.previous_window, 1. - progress),
                    (self.hover_window, progress),
                ] {
                    if let Some(r) = window {
                        let r = self.local(r);
                        c.fill(r, theme::ACCENT, 0.08 * alpha)?;
                        c.rounded_border(r, scale, theme::ACCENT, alpha, 1.5 * scale)?;
                    }
                }
            }
            if self.drag.is_none() {
                if let Some(t) = &self.toolbar {
                    t.paint(
                        &c,
                        &ToolbarState {
                            tool: self.tool,
                            color: self.color,
                            thickness: self.thickness,
                            fill: self.fill,
                            hover: self.hover,
                            previous_hover: self.previous_hover,
                            hover_progress: d2d::ease(
                                self.hover_since.elapsed().as_secs_f32() / theme::MOTION_SECONDS,
                            ),
                            pressed: self.pressed,
                            can_undo: self.history.can_undo(),
                            can_redo: self.history.can_redo(),
                            opacity: (self.shown.elapsed().as_secs_f32() / 0.15).min(1.),
                        },
                    )?;
                    if self.hover_since.elapsed().as_millis() >= 400 {
                        if let Some(h) = self.hover {
                            t.tooltip(
                                &c,
                                h,
                                self.local(self.monitor().0),
                                d2d::ease(
                                    (self.hover_since.elapsed().as_secs_f32() - 0.4)
                                        / theme::MOTION_SECONDS,
                                ),
                            )?
                        }
                    }
                }
            }
            if self.cfg.show_magnifier && self.area.is_none() {
                let p = Point {
                    x: self.cursor.x - self.shot.origin_x,
                    y: self.cursor.y - self.shot.origin_y,
                };
                let px = |n: f32| (n * scale).round() as i32;
                let r = geom::place_callout(
                    p,
                    self.local(self.monitor_under_cursor()),
                    px(136.),
                    px(184.),
                    px(24.),
                );
                c.panel(r, scale, 1.)?;
                let zoom = Rect::new(
                    r.left + px(8.),
                    r.top + px(8.),
                    r.left + px(128.),
                    r.top + px(128.),
                );
                let mut sample = Vec::with_capacity(900);
                for y in -7..=7 {
                    for x in -7..=7 {
                        let x =
                            (p.x + x).clamp(0, self.shot.width.saturating_sub(1) as i32) as usize;
                        let y =
                            (p.y + y).clamp(0, self.shot.height.saturating_sub(1) as i32) as usize;
                        let i = (y * self.shot.width as usize + x) * 4;
                        if let Some(pixel) = self.shot.bgra.get(i..i + 4) {
                            sample.extend_from_slice(pixel)
                        }
                    }
                }
                let bitmap = c.bitmap(15, 15, &sample)?;
                c.rounded_image(&bitmap, zoom, None, 12. * scale, true)?;
                let mid = zoom.center();
                c.line(
                    Point {
                        x: mid.x,
                        y: zoom.top,
                    },
                    Point {
                        x: mid.x,
                        y: zoom.bottom,
                    },
                    0xffffff,
                    0.2,
                    scale,
                )?;
                c.line(
                    Point {
                        x: zoom.left,
                        y: mid.y,
                    },
                    Point {
                        x: zoom.right,
                        y: mid.y,
                    },
                    0xffffff,
                    0.2,
                    scale,
                )?;
                let cell = Rect::new(
                    mid.x - px(4.),
                    mid.y - px(4.),
                    mid.x + px(4.),
                    mid.y + px(4.),
                );
                c.border(cell, 0, 3. * scale)?;
                c.border(cell, 0xffffff, scale)?;
                c.label(
                    &format!("{}, {}", self.cursor.x, self.cursor.y),
                    Rect::new(
                        r.left + px(10.),
                        zoom.bottom + px(4.),
                        r.right - px(8.),
                        zoom.bottom + px(23.),
                    ),
                    10. * scale,
                    theme::MUTED,
                    TextStyle::Mono,
                )?;
                let hex = self.pixel_hex();
                let color = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
                let y = zoom.bottom + px(29.);
                c.rounded(
                    Rect::new(r.left + px(10.), y, r.left + px(23.), y + px(13.)),
                    3. * scale,
                    color,
                    1.,
                )?;
                c.rounded_border(
                    Rect::new(r.left + px(10.), y, r.left + px(23.), y + px(13.)),
                    3. * scale,
                    0xffffff,
                    0.3,
                    scale,
                )?;
                c.label(
                    &hex,
                    Rect::new(r.left + px(31.), y - px(3.), r.right - px(8.), y + px(17.)),
                    11. * scale,
                    theme::TEXT,
                    TextStyle::Mono,
                )?;
            }

            Ok(())
        })();
        let end = unsafe { rt.EndDraw(None, None) };
        if end
            .as_ref()
            .is_err_and(|e| e.code() == D2DERR_RECREATE_TARGET)
        {
            self.target = None;
            self.background = None;
            self.ready = false;
            d2d::invalidate(hwnd);
            return end;
        }
        draw?;
        end?;
        Ok(())
    }
}
// Snapshot order is authoritative: the first containing target is the visible surface.
fn snap_target_at(targets: &[capture::SnapTarget], point: Point) -> Option<Rect> {
    targets
        .iter()
        .find(|target| target.rect.contains(point))
        .map(|target| target.rect)
}
fn key(k: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(k.0 as i32) < 0 }
}
impl NativeWindow for Overlay {
    fn message(&mut self, hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Option<LRESULT> {
        // Activation retries must also finish on time while a drawing target is being recreated.
        if msg == WM_TIMER && w.0 == FOREGROUND_TIMER {
            if self.session.get() {
                self.retry_foreground(hwnd);
            }
            return Some(LRESULT(0));
        }
        if (!self.session.get() || !self.ready)
            && matches!(
                msg,
                WM_MOUSEMOVE
                    | WM_LBUTTONDOWN
                    | WM_LBUTTONUP
                    | WM_LBUTTONDBLCLK
                    | WM_RBUTTONUP
                    | WM_KEYDOWN
                    | WM_CHAR
                    | WM_MOUSEWHEEL
                    | WM_TIMER
            )
        {
            return Some(LRESULT(0));
        }
        match msg {
            REFRESH_SURFACE => {
                if self.persistent {
                    warm_up_surface();
                }
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                unsafe {
                    let _ = BeginPaint(hwnd, &mut ps);
                }
                let result = if self.session.get() {
                    self.paint(hwnd)
                } else {
                    Ok(())
                };
                unsafe {
                    let _ = EndPaint(hwnd, &ps);
                }
                match result {
                    Ok(()) => {
                        if self.session.get() {
                            self.ready = true;
                        }
                    }
                    Err(e) if e.code() == D2DERR_RECREATE_TARGET => d2d::invalidate(hwnd),
                    Err(e) => {
                        crate::notify::error(&format!("Error de dibujo: {e}"));
                        self.finish(hwnd)
                    }
                }
            }
            WM_ERASEBKGND => return Some(LRESULT(1)),
            WM_LBUTTONDOWN => {
                let p = d2d::local_point(l);
                self.mouse_down(
                    hwnd,
                    Point {
                        x: p.x + self.shot.origin_x,
                        y: p.y + self.shot.origin_y,
                    },
                );
                d2d::invalidate(hwnd)
            }
            WM_MOUSEMOVE => {
                let p = d2d::local_point(l);
                self.mouse_move(Point {
                    x: p.x + self.shot.origin_x,
                    y: p.y + self.shot.origin_y,
                });
                d2d::invalidate(hwnd)
            }
            WM_LBUTTONUP => {
                self.mouse_up(hwnd);
                d2d::invalidate(hwnd)
            }
            WM_CAPTURECHANGED => {
                self.drag = None;
                d2d::invalidate(hwnd)
            }
            WM_RBUTTONUP => {
                self.escape(hwnd);
                d2d::invalidate(hwnd)
            }
            WM_LBUTTONDBLCLK => {
                let p = d2d::local_point(l);
                let p = Point {
                    x: p.x + self.shot.origin_x,
                    y: p.y + self.shot.origin_y,
                };
                if self.area.is_some_and(|a| a.contains(p)) {
                    self.complete(hwnd, Some(Action::Copy))
                } else {
                    self.cursor = p;
                    self.area = Some(self.monitor_under_cursor());
                    self.update_toolbar()
                }
                d2d::invalidate(hwnd)
            }
            WM_KEYDOWN => {
                self.keyboard(hwnd, w.0 as u32);
                d2d::invalidate(hwnd)
            }
            WM_CHAR => {
                if let Some(e) = &mut self.editing {
                    let unit = w.0 as u16;
                    if unit == 13 {
                        if !key(VK_CONTROL) {
                            e.edit.input(10)
                        }
                    } else if unit >= 32 {
                        e.edit.input(unit)
                    }
                    d2d::invalidate(hwnd)
                }
            }
            WM_MOUSEWHEEL => {
                let delta = ((w.0 >> 16) as u16 as i16) as f32 / 120.;
                self.thickness = (self.thickness + delta).clamp(1., 32.);
                d2d::invalidate(hwnd)
            }
            WM_TIMER => {
                if self.shown.elapsed().as_millis() < 200
                    || self.hover_since.elapsed().as_secs_f32() < theme::MOTION_SECONDS
                    || self.window_changed.elapsed().as_secs_f32() < theme::MOTION_SECONDS
                    || (400..540).contains(&self.hover_since.elapsed().as_millis())
                {
                    d2d::invalidate(hwnd)
                }
            }
            WM_SETCURSOR => {
                let local = Point {
                    x: self.cursor.x - self.shot.origin_x,
                    y: self.cursor.y - self.shot.origin_y,
                };
                let cursor = if self
                    .toolbar
                    .as_ref()
                    .is_some_and(|t| t.bounds.contains(local))
                {
                    IDC_ARROW
                } else if let Some(h) = self
                    .area
                    .and_then(|a| a.hit_handle(self.cursor, (7. * self.monitor().1) as i32))
                {
                    match h {
                        0 | 4 => IDC_SIZENWSE,
                        2 | 6 => IDC_SIZENESW,
                        1 | 5 => IDC_SIZENS,
                        _ => IDC_SIZEWE,
                    }
                } else if self.tool == Tool::Move
                    && self.area.is_some_and(|a| a.contains(self.cursor))
                {
                    IDC_SIZEALL
                } else if self.tool == Tool::Text && self.area.is_some() {
                    IDC_IBEAM
                } else {
                    IDC_CROSS
                };
                unsafe {
                    if let Ok(cursor) = LoadCursorW(None, cursor) {
                        SetCursor(Some(cursor));
                    }
                }
                return Some(LRESULT(1));
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                self.stale = true;
                self.finish(hwnd);
                if self.persistent {
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), REFRESH_SURFACE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            WM_CLOSE => self.finish(hwnd),
            WM_NCDESTROY => {
                self.session.set(false);
                self.target = None;
                self.background = None;
                return None;
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }
}
pub fn run_overlay(shot: Screenshot, cfg: &mut Config) -> OverlayResult {
    run(shot, cfg, false)
}
fn run(shot: Screenshot, cfg: &mut Config, preselected: bool) -> OverlayResult {
    let timings = Rc::new(OverlayTimings {
        start: Instant::now(),
        copy_ms: Cell::new(0.),
        paint_ms: Cell::new(0.),
        show_ms: Cell::new(0.),
    });
    let result = (|| -> Result<OverlayResult> {
        if shot.width == 0 || shot.height == 0 {
            return Ok(OverlayResult::Cancelled);
        }
        let key = SurfaceKey::shot(&shot);
        let hwnd = if preselected {
            create_surface(&key, false)?
        } else {
            ensure_surface(&key)?
        };
        if !d2d::with_window::<Overlay, _>(hwnd, |state| !state.session.get()).unwrap_or(false) {
            return Ok(OverlayResult::Cancelled);
        }
        let (session, outcome, updated_config) =
            d2d::with_window::<Overlay, _>(hwnd, |state| -> Result<_> {
                state.reset(shot, cfg, preselected, timings);
                state.prepare(hwnd)?;
                state.show(hwnd)?;
                Ok((
                    state.session.clone(),
                    state.result.clone(),
                    state.updated_config.clone(),
                ))
            })
            .ok_or_else(|| Error::from_hresult(E_FAIL))?
            .inspect_err(|_| {
                let _ = d2d::with_window::<Overlay, _>(hwnd, |state| state.finish(hwnd));
            })?;
        unsafe {
            let mut msg = MSG::default();
            while session.get() && IsWindow(Some(hwnd)).as_bool() {
                let code = GetMessageW(&mut msg, None, 0, 0).0;
                if code <= 0 {
                    let _ = d2d::with_window::<Overlay, _>(hwnd, |state| state.finish(hwnd));
                    if code == 0 {
                        PostQuitMessage(msg.wParam.0 as i32);
                    }
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some(updated) = updated_config.borrow_mut().take() {
            *cfg = updated;
        }
        let value = outcome
            .borrow_mut()
            .take()
            .unwrap_or(OverlayResult::Cancelled);
        Ok(value)
    })();
    match result {
        Ok(value) => value,
        Err(e) => {
            crate::notify::error(&format!("No se pudo abrir la captura: {e}"));
            OverlayResult::Cancelled
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Default,
    Software,
}
impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::Default => "DEFAULT",
            Self::Software => "SOFTWARE",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct SurfaceKey {
    area: Rect,
    monitors: Vec<(Rect, u32)>,
}
impl SurfaceKey {
    fn shot(shot: &Screenshot) -> Self {
        let mut key = Self {
            area: Rect::new(
                shot.origin_x,
                shot.origin_y,
                shot.origin_x + shot.width as i32,
                shot.origin_y + shot.height as i32,
            ),
            monitors: shot.monitors.iter().map(|m| (m.rect, m.dpi)).collect(),
        };
        key.monitors
            .sort_by_key(|(r, dpi)| (r.left, r.top, r.right, r.bottom, *dpi));
        key
    }
    fn desktop() -> Self {
        let mut key = Self {
            area: capture::virtual_screen_rect(),
            monitors: capture::monitors()
                .iter()
                .map(|m| (m.rect, m.dpi))
                .collect(),
        };
        key.monitors
            .sort_by_key(|(r, dpi)| (r.left, r.top, r.right, r.bottom, *dpi));
        key
    }
    fn blank(&self) -> Screenshot {
        Screenshot {
            targets: Vec::new(),
            origin_x: self.area.left,
            origin_y: self.area.top,
            width: self.area.width() as u32,
            height: self.area.height() as u32,
            bgra: Vec::new(),
            monitors: self
                .monitors
                .iter()
                .enumerate()
                .map(|(i, (rect, dpi))| capture::MonitorInfo {
                    rect: *rect,
                    work: *rect,
                    dpi: *dpi,
                    primary: i == 0,
                })
                .collect(),
        }
    }
}
#[derive(Clone)]
struct CachedSurface {
    hwnd: HWND,
    key: SurfaceKey,
    session: Rc<Cell<bool>>,
}
thread_local! {static CACHED_SURFACE:RefCell<Option<CachedSurface>>=const{RefCell::new(None)};}
struct Candidate {
    backend: Backend,
    target: ID2D1HwndRenderTarget,
    bitmap: ID2D1Bitmap,
    cold_ms: f64,
    warm_ms: f64,
}
fn probe(
    hwnd: HWND,
    key: &SurfaceKey,
    gfx: &Graphics,
    backend: Backend,
    blank: &[u8],
) -> Result<Candidate> {
    let started = Instant::now();
    let width = key.area.width() as u32;
    let height = key.area.height() as u32;
    let target = gfx.overlay_target(hwnd, width, height, backend == Backend::Software)?;
    let c = Canvas { rt: &target, gfx };
    let bitmap = c.reserve_bitmap(width, height)?;
    let mut samples = Vec::with_capacity(3);
    let mut cold_ms = 0.;
    for frame in 0..4 {
        let lap = Instant::now();
        unsafe {
            bitmap.CopyFromMemory(None, blank.as_ptr().cast(), width * 4)?;
            target.BeginDraw();
            target.Clear(Some(&d2d::color(0, 1.)));
        }
        c.image(
            &bitmap,
            Rect::new(0, 0, width as i32, height as i32),
            None,
            false,
        );
        let drawing = c.fill(Rect::new(0, 0, width as i32, height as i32), 0, 0.55);
        let end = unsafe { target.EndDraw(None, None) };
        drawing?;
        end?;
        if frame == 0 {
            cold_ms = started.elapsed().as_secs_f64() * 1000.;
        } else {
            samples.push(lap.elapsed().as_secs_f64() * 1000.);
        }
    }
    samples.sort_by(f64::total_cmp);
    let warm_ms = samples[1];
    crate::logging::info(&format!("ui overlay hidden_probe backend={} cold_prepare={cold_ms:.2}ms warm_copy_paint={warm_ms:.2}ms",backend.name()));
    Ok(Candidate {
        backend,
        target,
        bitmap,
        cold_ms,
        warm_ms,
    })
}
fn software_is_faster(default: (f64, f64), software: (f64, f64)) -> bool {
    software.0.max(software.1) <= default.0.max(default.1)
}
fn create_surface(key: &SurfaceKey, persistent: bool) -> Result<HWND> {
    if key.area.is_empty() {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    let started = Instant::now();
    let gfx = Graphics::new()?;
    let state = Overlay::model(
        key.blank(),
        &Config::default(),
        gfx.clone(),
        key.clone(),
        persistent,
        Backend::Software,
    );
    let hwnd = d2d::window(
        "Destello · Capturar",
        key.area,
        WS_POPUP,
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        Box::new(state),
    )?;
    let prepared = (|| -> Result<Candidate> {
        let bytes = (key.area.width() as usize)
            .checked_mul(key.area.height() as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| Error::from_hresult(E_OUTOFMEMORY))?;
        let mut blank = Vec::new();
        blank
            .try_reserve_exact(bytes)
            .map_err(|_| Error::from_hresult(E_OUTOFMEMORY))?;
        blank.resize(bytes, 0u8);
        if !persistent {
            return probe(hwnd, key, &gfx, Backend::Software, &blank);
        }
        // Only one HWND render target is alive during each probe; a discarded candidate cannot
        // detach or overwrite the winning target's retained presentation surface.
        let default = probe(hwnd, key, &gfx, Backend::Default, &blank)
            .map(|candidate| (candidate.cold_ms, candidate.warm_ms));
        let software = probe(hwnd, key, &gfx, Backend::Software, &blank);
        match (default, software) {
            (Ok(default), Ok(software)) => {
                if software_is_faster(default, (software.cold_ms, software.warm_ms)) {
                    Ok(software)
                } else {
                    drop(software);
                    probe(hwnd, key, &gfx, Backend::Default, &blank)
                }
            }
            (Ok(_), Err(e)) => {
                crate::logging::warn(&format!("ui software probe: {e}"));
                probe(hwnd, key, &gfx, Backend::Default, &blank)
            }
            (Err(e), Ok(software)) => {
                crate::logging::warn(&format!("ui default probe: {e}"));
                Ok(software)
            }
            (Err(_), Err(e)) => Err(e),
        }
    })();
    match prepared {
        Ok(candidate) => {
            let backend = candidate.backend;
            let installed = d2d::with_window::<Overlay, _>(hwnd, |state| {
                state.backend = backend;
                state.target = Some(candidate.target);
                state.background = Some(candidate.bitmap);
            });
            if installed.is_none() {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(Error::from_hresult(E_FAIL));
            }
            crate::logging::info(&format!("ui overlay hidden_surface backend={} persistent={persistent} size={}x{} prepare={:.2}ms",backend.name(),key.area.width(),key.area.height(),started.elapsed().as_secs_f64()*1000.));
            Ok(hwnd)
        }
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            Err(e)
        }
    }
}
fn ensure_surface(key: &SurfaceKey) -> Result<HWND> {
    let cached = CACHED_SURFACE.with(|surface| surface.borrow().clone());
    if let Some(cached) = cached {
        if cached.session.get() && unsafe { IsWindow(Some(cached.hwnd)).as_bool() } {
            return Ok(cached.hwnd);
        }
        if cached.key == *key
            && unsafe { IsWindow(Some(cached.hwnd)).as_bool() }
            && d2d::with_window::<Overlay, _>(cached.hwnd, |state| !state.stale).unwrap_or(false)
        {
            return Ok(cached.hwnd);
        }
        CACHED_SURFACE.with(|s| s.borrow_mut().take());
        unsafe {
            if IsWindow(Some(cached.hwnd)).as_bool() {
                let _ = DestroyWindow(cached.hwnd);
            }
        }
    }
    let hwnd = create_surface(key, true)?;
    let session = d2d::with_window::<Overlay, _>(hwnd, |state| state.session.clone())
        .ok_or_else(|| Error::from_hresult(E_FAIL))?;
    CACHED_SURFACE.with(|s| {
        *s.borrow_mut() = Some(CachedSurface {
            hwnd,
            key: key.clone(),
            session,
        })
    });
    Ok(hwnd)
}
/// Runtime-only startup path: creates and primes a hidden HWND, never calls ShowWindow.
pub(super) fn warm_up_surface() {
    let started = Instant::now();
    match ensure_surface(&SurfaceKey::desktop()) {
        Ok(_) => crate::logging::info(&format!(
            "ui overlay persistent warm_up={:.2}ms",
            started.elapsed().as_secs_f64() * 1000.
        )),
        Err(e) => crate::logging::warn(&format!("ui overlay persistent warm_up: {e}")),
    }
}
#[cfg(test)]
mod clipping_tests {
    use super::*;
    use windows::Win32::{Graphics::Imaging::*, System::Com::*};

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
    fn model() -> Overlay {
        let mut pixels = Vec::new();
        for y in 0..48u8 {
            for x in 0..64u8 {
                pixels.extend_from_slice(&[x * 3, y * 5, 80, 255]);
            }
        }
        let shot = Screenshot {
            targets: Vec::new(),
            origin_x: -32,
            origin_y: -24,
            width: 64,
            height: 48,
            bgra: pixels,
            monitors: Vec::new(),
        };
        let key = SurfaceKey::shot(&shot);
        Overlay::model(
            shot,
            &Config::default(),
            Graphics::new().expect("graphics"),
            key,
            true,
            Backend::Software,
        )
    }
    fn annotation(tool: Tool) -> Annotation {
        Annotation {
            tool,
            points: vec![Point { x: -20, y: -10 }, Point { x: 25, y: 15 }],
            color: 0xe5484d,
            thickness: 2.,
            fill: true,
            text: "Texto fuera de la selección".into(),
            number: 1,
        }
    }
    // Invoke the exact annotation pass used by the overlay on an in-memory target.
    fn screen(model: &Overlay) -> Vec<u8> {
        unsafe {
            let bitmap = model
                .gfx
                .wic
                .resolve()
                .expect("WIC")
                .CreateBitmap(64, 48, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)
                .expect("surface");
            let rt = model
                .gfx
                .factory
                .CreateWicBitmapRenderTarget(&bitmap, &Graphics::properties())
                .expect("RT");
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let c = Canvas {
                rt: &rt,
                gfx: &model.gfx,
            };
            let bg = c.bitmap(64, 48, &model.shot.bgra).expect("background");
            rt.BeginDraw();
            c.image(&bg, Rect::new(0, 0, 64, 48), None, false);
            model.paint_annotations(&c).expect("screen annotations");
            // UI drawn after the annotation pass must not inherit its clip.
            c.fill(Rect::new(0, 0, 1, 1), 0x00ff00, 1.)
                .expect("UI outside selection");
            rt.EndDraw(None, None).expect("paint");
            let mut pixels = vec![0; 64 * 48 * 4];
            bitmap
                .CopyPixels(std::ptr::null(), 64 * 4, &mut pixels)
                .expect("read");
            assert_eq!(&pixels[..4], &[0, 255, 0, 255], "clip leaked into UI pass");
            pixels
        }
    }
    #[test]
    fn ac3_screen_and_export_clip_every_tool_after_selection_moves_and_resizes() {
        let _com = Com::init();
        let mut model = model();
        for tool in Tool::ALL.into_iter().filter(|tool| *tool != Tool::Move) {
            let item = annotation(tool);
            for draft in [false, true] {
                model.history.items = if draft {
                    Vec::new()
                } else {
                    vec![item.clone()]
                };
                model.draft = draft.then(|| item.clone());
                for area in [
                    Rect::new(-16, -12, 16, 12),
                    Rect::new(-8, -6, 24, 18),
                    Rect::new(0, -4, 16, 8),
                ] {
                    model.area = Some(area);
                    let pixels = screen(&model);
                    let exported = export::render(&model.shot, area, std::slice::from_ref(&item))
                        .expect("export");
                    let mut changed = false;
                    for y in 0..48usize {
                        for x in 0..64usize {
                            let point = Point {
                                x: x as i32 - 32,
                                y: y as i32 - 24,
                            };
                            let offset = (y * 64 + x) * 4;
                            let pixel = &pixels[offset..offset + 4];
                            if area.contains(point) {
                                let i = ((point.y - area.top) as usize * area.width() as usize
                                    + (point.x - area.left) as usize)
                                    * 4;
                                let rgba = &exported.data[i..i + 4];
                                assert_eq!(
                                    pixel,
                                    &[rgba[2], rgba[1], rgba[0], rgba[3]],
                                    "{tool:?}, draft={draft}, {point:?}"
                                );
                                changed |= pixel != &model.shot.bgra[offset..offset + 4];
                            } else if offset != 0 {
                                assert_eq!(
                                    pixel,
                                    &model.shot.bgra[offset..offset + 4],
                                    "{tool:?} escaped selection at {point:?}, draft={draft}"
                                );
                            }
                        }
                    }
                    // Some moved crops legitimately contain no part of an annotation.
                    if area == Rect::new(-16, -12, 16, 12) {
                        assert!(changed, "fixture must draw {tool:?}");
                    }
                    if draft {
                        assert_eq!(
                            model.draft.as_ref(),
                            Some(&item),
                            "selection must not move draft"
                        );
                    } else {
                        assert_eq!(
                            model.history.items.as_slice(),
                            std::slice::from_ref(&item),
                            "selection must not move annotations"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn editing_text_is_clipped_and_hidden_annotations_cannot_be_selected() {
        let _com = Com::init();
        let mut model = model();
        let area = Rect::new(-16, -12, 16, 12);
        model.area = Some(area);
        let text = annotation(Tool::Text);
        let mut edit = TextEdit::default();
        for unit in text.text.encode_utf16() {
            edit.input(unit);
        }
        model.editing = Some(Editing {
            annotation: text,
            edit,
        });
        let pixels = screen(&model);
        for y in 0..48usize {
            for x in 0..64usize {
                let offset = (y * 64 + x) * 4;
                if offset != 0
                    && !area.contains(Point {
                        x: x as i32 - 32,
                        y: y as i32 - 24,
                    })
                {
                    assert_eq!(
                        &pixels[offset..offset + 4],
                        &model.shot.bgra[offset..offset + 4]
                    );
                }
            }
        }
        model.history.items.push(annotation(Tool::Rectangle));
        assert_eq!(model.hit_annotation(Point { x: -18, y: 0 }), None);
        assert_eq!(model.hit_annotation(Point { x: -8, y: 0 }), Some(0));
        model.area = Some(Rect::new(0, -4, 16, 8));
        assert_eq!(model.hit_annotation(Point { x: -8, y: 0 }), None);
        assert_eq!(model.hit_annotation(Point { x: 8, y: 0 }), Some(0));
        model.area = None;
        assert_eq!(model.hit_annotation(Point { x: 8, y: 0 }), None);
    }
    #[test]
    fn ac4_snapshot_targets_control_hover_and_click_in_z_order() {
        use capture::{SnapKind, SnapTarget};
        let _com = Com::init();
        let mut model = model();
        let window = model.bounds();
        let popup = Rect::new(-16, -12, 16, 12);
        let submenu = Rect::new(-8, -6, 8, 6);
        for kind in [SnapKind::Menu, SnapKind::ShellFlyout, SnapKind::Taskbar] {
            model.shot.targets = vec![
                SnapTarget {
                    rect: submenu,
                    kind: SnapKind::Menu,
                },
                SnapTarget { rect: popup, kind },
                SnapTarget {
                    rect: window,
                    kind: SnapKind::Window,
                },
            ];
            for (point, expected) in [
                (Point { x: 0, y: 0 }, submenu),
                (Point { x: 12, y: 0 }, popup),
                (Point { x: 16, y: 0 }, window),
                (Point { x: 24, y: 0 }, window),
            ] {
                model.area = None;
                model.mouse_move(point);
                assert_eq!(
                    model.hover_window,
                    Some(expected),
                    "hover {kind:?} at {point:?}"
                );
                model.drag = Some(Drag::Select(point));
                model.mouse_up(HWND::default());
                assert_eq!(model.area, Some(expected), "click {kind:?} at {point:?}");
            }
            // Honor captured Z order even when an ordinary window is above a popup.
            model.shot.targets.swap(0, 2);
            model.area = None;
            model.mouse_move(Point { x: 0, y: 0 });
            assert_eq!(model.hover_window, Some(window));
        }
        model.shot.targets.clear();
        model.area = None;
        model.mouse_move(Point { x: 0, y: 0 });
        assert!(model.hover_window.is_none());
        model.drag = Some(Drag::Select(model.cursor));
        model.mouse_up(HWND::default());
        assert!(model.area.is_none());
    }
}

#[cfg(test)]
mod persistent_tests {
    use super::*;
    #[test]
    fn cold_gpu_penalty_selects_software() {
        assert!(software_is_faster((2300., 2.), (12., 7.)));
        assert!(!software_is_faster((3., 1.), (12., 7.)));
    }
    #[test]
    fn topology_key_changes_only_for_surface_geometry_and_dpi() {
        let shot = Screenshot {
            targets: Vec::new(),
            origin_x: -1920,
            origin_y: 0,
            width: 3840,
            height: 1080,
            bgra: Vec::new(),
            monitors: vec![capture::MonitorInfo {
                rect: Rect::new(-1920, 0, 0, 1080),
                work: Rect::new(-1920, 0, 0, 1032),
                dpi: 96,
                primary: false,
            }],
        };
        let key = SurfaceKey::shot(&shot);
        let mut next = shot.clone();
        next.bgra = vec![1, 2, 3, 255];
        assert_eq!(key, SurfaceKey::shot(&next));
        next.targets.push(capture::SnapTarget {
            rect: Rect::new(-100, 0, 0, 100),
            kind: capture::SnapKind::Menu,
        });
        assert_eq!(key, SurfaceKey::shot(&next));
        next.monitors[0].work.bottom = 1000;
        assert_eq!(key, SurfaceKey::shot(&next));
        next.monitors[0].dpi = 144;
        assert_ne!(key, SurfaceKey::shot(&next));
        next.monitors[0].dpi = 96;
        next.origin_x = -1600;
        assert_ne!(key, SurfaceKey::shot(&next));
    }
    #[test]
    fn session_reset_discards_edits_and_ignores_input_until_prepared() {
        use windows::Win32::System::Com::*;
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .expect("COM");
        }
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _com = Com;
        let shot = Screenshot {
            targets: Vec::new(),
            origin_x: 0,
            origin_y: 0,
            width: 2,
            height: 2,
            bgra: [0, 0, 0, 255].repeat(4),
            monitors: Vec::new(),
        };
        let cfg = Config::default();
        let key = SurfaceKey::shot(&shot);
        let mut model = Overlay::model(
            shot.clone(),
            &cfg,
            Graphics::new().expect("graphics"),
            key,
            true,
            Backend::Software,
        );
        let identity = model.session.clone();
        model.area = Some(Rect::new(0, 0, 1, 1));
        model.ready = true;
        model.session.set(true);
        model.marker_chosen = true;
        model.foreground_retry = Some(Instant::now());
        let timings = model.timings.clone();
        model.reset(shot, &cfg, false, timings);
        assert!(Rc::ptr_eq(&identity, &model.session));
        assert!(!model.session.get());
        assert!(!model.ready);
        assert!(model.area.is_none());
        assert!(!model.marker_chosen);
        assert!(model.foreground_retry.is_none());
        model.cursor = Point { x: 1, y: 1 };
        let invalid = HWND(std::ptr::dangling_mut::<core::ffi::c_void>());
        let motion = LPARAM(200 | (200 << 16));
        model.message(invalid, WM_MOUSEMOVE, WPARAM(0), motion);
        assert_eq!(model.cursor, Point { x: 1, y: 1 });
        model.session.set(true);
        model.message(invalid, WM_MOUSEMOVE, WPARAM(0), motion);
        assert_eq!(model.cursor, Point { x: 1, y: 1 });
        assert!(model.drag.is_none());
    }
}
